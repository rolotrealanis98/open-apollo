"""Native logical inserts backed by explicit, transactionally swapped DSP chains.

The injected private composer owns effect recipes. This adapter owns real physical
instances and maps each stable logical identity to its current instance and rows.
"""
import copy

from dataclasses import dataclass, field
import logging
import re

from plugin_driver import START_REFUSALS, SWAP_REFUSALS
from plugin_native import NativePluginBackend, _Lease
from plugin_delay import InputLatency, plan_compensation
from native_carrier import dsp_input
from plugin_publication import LoadedPlugin, OwnedHandle
from plugin_transactions import (
    ActivateOutcome, FinalizeOutcome, Outcome, PreparedPlugin, PrepareOutcome,
    RetireOutcome, RollbackOutcome,
)

log = logging.getLogger(__name__)


class _UnconfirmedDeviceOperation(RuntimeError):
    pass


@dataclass(frozen=True)
class CompensationOutcome:
    status: Outcome
    setting: str
    inputs: tuple = ()
    detail: str = ''


@dataclass
class _Chain:
    channels: tuple
    native: object
    leases: dict
    sources: dict
    composed: object = None


@dataclass
class _Change:
    slot: str
    previous: _Chain | None
    candidate: _Chain
    activated: bool = False


@dataclass
class _BatchChange:
    slot: str
    channels: tuple
    previous: dict
    recipes: dict
    inputs: tuple
    candidates: dict = field(default_factory=dict)
    swapped: list = field(default_factory=list)
    strips_before: object = None
    strips_after: object = None
    activated: bool = False


class NativeChainBackend(NativePluginBackend):
    """One physical chain per input layout, with up to four logical inserts."""

    # Legacy native-mode discovery marker; chain_supported selects chain behavior.
    native_single_insert = True
    chain_supported = True

    def __init__(self, publisher, driver, provider, *, composer, max_inserts=4,
                 record_delay_samples=200, compensation_supported=False,
                 compensation_setting='Medium', start_staged=False, **kwargs):
        super().__init__(publisher, driver, provider, **kwargs)
        # The driver can route a staged chain with no anchor to swap from (UA_PLUGIN_INST_START).
        # Without it a chain whose startup is over one command page cannot go live on an
        # empty input, and such a load is refused instead of showing a silent plug-in.
        self.start_staged = bool(start_staged)
        if not callable(composer) or type(max_inserts) is not int or not 1 <= max_inserts <= 4:
            raise ValueError('explicit composer and bounded insert count required')
        if type(record_delay_samples) is not int or not 0 <= record_delay_samples < 2048:
            raise ValueError('bounded explicit carrier delay required')
        self.composer, self.max_inserts = composer, max_inserts
        self.record_delay_samples = record_delay_samples
        self._chains = {}
        self._serial = 0
        if type(compensation_supported) is not bool:
            raise ValueError('explicit compensation capability required')
        plan_compensation(compensation_setting, ())
        self.compensation_supported = compensation_supported
        self.compensation_setting = compensation_setting
        self.compensation_inputs = ()
        self._compensating = False
        self._compensation_transition = None

    @staticmethod
    def _compensation_io(operation, *args, **kwargs):
        try:
            return operation(*args, **kwargs)
        except Exception as error:
            raise _UnconfirmedDeviceOperation('device operation unconfirmed') from error

    @staticmethod
    def _live_observations(composed):
        """What to read back once a chain runs. Its whole startup was verified while it was
        inactive (just created or staged); once the DSP runs it, plug-ins own some of those words
        (delay pointers, LFO phases), so only the chain's own header words are checked: row 0 and
        each effect's count and tap words, and the Mic strip's tap word (UAD MON)."""
        effects = getattr(composed, 'effects', None)
        if not effects:
            return composed.observations
        heads = {getattr(effect, 'base_row', None) for effect in effects}
        live = tuple(o for o in composed.observations if o[0] == 0 or o[:2] == (1, 4)
                     or (o[0] in heads and o[1] in (1, 4)))
        return live or composed.observations

    def _start(self, candidate):
        try:
            return self.driver.start(candidate)
        except OSError as error:
            if error.errno in START_REFUSALS:
                raise ValueError(f'driver refused to start the staged chain: {error.strerror}') from error
            raise _UnconfirmedDeviceOperation('device operation unconfirmed') from error
        except ValueError:
            raise
        except Exception as error:
            raise _UnconfirmedDeviceOperation('device operation unconfirmed') from error

    def _swap(self, current, candidate):
        """Route `candidate` in place of `current`. A swap the driver refused before acting
        (plugin_driver.SWAP_REFUSALS) changed nothing: a ValueError, so the change rolls back
        normally instead of leaving every slot quarantined."""
        try:
            return self.driver.swap(current, candidate)
        except OSError as error:
            if error.errno in SWAP_REFUSALS:
                raise ValueError(f'driver refused the swap: {error.strerror}') from error
            raise _UnconfirmedDeviceOperation('device operation unconfirmed') from error
        except ValueError:
            raise
        except Exception as error:
            raise _UnconfirmedDeviceOperation('device operation unconfirmed') from error

    def _group(self, channels):
        groups = []
        for channel in channels:
            path = f'/devices/0/inputs/{channel}/DelayCompGroup'
            value = self.publisher.state.get_value(path) if self.publisher.state.path_exists(path) else 0
            if type(value) is not int or not -1 <= value < 32:
                raise ValueError('invalid input compensation group')
            groups.append(value)
        if len(set(groups)) != 1:
            raise ValueError('linked inputs have different compensation groups')
        return groups[0]

    @staticmethod
    def _latency(sources):
        values = [getattr(source, 'descriptor_latency_samples', None) for source in sources.values()]
        if any(type(value) is not int or not 0 <= value <= 0x7fffffff for value in values):
            raise ValueError('authoritative descriptor latency required')
        return sum(values)

    def _record_pre(self, channels, override=None):
        """UAD MON (True) or REC for a chain: its first input's RecordPreEffects, as published
        (Console writes both inputs of a pair). `override` maps channels to a proposed value."""
        if override and channels in override:
            return override[channels]
        path = f'/devices/0/inputs/{channels[0]}/RecordPreEffects'
        state = self.publisher.state
        return state.path_exists(path) and state.get_value(path) is True

    def _input_latency(self, channels, sources, record_pre=None):
        latency = self._latency(sources)
        # UAD MON records ahead of the inserts: no plug-in latency on the record path.
        record = 0 if self._record_pre(channels, record_pre) else latency
        return InputLatency(channels, latency, record, self._group(channels))

    def _compensation_plan(self, setting, record_pre=None):
        inputs, occupied = [], set()
        for channels, chain in self._chains.items():
            self._current(chain)
            inputs.append(self._input_latency(channels, chain.sources, record_pre))
            occupied.update(channels)
        inputs.extend(InputLatency((channel,), 0, 0, self._group((channel,))) for channel in range(4) if channel not in occupied)
        return plan_compensation(setting, inputs)

    def set_compensation(self, setting):
        """Rebuild every chain and retained strip under one validated policy.

        Caller must serialize this with plugin transactions and publish the setting
        only after CONFIRMED. Indeterminate results require session quarantine.
        """
        if not self.compensation_supported or self._pending or self._compensating:
            return CompensationOutcome(Outcome.FAILED, setting, detail='Compensation transaction unavailable')
        prepared = []
        try:
            self._session()
            inputs = self._compensation_plan(setting)
            by_channels = {item.channels: item for item in inputs}
            # All recipe/metadata checks happen before any device operation.
            for channels, previous in self._chains.items():
                sources = copy.deepcopy(previous.sources)
                composed, arguments, ordered = self._compose(sources, channels, by_channels[channels])
                retained = self._retained(composed, ordered, previous, None)
                arguments['stage'] = True
                prepared.append((previous, sources, composed, arguments, ordered, retained))
        except Exception:
            log.exception('native chain: compensation policy %r refused', setting)
            return CompensationOutcome(Outcome.FAILED, setting, detail='Compensation policy validation failed')
        self._compensating = True
        candidates, swapped, strips_before, strips_after = [], [], None, None
        self._compensation_transition = candidates
        try:
            strips_before = self._compensation_io(self.driver.strip_delays)
            for previous, sources, composed, arguments, ordered, retained in prepared:
                native = self._compensation_io(self.driver.create, **arguments)
                candidate = self._compensation_io(self._candidate, native, composed, sources, ordered, previous, None, 0, retained)
                candidates.append((previous, candidate))
                self._observe(native, composed.observations)
            record, monitor, capacity = [0]*4, [0]*4, [0]*4
            for item in inputs:
                for channel in item.channels:
                    if channel < 4:  # the driver retunes the Mic strips; a Virtual chain carries its own delays
                        record[channel], monitor[channel], capacity[channel] = item.record, item.monitor, item.capacity
            expected = tuple(record), tuple(monitor), tuple(capacity)
            strips_after = self._compensation_io(self.driver.strip_delays, strips_before, record=expected[0],
                                                    monitor=expected[1], capacity=expected[2])
            if (strips_after.record, strips_after.monitor, strips_after.capacity) != expected:
                raise ValueError('retained strip compensation readback differs')
            for previous, candidate in candidates:
                self._current(previous)
                self._swap(previous.native, candidate.native)
                swapped.append((previous, candidate))
            # Observe after activation as well, before replacing any row adapters.
            for (_, _, composed, _, _, _), (_, candidate) in zip(prepared, candidates):
                self._observe(candidate.native, self._live_observations(composed))
        except ValueError as error:
            # A confirmed readback/validation failure is reversible. Driver I/O
            # exceptions are handled separately because their effect is unknown.
            log.warning('native chain: compensation %r refused: %s', setting, error)
            try:
                for previous, candidate in reversed(swapped):
                    self._compensation_io(self.driver.swap, candidate.native, previous.native)
                if strips_after is not None:
                    restored = self._compensation_io(self.driver.strip_delays, strips_after, record=strips_before.record,
                        monitor=strips_before.monitor, capacity=strips_before.capacity)
                    if (restored.record, restored.monitor, restored.capacity) != (
                            strips_before.record, strips_before.monitor, strips_before.capacity):
                        raise ValueError('retained strip restoration differs')
                for _, candidate in reversed(candidates):
                    self._compensation_io(self.driver.destroy, candidate.native)
            except Exception:
                self.invalidate()
                return CompensationOutcome(Outcome.INDETERMINATE, setting, inputs,
                    'Compensation restoration unconfirmed')
            self._compensating = False
            self._compensation_transition = None
            return CompensationOutcome(Outcome.FAILED, setting, inputs, 'Previous compensation restored')
        except Exception:
            log.exception('native chain: compensation %r device operation raised', setting)
            self.invalidate()
            return CompensationOutcome(Outcome.INDETERMINATE, setting, inputs,
                'Compensation device operation unconfirmed')
        # The new graph is authoritative only once all swaps and reads succeeded.
        for previous, candidate in candidates:
            self._install(previous, candidate)
        self.compensation_setting = setting
        self.compensation_inputs = inputs
        try:
            for previous, _ in candidates:
                self._compensation_io(self.driver.destroy, previous.native)
        except Exception:
            self.invalidate()
            return CompensationOutcome(Outcome.INDETERMINATE, setting, inputs,
                'Previous compensation chain retirement unconfirmed')
        self._compensating = False
        self._compensation_transition = None
        return CompensationOutcome(Outcome.CONFIRMED, setting, inputs)

    @staticmethod
    def _delay_values(item):
        return item.record, item.monitor, item.capacity

    def _plan_batch(self, slot, channels, sources, record_pre=None):
        old_plan = {item.channels: item for item in self._compensation_plan(self.compensation_setting)}
        prospective = {key: chain.sources for key, chain in self._chains.items() if key != channels}
        if sources:
            prospective[channels] = sources
        inputs, occupied = [], set()
        for key, providers in prospective.items():
            inputs.append(self._input_latency(key, providers, record_pre))
            occupied.update(key)
        inputs.extend(InputLatency((channel,), 0, 0, self._group((channel,))) for channel in range(4) if channel not in occupied)
        inputs = plan_compensation(self.compensation_setting, inputs)
        planned = {item.channels: item for item in inputs}
        affected = [channels] + [key for key in self._chains if key != channels
            and self._delay_values(old_plan[key]) != self._delay_values(planned[key])]
        previous = {key: self._chains.get(key) for key in affected}
        recipes = {}
        for key in affected:
            if key not in prospective:
                continue
            providers = copy.deepcopy(prospective[key])
            composed, arguments, ordered = self._compose(providers, key, planned[key],
                                                         self._record_pre(key, record_pre))
            if composed.needs_staging and not previous[key] and not self.start_staged:
                raise ValueError('this chain needs a driver that can start a staged chain '
                                 '(UA_FEAT2_PLUGIN_START_STAGED): update the driver')
            retained = self._retained(composed, ordered, previous[key], slot if key == channels else None)
            if previous[key]:
                arguments['stage'] = True
            recipes[key] = providers, composed, arguments, ordered, retained
        return _BatchChange(slot, channels, previous, recipes, inputs)

    def _allocate_batch(self, change, serial):
        self._pending[change.channels] = change
        try:
            change.strips_before = self._compensation_io(self.driver.strip_delays)
            for key, (sources, composed, arguments, ordered, retained) in change.recipes.items():
                stage = composed.stage_bundle() if composed.needs_staging else None
                # The staged recipe is the same chain: keep its chain/Unison flags.
                create_arguments = (dict(stage['create_arguments'], **{k: arguments[k] for k in ('chain', 'unison')
                                                                         if k in arguments})
                                    if stage is not None else arguments)
                native = self._compensation_io(self.driver.create, **create_arguments)
                if stage is not None:
                    for writes in stage['initialize_chunks']:
                        self._compensation_io(self.driver.service, native, writes=writes)
                candidate = self._compensation_io(self._candidate, native, composed, sources, ordered,
                    change.previous[key], change.slot if key == change.channels else None, serial, retained)
                change.candidates[key] = candidate
                self._observe(native, composed.observations)
        except ValueError as error:
            log.warning('native chain: allocation refused on %s: %s', change.slot, error)
            return Outcome.FAILED
        except Exception:
            log.exception('native chain: allocation on %s raised', change.slot)
            self.invalidate()
            return Outcome.INDETERMINATE
        return Outcome.CONFIRMED

    def _activate_batch(self, change):
        try:
            self._session()
            for previous in change.previous.values():
                if previous:
                    self._current(previous)
            record, monitor, capacity = [0]*4, [0]*4, [0]*4
            for item in change.inputs:
                for channel in item.channels:
                    if channel < 4:  # as in set_compensation
                        record[channel], monitor[channel], capacity[channel] = self._delay_values(item)
            expected = tuple(record), tuple(monitor), tuple(capacity)
            change.strips_after = self._compensation_io(self.driver.strip_delays,
                change.strips_before, record=expected[0], monitor=expected[1], capacity=expected[2])
            if (change.strips_after.record, change.strips_after.monitor, change.strips_after.capacity) != expected:
                raise ValueError('topology strip compensation differs')
            for key, candidate in change.candidates.items():
                previous = change.previous[key]
                if previous:
                    self._swap(previous.native, candidate.native)
                    change.swapped.append(key)
                elif change.recipes[key][1].needs_staging:
                    # Nothing to swap from: route the staged chain itself.
                    self._start(candidate.native)
            for key, candidate in change.candidates.items():
                self._observe(candidate.native, self._live_observations(change.recipes[key][1]))
            change.activated = True
        except ValueError as error:
            log.warning('native chain: activation on %s refused: %s', change.slot, error)
            return ActivateOutcome(Outcome.FAILED, previous_retained=True)
        except Exception:
            log.exception('native chain: activation on %s raised', change.slot)
            self.invalidate()
            return ActivateOutcome(Outcome.INDETERMINATE)
        return ActivateOutcome(Outcome.CONFIRMED, previous_retained=True)

    def _rollback_batch(self, change):
        try:
            for key in reversed(change.swapped):
                self._swap(change.candidates[key].native, change.previous[key].native)
            if change.strips_after is not None:
                before = change.strips_before
                restored = self._compensation_io(self.driver.strip_delays, change.strips_after,
                    record=before.record, monitor=before.monitor, capacity=before.capacity)
                if (restored.record, restored.monitor, restored.capacity) != (
                        before.record, before.monitor, before.capacity):
                    raise ValueError('topology strip restoration differs')
            for candidate in reversed(tuple(change.candidates.values())):
                self._compensation_io(self.driver.destroy, candidate.native)
        except Exception:
            log.exception('native chain: rollback on %s failed', change.slot)
            self.invalidate()
            return RollbackOutcome(Outcome.INDETERMINATE)
        del self._pending[change.channels]
        return RollbackOutcome(Outcome.CONFIRMED, True, True)

    def _commit_batch(self, change):
        try:
            for key, previous in change.previous.items():
                self._install(previous, change.candidates.get(key))
            self.compensation_inputs = change.inputs
            for previous in change.previous.values():
                if previous:
                    self._compensation_io(self.driver.destroy, previous.native)
        except Exception:
            log.exception('native chain: commit on %s failed', change.slot)
            self.invalidate()
            return Outcome.INDETERMINATE
        del self._pending[change.channels]
        return Outcome.CONFIRMED

    def _prepare_compensated(self, slot, channels, sources, old):
        try:
            change = self._plan_batch(slot, channels, sources)
        except Exception as error:
            log.exception('native chain: compensation plan refused a load on %s', slot)
            return PrepareOutcome(Outcome.FAILED, detail='Prospective compensation validation failed'
                                  + (f': {error}' if isinstance(error, ValueError) else ''))
        self._serial += 1
        status = self._allocate_batch(change, self._serial)
        candidate = change.candidates.get(channels)
        prepared = (PreparedPlugin(change, candidate.leases[slot].loaded,
                    old.loaded.handle if old else None) if candidate else None)
        if status is Outcome.FAILED and prepared is None:
            if self._rollback_batch(change).status is not Outcome.CONFIRMED:
                status = Outcome.INDETERMINATE
        return PrepareOutcome(status, prepared, '' if status is Outcome.CONFIRMED else
                              'Prospective chain allocation/readback failed')

    def _retire_compensated(self, slot, handle, previous):
        try:
            sources = {path: source for path, source in previous.sources.items() if path != slot}
            change = self._plan_batch(slot, previous.channels, sources)
        except Exception:
            log.exception('native chain: compensation plan refused a removal on %s', slot)
            return RetireOutcome(Outcome.FAILED, handle, previous_retained=True,
                                 detail='Surviving compensation validation failed')
        status = self._allocate_batch(change, 0)
        if status is Outcome.CONFIRMED:
            status = self._activate_batch(change).status
        if status is Outcome.FAILED:
            rollback = self._rollback_batch(change)
            if rollback.status is Outcome.CONFIRMED:
                return RetireOutcome(Outcome.FAILED, handle, previous_retained=True)
            status = Outcome.INDETERMINATE
        if status is Outcome.CONFIRMED:
            status = self._commit_batch(change)
        return RetireOutcome(status, handle, retired=status is Outcome.CONFIRMED)

    def set_record_tap(self, channel, pre):
        """UAD REC (False) / MON (True) on the chain whose first input is `channel`.

        Caller must serialize this with plugin transactions and publish RecordPreEffects only
        after CONFIRMED; INDETERMINATE requires session quarantine. With no such chain nothing
        changes on the device: a chain composed later reads the published value."""
        def outcome(status, detail=''):
            return CompensationOutcome(status, self.compensation_setting, self.compensation_inputs, detail)
        chain = next((chain for key, chain in self._chains.items() if key[0] == channel), None)
        if type(pre) is not bool:
            return outcome(Outcome.FAILED, 'Record tap must be true or false')
        if chain is None:
            return outcome(Outcome.CONFIRMED)
        if self._pending or self._compensating:
            return outcome(Outcome.FAILED, 'A native chain transition is pending')
        try:
            self._current(chain)
            if self.compensation_supported:
                def delays(override):
                    return [self._delay_values(item) for item in
                            self._compensation_plan(self.compensation_setting, override)]
                if delays(None) != delays({chain.channels: pre}):
                    return outcome(*self._rebuild_record_tap(chain, pre))
            tap = chain.composed.record_tap_plan(pre)
            restore = chain.composed.record_tap_plan(self._record_pre(chain.channels))
        except Exception:
            log.exception('native chain: record tap on input %d refused', channel)
            return outcome(Outcome.FAILED, 'Record tap validation failed')
        try:
            self._apply_tap(chain.native, tap)
        except ValueError as error:
            log.warning('native chain: record tap on input %d refused: %s', channel, error)
            try:
                self._apply_tap(chain.native, restore)
            except Exception:
                self.invalidate()
                return outcome(Outcome.INDETERMINATE, 'Record tap restoration unconfirmed')
            return outcome(Outcome.FAILED, 'Previous record tap restored')
        except Exception:
            log.exception('native chain: record tap on input %d raised', channel)
            self.invalidate()
            return outcome(Outcome.INDETERMINATE, 'Record tap device operation unconfirmed')
        return outcome(Outcome.CONFIRMED)

    def _apply_tap(self, native, plan):
        self._compensation_io(self.driver.service, native, writes=plan.writes)
        self._observe(native, plan.observations)

    def _rebuild_record_tap(self, chain, pre):
        """The record delay moves with the tap: a new chain through the topology batch."""
        label = f'/devices/0/inputs/{chain.channels[0]}/RecordPreEffects'
        try:
            change = self._plan_batch(label, chain.channels, chain.sources, {chain.channels: pre})
        except Exception:
            log.exception('native chain: record tap compensation refused on %s', label)
            return Outcome.FAILED, 'Record tap compensation validation failed'
        status = self._allocate_batch(change, 0)
        if status is Outcome.CONFIRMED:
            status = self._activate_batch(change).status
        if status is Outcome.FAILED:
            if self._rollback_batch(change).status is Outcome.CONFIRMED:
                return Outcome.FAILED, 'Previous record tap restored'
            status = Outcome.INDETERMINATE
        if status is Outcome.CONFIRMED:
            status = self._commit_batch(change)
        return status, '' if status is Outcome.CONFIRMED else 'Record tap rebuild unconfirmed'

    def capabilities(self):
        return [entry | {'slots': (entry['slots'] if '/preamps/' in entry['slots'][0]
                                  else [entry['slots'][0].rsplit('/', 1)[0] + '/' + str(i)
                                        for i in range(self.max_inserts)])}
                for entry in super().capabilities()]

    def _owned(self, slot, handle):
        if self._pending or self._compensating:
            raise ValueError('physical chain transition is pending')
        return super()._owned(slot, handle)

    def _current(self, chain):
        self._session()
        if self._chains.get(chain.channels) is not chain:
            raise ValueError('physical chain ownership changed')
        for slot, lease in chain.leases.items():
            if self._leases.get(slot) is not lease:
                raise ValueError('logical row adapter changed')
            self.publisher.snapshot_control(slot, lease.loaded.handle, 'Power')
            if self.publisher.state.get_value(slot + '/EffectName') != lease.loaded.name:
                raise ValueError('logical effect name changed')
            if self._channels(slot) != chain.channels:
                raise ValueError('input layout changed while a chain was live')

    def _compose(self, sources, channels, compensation=None, record_pre=None):
        def index(slot):
            return -1 if '/preamps/0/effects/' in slot else int(slot.rsplit('/', 1)[1])
        ordered = sorted(sources, key=index)
        delay = {'record_delay_samples': self.record_delay_samples}
        if compensation is None and any(getattr(source, 'descriptor_latency_samples', 0)
                                        for source in sources.values()):
            raise ValueError('nonzero descriptor latency requires compensation support')
        if compensation is not None:
            delay = dict(record_delay_samples=compensation.record,
                monitor_delay_samples=compensation.monitor, active_capacity=compensation.capacity)
        if record_pre is None:
            record_pre = self._record_pre(channels)
        # UAD REC is every composer's default; only MON is passed.
        mon = {'record_pre_effects': True} if record_pre else {}
        composed = self.composer([sources[slot] for slot in ordered], channels=channels, **delay,
            slots=tuple(index(slot) for slot in ordered), **mon)
        if len(composed.effects) != len(ordered):
            raise ValueError('composer effect count differs')
        for slot, view in zip(ordered, composed.effects):
            if (view.slot != slot or tuple(view.channels) != channels
                    or view.name != sources[slot].name):
                raise ValueError('composer logical effect differs')
            self.publisher.validate(view.initial(OwnedHandle(
                1, self.publisher.owner, self.publisher.generation)))
        self._validate_observations(composed.observations)
        arguments = (composed.stage_bundle()['create_arguments'] if composed.needs_staging
                     else composed.create_arguments())
        expected = (dsp_input(channels[0]), dsp_input(channels[1]) if len(channels) == 2 else 32)
        if (tuple(arguments.get('input_pair', ())) != expected
                or type(arguments.get('dsp')) is not int or not 0 <= arguments['dsp'] < 8):
            raise ValueError('composer physical layout differs')
        arguments['chain'] = True
        if any(index(slot) == -1 for slot in ordered):
            if not self.unison:
                raise ValueError('Unison header is unsupported')
            arguments['unison'] = True
        return composed, arguments, ordered

    def _retained(self, composed, ordered, previous, changed_slot):
        """Check regenerated encoder metadata against both current public views."""
        retained = {}
        if previous is None:
            return retained
        for slot, view in zip(ordered, composed.effects):
            if slot == changed_slot or slot not in previous.leases:
                continue
            handle = previous.leases[slot].loaded.handle
            fresh = view.initial(handle)
            power = self.publisher.snapshot_control(slot, handle, 'Power').values['Power']
            node, _ = self.publisher.state._resolve_path(slot + '/parameters')
            children = node.get('children', {}) if node else {}
            if set(children) != {str(p['id']) for p in fresh.parameters}:
                raise ValueError('regenerated parameter identities differ')
            for parameter in fresh.parameters:
                ident = str(parameter['id'])
                self.publisher.snapshot_control(slot, handle,
                    'parameters/' + ident + '/NormalizedValue')
                if children[ident]['properties'] != parameter['properties']:
                    raise ValueError('regenerated controls differ from retained public values')
            latency = self.publisher.state.get_value(slot + '/Latency')
            if latency != self.publisher.helper.get_value(slot + '/Latency') or latency != fresh.latency:
                raise ValueError('retained latency differs from regenerated metadata')
            loaded = LoadedPlugin(fresh.name, handle, power, latency, copy.deepcopy(fresh.parameters))
            self.publisher.validate(loaded)
            retained[slot] = loaded
        return retained

    def _candidate(self, native, composed, sources, ordered, previous, changed_slot, serial, retained):
        if type(native.handle) is not int or not 0 < native.handle <= 0xffffffff:
            raise ValueError('bounded native instance identity required')
        leases = {}
        for slot, view in zip(ordered, composed.effects):
            if previous and slot in previous.leases and slot != changed_slot:
                handle = previous.leases[slot].loaded.handle
            else:
                # Logical identity, not a kernel handle. The monotonic upper part
                # prevents recycled kernel handles from aliasing old panels. This
                # stays <=2^53-1 for exact JSON/JavaScript numeric transport.
                handle = OwnedHandle(serial << 32 | native.handle,
                                     self.publisher.owner, self.publisher.generation)
            loaded = retained[slot] if slot in retained else view.initial(handle)
            self.publisher.validate(loaded)
            leases[slot] = _Lease(native, loaded, view)
        return _Chain(tuple(composed.effects[0].channels), native, leases, sources, composed)

    def _install(self, previous, candidate):
        if previous:
            for slot in previous.leases:
                self._leases.pop(slot, None)
            self._chains.pop(previous.channels, None)
        if candidate:
            self._chains[candidate.channels] = candidate
            self._leases.update(candidate.leases)

    def prepare(self, slot, name):
        match = re.fullmatch(r'/devices/0/inputs/([0-3]|1[4-7])/(preamps/0/)?effects/([0-3])', slot)
        if (not match or (match[2] and (not self.unison or int(match[1]) > 3 or match[3] != '0'))
                or (not match[2] and int(match[3]) >= self.max_inserts)):
            return PrepareOutcome(Outcome.FAILED, detail='No native chain recipe for this slot')
        channels = self._channels(slot)
        if not channels or (len(channels) == 2 and not self.stereo):
            return PrepareOutcome(Outcome.FAILED, detail='Native input layout is unsupported')
        template_slot = slot.rsplit('/', 1)[0] + '/0'
        template = self.providers.get((template_slot, name, channels))
        if template is None:
            return PrepareOutcome(Outcome.FAILED, detail=self._layout_refusal(template_slot, name, channels)
                                  or 'No native chain recipe for this effect')
        previous = self._chains.get(channels)
        old = previous.leases.get(slot) if previous else None
        if self._pending or self._compensating:
            return PrepareOutcome(Outcome.FAILED, detail='A native chain transition is pending')
        if old and not self.replacement:
            return PrepareOutcome(Outcome.FAILED, detail='Native replacement is unsupported')
        if previous is None and len(self._chains) >= self.max_instances:
            return PrepareOutcome(Outcome.FAILED, detail='Native DSP chain limit reached')
        if any(set(channels).intersection(other) for other in self._chains if other != channels):
            return PrepareOutcome(Outcome.FAILED, detail='Inputs already belong to another native chain')
        try:
            self._session()
            if previous:
                self._current(previous)
            if old is None and (self.publisher.state.get_value(slot + '/EffectName') != ''
                                or self.publisher.state.get_value(slot + '/EffectInstance') != 0):
                raise ValueError('slot ownership is unknown')
            if self._serial >= 0x1fffff:
                raise ValueError('exact logical identity range exhausted')
            source = copy.deepcopy(template)
            source.reset()
            sources = copy.deepcopy(previous.sources) if previous else {}
            sources[slot] = source
            if self.compensation_supported:
                return self._prepare_compensated(slot, channels, sources, old)
            composed, arguments, ordered = self._compose(sources, channels)
            retained = self._retained(composed, ordered, previous, slot)
            chunks = ()
            if composed.needs_staging:
                if not previous and not self.start_staged:
                    raise ValueError('this chain needs a driver that can start a staged chain '
                                     '(UA_FEAT2_PLUGIN_START_STAGED): update the driver')
                stage = composed.stage_bundle()
                arguments = dict(stage['create_arguments'], **{k: arguments[k] for k in ('chain', 'unison')
                                                                if k in arguments})
                chunks = stage['initialize_chunks']
            elif previous:
                arguments['stage'] = True
            self._serial += 1
        except Exception as error:
            log.warning('native chain: load on %s refused: %s', slot, error)
            return PrepareOutcome(Outcome.FAILED, detail='Native chain validation failed'
                                  + (f': {error}' if isinstance(error, ValueError) else ''))
        try:
            native = self.driver.create(**arguments)
            for writes in chunks:
                self.driver.service(native, writes=writes)
            candidate = self._candidate(native, composed, sources, ordered, previous, slot, self._serial, retained)
        except Exception:
            self.invalidate()
            return PrepareOutcome(Outcome.INDETERMINATE, detail='Native chain CREATE unconfirmed')
        change = _Change(slot, previous, candidate)
        change.start_staged = bool(chunks or composed.needs_staging) and not previous
        self._pending[channels] = change
        prepared = PreparedPlugin(change, candidate.leases[slot].loaded,
                                  old.loaded.handle if old else None)
        try:
            self._observe(native, composed.observations)
        except Exception:
            return PrepareOutcome(Outcome.FAILED, prepared, 'Native chain initial readback unconfirmed')
        return PrepareOutcome(Outcome.CONFIRMED, prepared)

    def activate(self, prepared):
        change = prepared.token
        if isinstance(change, _BatchChange):
            if self._pending.get(change.channels) is not change:
                return ActivateOutcome(Outcome.FAILED)
            return self._activate_batch(change)
        if self._pending.get(change.candidate.channels) is not change:
            return ActivateOutcome(Outcome.FAILED)
        try:
            self._session()
            if change.previous:
                self._current(change.previous)
                self._swap(change.previous.native, change.candidate.native)
            elif getattr(change, 'start_staged', False):
                self._start(change.candidate.native)
            change.activated = True
        except ValueError as error:
            log.warning('native chain: activation on %s refused: %s', change.slot, error)
            return ActivateOutcome(Outcome.FAILED, previous_retained=True)
        except Exception:
            log.exception('native chain: activation on %s raised', change.slot)
            self.invalidate()
            return ActivateOutcome(Outcome.INDETERMINATE)
        # Keep old adapters until the coordinator commits publication. Its busy
        # fence and _owned above prevent reads through an in-flight topology.
        return ActivateOutcome(Outcome.CONFIRMED, previous_retained=True)

    def rollback(self, prepared):
        change = prepared.token
        if isinstance(change, _BatchChange):
            if self._pending.get(change.channels) is not change:
                return RollbackOutcome(Outcome.INDETERMINATE)
            return self._rollback_batch(change)
        if self._pending.get(change.candidate.channels) is not change:
            return RollbackOutcome(Outcome.INDETERMINATE)
        try:
            if change.previous and change.activated:
                self.driver.swap(change.candidate.native, change.previous.native)
            self.driver.destroy(change.candidate.native)
        except Exception:
            self.invalidate()
            return RollbackOutcome(Outcome.INDETERMINATE)
        del self._pending[change.candidate.channels]
        return RollbackOutcome(Outcome.CONFIRMED, True, True)

    def finalize(self, prepared):
        change = prepared.token
        if isinstance(change, _BatchChange):
            if self._pending.get(change.channels) is not change or not change.activated:
                return FinalizeOutcome(Outcome.INDETERMINATE)
            try:
                self._session()
                self.publisher.snapshot_control(change.slot, prepared.loaded.handle, 'Power')
            except Exception:
                self.invalidate()
                return FinalizeOutcome(Outcome.INDETERMINATE)
            return FinalizeOutcome(self._commit_batch(change))
        if self._pending.get(change.candidate.channels) is not change or not change.activated:
            return FinalizeOutcome(Outcome.INDETERMINATE)
        try:
            self._session()
            self.publisher.snapshot_control(change.slot, prepared.loaded.handle, 'Power')
            self._install(change.previous, change.candidate)
            if change.previous:
                self.driver.destroy(change.previous.native)
        except Exception:
            self.invalidate()
            return FinalizeOutcome(Outcome.INDETERMINATE)
        del self._pending[change.candidate.channels]
        return FinalizeOutcome(Outcome.CONFIRMED)

    def retire(self, slot, handle):
        lease = self._owned(slot, handle)
        previous = self._chains[self._provider_channels(lease.provider)]
        self._current(previous)
        if self.compensation_supported:
            return self._retire_compensated(slot, handle, previous)
        if len(previous.leases) == 1:
            try:
                self.driver.destroy(previous.native)
            except Exception:
                self.invalidate()
                return RetireOutcome(Outcome.INDETERMINATE, handle)
            self._install(previous, None)
            return RetireOutcome(Outcome.CONFIRMED, handle, retired=True)
        try:
            sources = copy.deepcopy({p: source for p, source in previous.sources.items() if p != slot})
            composed, arguments, ordered = self._compose(sources, previous.channels)
            retained = self._retained(composed, ordered, previous, None)
            arguments['stage'] = True
        except Exception:
            return RetireOutcome(Outcome.FAILED, handle, previous_retained=True,
                                 detail='Surviving chain validation failed')
        try:
            native = self.driver.create(**arguments)
            candidate = self._candidate(native, composed, sources, ordered, previous, None, 0, retained)
        except Exception:
            self.invalidate()
            return RetireOutcome(Outcome.INDETERMINATE, handle)
        try:
            self._observe(native, composed.observations)
        except Exception:
            try:
                self.driver.destroy(native)
            except Exception:
                self.invalidate()
                return RetireOutcome(Outcome.INDETERMINATE, handle)
            return RetireOutcome(Outcome.FAILED, handle, previous_retained=True,
                                 detail='Surviving chain readback failed')
        try:
            self.driver.swap(previous.native, native)
            self._install(previous, candidate)
            self.driver.destroy(previous.native)
        except Exception:
            self.invalidate()
            return RetireOutcome(Outcome.INDETERMINATE, handle)
        # Existing coordinator contract: terminal physical retirement precedes
        # empty publication. A later publication failure must quarantine, never
        # falsely claim rollback to this now destroyed allocation.
        return RetireOutcome(Outcome.CONFIRMED, handle, retired=True)
