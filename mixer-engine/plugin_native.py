"""Opt-in native insert adapter; effect data comes from explicit private providers.

The provider is trusted local code, explicitly loaded by the owner. It plans
writes and formats verified values; this adapter owns every device operation.
"""

import logging
import copy
from dataclasses import dataclass
import importlib.util
import re
import sys
import time
import uuid
import traceback

from plugin_remote import RemoteReadback
from plugin_driver import PluginDriver
from plugin_publication import ControlReadback, LoadedPlugin, OwnedHandle, PublicationError, Publisher
from plugin_transactions import (
    ActivateOutcome, ApplyControlOutcome, FinalizeOutcome, Outcome,
    PluginTransactions, PreparedControl, PreparedPlugin, PrepareControlOutcome,
    PrepareOutcome, RestoreControlOutcome, RetireOutcome, RollbackOutcome,
)


log = logging.getLogger(__name__)

@dataclass(frozen=True)
class ControlPlan:
    writes: tuple
    observations: tuple  # (private row, offset, exact expected uint32 tuple)
    values: dict
    state: object


SERVICE_WORDS = 1016  # PluginDriver.service command bound


def _write_chunks(words, maximum=SERVICE_WORDS):
    """Split WRITE commands ({0x0014LLLL, ...}, LLLL = total block words) into service-sized
    chunks at whole-block boundaries; one chunk when they already fit."""
    words = tuple(words)
    if len(words) <= maximum:
        return (words,)
    chunks, current, i = [], [], 0
    while i < len(words):
        n = words[i] & 0xffff
        if words[i] >> 16 != 0x14 or n < 4 or i + n > len(words) or n > maximum:
            raise ValueError('bounded WRITE blocks required to split a long control')
        if current and len(current) + n > maximum:
            chunks.append(tuple(current))
            current = []
        current.extend(words[i:i + n])
        i += n
    chunks.append(tuple(current))
    return tuple(chunks)


@dataclass
class _Lease:
    native: object
    loaded: LoadedPlugin
    provider: object
    previous: object = None
    activated: bool = False


class NativePluginBackend:
    """Explicit input recipes with disjoint leases; no reconnect replay."""

    native_single_insert = True

    def __init__(self, publisher, driver, provider, *, session_valid=lambda: True,
                 max_instances=1, stereo=False, replacement=False, unison=False):
        self.publisher, self.driver = publisher, driver
        self.session_valid = session_valid
        if type(max_instances) is not int or not 1 <= max_instances <= 4:
            raise ValueError("native instance limit must be 1..4")
        self.max_instances, self.stereo = max_instances, bool(stereo)
        self.replacement = bool(replacement)
        if type(unison) is not bool:
            raise ValueError('explicit Unison capability required')
        if unison and not getattr(self, 'chain_supported', False):
            raise ValueError('Unison requires input-header composition')
        self.unison = unison
        providers = provider if isinstance(provider, (tuple, list)) else (provider,)
        # A growable set (maps still being built on this machine) may start empty.
        if not providers and not getattr(provider, 'growable', False):
            raise ValueError('at least one explicit native provider required')
        self.providers = {}
        self._leases = {}
        self._pending = {}
        self._dynamic = {}   # native handle -> {(row, word)} the running plug-in owns
        self.add_providers(providers)

    def add_providers(self, providers):
        """Offer more recipes (a plug-in whose maps just landed). All are checked before any is
        added; existing recipes and live leases are untouched. Call between transactions."""
        added = {}
        for item in providers:
            match = re.fullmatch(r'/devices/0/inputs/([0-3])/(preamps/0/)?effects/0', item.slot)
            if (not match or not isinstance(item.name, str) or not item.name
                    or (match[2] and not self.unison)
                    or type(item.sample_rate) is not int or item.sample_rate <= 0):
                raise ValueError('named supported input effect-zero provider required')
            first = int(match[1])
            channels = getattr(item, 'channels', (first,))
            if (type(channels) is not tuple or any(type(c) is not int for c in channels)
                    or channels not in ((first,), (first, first + 1))
                    or (len(channels) == 2 and first not in (0, 2))):
                raise ValueError('explicit mono input or adjacent stereo pair required')
            key = (item.slot, item.name, channels)
            if key in self.providers or key in added:
                raise ValueError('duplicate native provider for slot and effect')
            added[key] = item
        self.providers.update(added)
        return list(added)

    def _channels(self, slot):
        first = int(slot.split('/')[4])
        linked = self.publisher.state.get_value(f'/devices/0/inputs/{first}/Stereo')
        if linked is True:
            return (first, first + 1) if first in (0, 2) else ()
        return (first,)

    def _layout_refusal(self, slot, name, channels):
        """Why `name` has no provider for this layout when it has one for the other.

        A bundle is mono or stereo: on an input whose link state it lacks, say which it is
        and what to do, instead of a generic missing-recipe error."""
        if not channels:
            return None
        first = channels[0]
        if len(channels) == 2:
            other, kind, verb, want = (first,), 'mono', 'unlink', 'stereo'
        else:
            other, kind, verb, want = (first, first + 1), 'stereo', 'link', 'mono'
        if (slot, name, other) not in self.providers:
            return None
        return f'{name}: {kind} only — {verb} the input or use a {want} bundle'

    @staticmethod
    def _provider_channels(provider):
        return getattr(provider, 'channels', (int(provider.slot.split('/')[4]),))

    def capabilities(self):
        """Configured recipes only; licensing remains the device admission gate."""
        return [dict(name=p.name, slots=[p.slot], sample_rate=p.sample_rate,
                     channels=list(self._provider_channels(p)))
                for p in self.providers.values()
                if self.stereo or len(self._provider_channels(p)) == 1]

    def invalidate(self):
        self.driver.invalidate()
        if self.publisher.connected:
            self.publisher.disconnect()

    def _session(self):
        if not self.publisher.connected or not self.session_valid():
            self.invalidate()
            raise ValueError('native session ended; restart required')

    def _owned(self, slot, handle):
        self._session()
        lease = self._leases.get(slot)
        if lease is None or lease.loaded.handle != handle:
            raise ValueError('exact current native lease required')
        return lease

    @staticmethod
    def _validate_observations(observations):
        # Local safety bound: large plug-ins (thousands of shadow runs) are audited in full once at
        # load; control changes audit only the words they write.
        if type(observations) is not tuple or not 1 <= len(observations) <= 32768:
            raise ValueError('bounded authoritative observation plan required')
        for row, offset, expected in observations:
            if (type(row) is not int or not 0 <= row < 128
                    or type(offset) is not int or not 0 <= offset < 1 << 24
                    or type(expected) is not tuple or not 1 <= len(expected) <= 64
                    or any(type(word) is not int or not 0 <= word <= 0xFFFFFFFF
                           for word in expected)):
                raise ValueError('invalid authoritative observation')

    def _observe(self, native, observations, learn=False):
        """Read back `observations` and require them. Words the running plug-in owns (learned
        for this instance, see prepare_control) are not compared.

        learn: before a control writes anything, its prior words can only differ because the
        DSP code changed them since they were verified (delay pointers, LFO phases, meters);
        a few such words are learned for this instance instead of failing."""
        self._validate_observations(observations)
        dynamic = self._dynamic.setdefault(native.handle, set())
        words = self._read_spans(native, observations)
        for row, offset, expected in observations:
            actual = tuple(words[row, offset + i] for i in range(len(expected)))
            if actual == expected:
                continue
            diffs = [(offset + i, e, a) for i, (e, a) in enumerate(zip(expected, actual))
                     if e != a and (row, offset + i) not in dynamic]
            if not diffs:
                continue
            # A word whose value moves between two reads is the running DSP code's (a ramp, a
            # pointer): a write that failed would read back the same wrong value both times.
            try:
                again = self.driver.service(native, read=(row, offset, len(expected)))
            except OSError:
                self.invalidate()
                raise
            moving = {w for w, _, a in diffs if again[w - offset] != a}
            if moving:
                dynamic.update((row, w) for w in moving)
                log.info('native instance %s: row %d words %s change while it runs; not verified',
                         native.handle, row, ', '.join(str(w) for w in sorted(moving)))
                diffs = [d for d in diffs if d[0] not in moving]
                if not diffs:
                    continue
            if learn and len(diffs) <= max(2, len(expected) // 2):
                dynamic.update((row, w) for w, _, _ in diffs)
                log.info('native instance %s: row %d words %s are the running plug-in\'s; not verified',
                         native.handle, row, ', '.join(str(w) for w, _, _ in diffs))
                continue
            raise ValueError('native coefficient observation differs: row %d %s' % (
                row, ', '.join(f'word {w} 0x{e:x}->0x{a:x}' for w, e, a in diffs[:4])))
        self._session()

    def _read_spans(self, native, observations):
        """{(row, word): value} for every observed word, read in as few SERVICE calls as fit.

        Each SERVICE is a DSP round trip (tens of ms) and reads at most 64 words; codecs emit
        16-word runs, so a control's 25 runs on one row are 8 reads, not 25. Only runs that
        touch are joined: a read never covers a word between observations. Reading such gap
        words (API Preamp's load, 2026-10-19) got a reply the driver refused (EPROTO) and
        quarantined the DSP until a power cycle."""
        spans = []
        for row, offset, expected in sorted(observations):
            end = offset + len(expected)
            if (spans and spans[-1][0] == row and offset <= spans[-1][2]
                    and end - spans[-1][1] <= 64):
                spans[-1][2] = max(spans[-1][2], end)
            else:
                spans.append([row, offset, end])
        words = {}
        for row, start, end in spans:
            try:
                values = self.driver.service(native, read=(row, start, end - start))
            except OSError as error:
                log.error('native instance %s: read row %d words %d..%d failed: %s',
                          native.handle, row, start, end - 1, error)
                self.invalidate()
                raise
            words.update(((row, start + i), v) for i, v in enumerate(values))
        return words

    def read(self, slot, handle, query):
        """Fresh reads from exact provider allowlists; no remote writes."""
        lease = self._owned(slot, handle)
        mapping = lease.provider.remote_reads
        if query.kind == 'opcode':
            # An exact read-only opcode tuple must be supplied by the provider.
            # Never forward arbitrary opcodes or translate tagged IDs by guessing.
            if query.opcode != 519 or type(query.count) is not int or not 1 <= query.count <= 64:
                raise ValueError('only bounded authorized meter-array reads are supported')
            keys = [(('opcode', query.opcode, query.variable_id,
                      query.count, query.header_word), query.count)]
        elif query.kind in ('ints', 'floats') and 1 <= len(query.ids) <= 64:
            keys = [((query.kind, variable), 1) for variable in query.ids]
        else:
            raise ValueError('only bounded authorized reads are supported')
        reads = []
        for key, count in keys:
            location = mapping.get(key)
            if type(location) is not tuple or len(location) != 2:
                raise ValueError('remote variable has no authorized native mapping')
            row, offset = location
            self._validate_observations(((row, offset, (0,) * count),))
            reads.append((row, offset, count))
        words = []
        for address in reads:
            try:
                words.extend(self.driver.service(lease.native, read=address))
            except OSError:
                self.invalidate()
                raise
        if self._owned(slot, handle) is not lease:
            raise ValueError('native lease changed during read')
        return RemoteReadback(handle, query, tuple(words))

    def allows_idle_mixer_link_change(self, canonical):
        """Allow changing a physical mono/stereo pair only before native ownership."""
        return (bool(re.fullmatch(r'/devices/0/inputs/[02]/Stereo', canonical))
                and not self._leases and not self._pending)

    def prepare(self, slot, name):
        if not re.fullmatch(r'/devices/0/inputs/[0-3]/effects/0', slot):
            return PrepareOutcome(Outcome.FAILED, detail='This effect has no native recipe for this slot')
        channels = self._channels(slot)
        if not channels:
            return PrepareOutcome(Outcome.FAILED, detail='Load linked inputs on the first channel of the pair')
        if len(channels) == 2 and not self.stereo:
            return PrepareOutcome(Outcome.FAILED, detail='This driver does not support stereo native chains')
        provider = self.providers.get((slot, name, channels))
        if provider is None:
            return PrepareOutcome(Outcome.FAILED, detail=self._layout_refusal(slot, name, channels)
                                  or 'This effect has no native recipe for this slot')
        previous = self._leases.get(slot)
        if slot in self._pending:
            return PrepareOutcome(Outcome.FAILED, detail='A native replacement is pending')
        if previous is not None and not self.replacement:
            return PrepareOutcome(Outcome.FAILED, detail='Replacement is unsupported')
        if previous is None and len(self._leases) >= self.max_instances:
            return PrepareOutcome(Outcome.FAILED, detail='Native DSP instance limit reached')
        if any(set(channels).intersection(self._provider_channels(lease.provider))
               for lease in self._leases.values() if lease is not previous):
            return PrepareOutcome(Outcome.FAILED, detail='Input channels already have a native chain')
        published_name = self.publisher.state.get_value(slot + '/EffectName')
        published_handle = self.publisher.state.get_value(slot + '/EffectInstance')
        if previous is None:
            if published_name != '' or published_handle != 0:
                return PrepareOutcome(Outcome.FAILED, detail='Slot ownership is unknown')
        elif (published_name != previous.loaded.name
              or published_handle != previous.loaded.handle.value
              or channels != self._provider_channels(previous.provider)):
            return PrepareOutcome(Outcome.FAILED, detail='Current lease differs from slot state')
        try:
            self._session()
            if previous is not None:
                provider = copy.deepcopy(provider)
            provider.reset()
            initial = provider.initial(OwnedHandle(
                1, self.publisher.owner, self.publisher.generation))
            self.publisher.validate(initial)
            if initial.name != name:
                raise ValueError('provider name differs')
            arguments = provider.create_arguments()
            if (tuple(arguments.get('input_pair', ())) != (channels[0], channels[1] if len(channels) == 2 else 32)
                    or type(arguments.get('dsp')) is not int
                    or not 0 <= arguments['dsp'] < 8):
                raise ValueError('explicit matching input layout and DSP required')
            if previous is not None:
                arguments["stage"] = True
            observations = provider.observations()
            self._validate_observations(observations)
        except Exception:
            traceback.print_exc()
            return PrepareOutcome(Outcome.FAILED, detail='Native provider validation failed')
        try:
            native = self.driver.create(**arguments)
        except Exception:
            # CREATE may have acquired resources before an output-copy failure.
            self.invalidate()
            return PrepareOutcome(Outcome.INDETERMINATE, detail='Native CREATE unconfirmed')
        handle = OwnedHandle(native.handle, self.publisher.owner, self.publisher.generation)
        loaded = LoadedPlugin(initial.name, handle, initial.power,
                              initial.latency, copy.deepcopy(initial.parameters))
        lease = _Lease(native, loaded, provider, previous)
        if previous is None:
            self._leases[slot] = lease
        else:
            self._pending[slot] = lease
        prepared = PreparedPlugin(lease, loaded, previous.loaded.handle if previous else None)
        try:
            self._observe(native, observations)
        except Exception:
            return PrepareOutcome(Outcome.FAILED, prepared, 'Initial readback unconfirmed')
        return PrepareOutcome(Outcome.CONFIRMED, prepared)

    def activate(self, prepared):
        self._session()
        lease = prepared.token
        slot = lease.provider.slot
        if lease.previous is None:
            if self._owned(slot, prepared.loaded.handle) is not lease:
                return ActivateOutcome(Outcome.FAILED)
        else:
            if (self._pending.get(slot) is not lease
                    or self._leases.get(slot) is not lease.previous
                    or prepared.previous != lease.previous.loaded.handle):
                return ActivateOutcome(Outcome.FAILED)
            try:
                self.driver.swap(lease.previous.native, lease.native)
            except Exception:
                self.invalidate()
                return ActivateOutcome(Outcome.INDETERMINATE)
            self._leases[slot] = lease
        lease.activated = True
        return ActivateOutcome(Outcome.CONFIRMED, previous_retained=True)

    def rollback(self, prepared):
        lease = prepared.token
        slot = lease.provider.slot
        expected = self._pending.get(slot) if lease.previous else self._leases.get(slot)
        if expected is not lease:
            return RollbackOutcome(Outcome.INDETERMINATE)
        try:
            if lease.previous and lease.activated:
                self.driver.swap(lease.native, lease.previous.native)
            self.driver.destroy(lease.native)
        except Exception:
            self.invalidate()
            return RollbackOutcome(Outcome.INDETERMINATE)
        if lease.previous:
            self._leases[slot] = lease.previous
            del self._pending[slot]
        else:
            del self._leases[slot]
        return RollbackOutcome(Outcome.CONFIRMED, True, True)

    def finalize(self, prepared):
        lease = self._owned(prepared.token.provider.slot, prepared.loaded.handle)
        if lease is not prepared.token:
            return FinalizeOutcome(Outcome.INDETERMINATE)
        if lease.previous:
            if self._pending.get(lease.provider.slot) is not lease or not lease.activated:
                return FinalizeOutcome(Outcome.INDETERMINATE)
            try:
                self.driver.destroy(lease.previous.native)
            except Exception:
                self.invalidate()
                return FinalizeOutcome(Outcome.INDETERMINATE)
            del self._pending[lease.provider.slot]
            lease.previous = None
        return FinalizeOutcome(Outcome.CONFIRMED)

    def retire(self, slot, handle):
        lease = self._owned(slot, handle)
        try:
            self.driver.destroy(lease.native)
        except Exception:
            self.invalidate()
            return RetireOutcome(Outcome.INDETERMINATE, handle)
        del self._leases[slot]
        return RetireOutcome(Outcome.CONFIRMED, handle, retired=True)

    def prepare_control(self, slot, handle, field, value):
        started = time.monotonic()
        lease = self._owned(slot, handle)
        prior = lease.provider.snapshot(field)
        proposal = lease.provider.plan_control(field, value)
        restoration = lease.provider.plan_restore(prior, proposal)
        if restoration.values != prior.values:
            raise ValueError('restore plan changes prior values')
        for plan in (prior, proposal, restoration):
            if type(plan) is not ControlPlan:
                raise ValueError('typed control plan required')
            self._validate_observations(plan.observations)
            self.publisher.check_control(slot, ControlReadback(handle, field, plan.values))
        planned = time.monotonic()
        self._observe(lease.native, prior.observations, learn=True)
        self._timing = [started, planned, time.monotonic()]
        return PrepareControlOutcome(Outcome.CONFIRMED, PreparedControl(
            (lease, prior, proposal, restoration), handle, field, copy.deepcopy(prior.values)))

    def _apply(self, prepared, restore):
        owner, prior, proposal, restoration = prepared.token
        lease = self._owned(owner.provider.slot, prepared.handle)
        if owner is not lease:
            raise ValueError('control token belongs to another lease')
        plan = restoration if restore else proposal
        if plan.writes:
            try:
                # UA's host spreads long parameter pages over several calls; one SERVICE
                # takes at most 1016 command words, so split at whole WRITE-block boundaries.
                for chunk in _write_chunks(plan.writes):
                    self.driver.service(lease.native, writes=chunk)
            except OSError:
                self.invalidate()
                raise
        self._observe(lease.native, plan.observations)
        lease.provider.accept(plan)
        return ControlReadback(prepared.handle, prepared.field, copy.deepcopy(plan.values))

    def apply_control(self, prepared):
        applying = time.monotonic()
        readback = self._apply(prepared, False)
        applied = time.monotonic()
        coupled = self._coupled(prepared)
        done = time.monotonic()
        timing = getattr(self, '_timing', None)
        if timing and done - timing[0] > 0.25:
            log.info('slow native control %s: %.0f ms (plan %.0f, prior readback %.0f, wait %.0f, '
                     'apply %.0f, coupled %.0f)', prepared.field, (done - timing[0]) * 1e3,
                     (timing[1] - timing[0]) * 1e3, (timing[2] - timing[1]) * 1e3,
                     (applying - timing[2]) * 1e3, (applied - applying) * 1e3, (done - applied) * 1e3)
        return ApplyControlOutcome(Outcome.CONFIRMED, readback, coupled=coupled)

    def _coupled(self, prepared):
        """Other parameters the original setter moved with this one (e.g. a Sync switch that
        sets a rate): their provider values, where they differ from what is published."""
        slot = prepared.token[0].provider.slot
        current = getattr(self._owned(slot, prepared.handle).provider, 'current_values', None)
        if current is None or not prepared.field.startswith('parameters/'):
            return ()
        changed, index = [], 0
        while True:
            field = f'parameters/{index}/NormalizedValue'
            try:
                published = self.publisher.snapshot_control(slot, prepared.handle, field)
            except PublicationError:
                return tuple(changed)
            if field.split('/')[1] != prepared.field.split('/')[1]:
                values = current(field)
                if values != published.values:
                    changed.append(ControlReadback(prepared.handle, field, values))
            index += 1

    def restore_control(self, prepared):
        # A failed SERVICE invalidates PluginDriver; it cannot fabricate restore.
        return RestoreControlOutcome(Outcome.CONFIRMED, self._apply(prepared, True))


def load_provider(path):
    """Load an explicitly selected trusted local provider; no default search."""
    spec = importlib.util.spec_from_file_location('_openapollo_native_provider', path)
    if spec is None or spec.loader is None:
        raise ValueError('native provider is not a Python module')
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    factory = getattr(module, 'create_providers', None)
    return factory() if factory is not None else module.create_provider()


def register_native(state, helper, hardware, provider):
    """Borrow the existing hardware fd; never reconnect or restore saved leases."""
    from hardware import (DRIVER_PARAM_SAMPLE_RATE, UA_FEAT2_PLUGIN_MULTI_INPUT,
                          UA_FEAT2_PLUGIN_STEREO, UA_FEAT2_PLUGIN_SWAP,
                          UA_FEAT2_PLUGIN_CHAIN, UA_FEAT2_PLUGIN_DELAY_COMP,
                          UA_FEAT2_PLUGIN_UNISON, UA_FEAT2_PLUGIN_ANALOG_LEASE,
                          UA_FEAT2_PLUGIN_START_STAGED)

    if helper is None or not hardware.connected or hardware.fd < 0:
        raise ValueError('native hosting needs live PCIe hardware and helper tree')
    fd = hardware.fd
    rate = hardware.get_driver_param(DRIVER_PARAM_SAMPLE_RATE)
    providers = provider if isinstance(provider, (tuple, list)) else (provider,)
    if (not providers and not getattr(provider, 'growable', False)) or any(
            type(p.sample_rate) is not int or p.sample_rate != rate for p in providers):
        raise ValueError('native provider sample rate does not match hardware')
    features = hardware.features2()
    if type(features) is not int:
        raise ValueError("driver feature word required")
    publisher = Publisher(state, helper, owner=uuid.uuid4().hex)
    backend_type = NativePluginBackend
    options = {}
    composer = getattr(provider, 'composer', None)
    if callable(composer) and features & UA_FEAT2_PLUGIN_CHAIN and features & UA_FEAT2_PLUGIN_SWAP:
        from plugin_native_chain import NativeChainBackend
        backend_type = NativeChainBackend
        options['composer'] = composer
        options['compensation_supported'] = bool(features & UA_FEAT2_PLUGIN_DELAY_COMP)
        options['start_staged'] = bool(features & UA_FEAT2_PLUGIN_START_STAGED)
        options['unison'] = bool(features & UA_FEAT2_PLUGIN_UNISON
                                 and features & UA_FEAT2_PLUGIN_ANALOG_LEASE
                                 and getattr(provider, 'unison_supported', False))
    backend = backend_type(publisher, PluginDriver(fd), provider, **options,
        session_valid=lambda: hardware.connected and hardware.fd == fd
        and hardware.get_driver_param(DRIVER_PARAM_SAMPLE_RATE) == rate,
        max_instances=4 if features & UA_FEAT2_PLUGIN_MULTI_INPUT else 1,
        stereo=bool(features & UA_FEAT2_PLUGIN_STEREO),
        replacement=bool(features & UA_FEAT2_PLUGIN_SWAP))
    if getattr(backend, 'compensation_supported', False):
        setting = state.get_value('/MaxDelayComp')
        result = backend.set_compensation(setting)
        if result.status is not Outcome.CONFIRMED:
            backend.invalidate()
            raise ValueError('initial native compensation could not be confirmed: '
                             + (getattr(result, 'detail', '') or result.status.name))
    return backend, PluginTransactions(publisher, backend)
