"""Opt-in single-insert adapter; all effect data comes from a private provider.

The provider is trusted local code, explicitly loaded by the owner. It plans
writes and formats verified values; this adapter owns every device operation.
"""

import copy
from dataclasses import dataclass
import importlib.util
import re
import sys
import uuid

from plugin_remote import RemoteReadback
from plugin_driver import PluginDriver
from plugin_publication import ControlReadback, LoadedPlugin, OwnedHandle, Publisher
from plugin_transactions import (
    ActivateOutcome, ApplyControlOutcome, FinalizeOutcome, Outcome,
    PluginTransactions, PreparedControl, PreparedPlugin, PrepareControlOutcome,
    PrepareOutcome, RestoreControlOutcome, RetireOutcome, RollbackOutcome,
)


@dataclass(frozen=True)
class ControlPlan:
    writes: tuple
    observations: tuple  # (private row, offset, exact expected uint32 tuple)
    values: dict
    state: object


@dataclass
class _Lease:
    native: object
    loaded: LoadedPlugin


class NativePluginBackend:
    """One configured mono input slot, with no replacement or reconnect replay."""

    native_single_insert = True

    def __init__(self, publisher, driver, provider, *, session_valid=lambda: True):
        self.publisher, self.driver, self.provider = publisher, driver, provider
        self.session_valid = session_valid
        match = re.fullmatch(r'/devices/\d+/inputs/(\d+)/effects/0', provider.slot)
        if not match or not isinstance(provider.name, str) or not provider.name:
            raise ValueError('one named mono input effect-zero slot required')
        self.input = int(match[1])
        self._lease = None

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
        lease = self._lease
        if slot != self.provider.slot or lease is None or lease.loaded.handle != handle:
            raise ValueError('exact current native lease required')
        return lease

    @staticmethod
    def _validate_observations(observations):
        if type(observations) is not tuple or not 1 <= len(observations) <= 256:
            raise ValueError('bounded authoritative observation plan required')
        for row, offset, expected in observations:
            if (type(row) is not int or not 0 <= row < 128
                    or type(offset) is not int or not 0 <= offset < 1 << 24
                    or type(expected) is not tuple or not 1 <= len(expected) <= 64
                    or any(type(word) is not int or not 0 <= word <= 0xFFFFFFFF
                           for word in expected)):
                raise ValueError('invalid authoritative observation')

    def _observe(self, native, observations):
        self._validate_observations(observations)
        for row, offset, expected in observations:
            try:
                actual = self.driver.service(native, read=(row, offset, len(expected)))
            except OSError:
                self.invalidate()
                raise
            if actual != expected:
                raise ValueError('native coefficient observation differs')
        self._session()

    def read(self, slot, handle, query):
        """Fresh scalar reads from a provider allowlist; no remote writes/opcodes."""
        lease = self._owned(slot, handle)
        if query.kind not in ('ints', 'floats') or not 1 <= len(query.ids) <= 64:
            raise ValueError('only bounded authorized scalar reads are supported')
        mapping = self.provider.remote_reads
        reads = []
        for variable in query.ids:
            location = mapping.get((query.kind, variable))
            if type(location) is not tuple or len(location) != 2:
                raise ValueError('remote variable has no authorized native mapping')
            row, offset = location
            self._validate_observations(((row, offset, (0,)),))
            reads.append((row, offset, 1))
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

    def prepare(self, slot, name):
        if slot != self.provider.slot or name != self.provider.name or self._lease:
            return PrepareOutcome(Outcome.FAILED, detail='Only the configured empty insert is supported')
        if self.publisher.state.get_value(slot + '/EffectName') != '' or \
                self.publisher.state.get_value(slot + '/EffectInstance') != 0:
            return PrepareOutcome(Outcome.FAILED, detail='Replacement is unsupported')
        try:
            self._session()
            self.provider.reset()
            initial = self.provider.initial(OwnedHandle(
                1, self.publisher.owner, self.publisher.generation))
            self.publisher.validate(initial)
            if initial.name != name:
                raise ValueError('provider name differs')
            arguments = self.provider.create_arguments()
            if (tuple(arguments.get('input_pair', ())) != (self.input, 32)
                    or type(arguments.get('dsp')) is not int
                    or not 0 <= arguments['dsp'] < 8):
                raise ValueError('explicit matching mono input and DSP required')
            observations = self.provider.observations()
            self._validate_observations(observations)
        except Exception:
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
        self._lease = lease = _Lease(native, loaded)
        prepared = PreparedPlugin(lease, loaded)
        try:
            self._observe(native, observations)
        except Exception:
            return PrepareOutcome(Outcome.FAILED, prepared, 'Initial readback unconfirmed')
        return PrepareOutcome(Outcome.CONFIRMED, prepared)

    def activate(self, prepared):
        lease = self._owned(self.provider.slot, prepared.loaded.handle)
        if prepared.token is not lease or prepared.previous is not None:
            return ActivateOutcome(Outcome.FAILED)
        # CREATE F_START already started the exact readback-verified instance.
        return ActivateOutcome(Outcome.CONFIRMED, previous_retained=True)

    def rollback(self, prepared):
        if prepared.token is not self._lease:
            return RollbackOutcome(Outcome.INDETERMINATE)
        try:
            self.driver.destroy(self._lease.native)
        except Exception:
            self.invalidate()
            return RollbackOutcome(Outcome.INDETERMINATE)
        self._lease = None
        return RollbackOutcome(Outcome.CONFIRMED, True, True)

    def finalize(self, prepared):
        self._owned(self.provider.slot, prepared.loaded.handle)
        return FinalizeOutcome(Outcome.CONFIRMED)

    def retire(self, slot, handle):
        lease = self._owned(slot, handle)
        try:
            self.driver.destroy(lease.native)
        except Exception:
            self.invalidate()
            return RetireOutcome(Outcome.INDETERMINATE, handle)
        self._lease = None
        return RetireOutcome(Outcome.CONFIRMED, handle, retired=True)

    def prepare_control(self, slot, handle, field, value):
        lease = self._owned(slot, handle)
        prior = self.provider.snapshot(field)
        proposal = self.provider.plan_control(field, value)
        restoration = self.provider.plan_restore(prior, proposal)
        if restoration.values != prior.values:
            raise ValueError('restore plan changes prior values')
        for plan in (prior, proposal, restoration):
            if type(plan) is not ControlPlan:
                raise ValueError('typed control plan required')
            self._validate_observations(plan.observations)
            self.publisher.stage_control(slot, ControlReadback(handle, field, plan.values))
        self._observe(lease.native, prior.observations)
        return PrepareControlOutcome(Outcome.CONFIRMED, PreparedControl(
            (lease, prior, proposal, restoration), handle, field, copy.deepcopy(prior.values)))

    def _apply(self, prepared, restore):
        lease = self._owned(self.provider.slot, prepared.handle)
        owner, prior, proposal, restoration = prepared.token
        if owner is not lease:
            raise ValueError('control token belongs to another lease')
        plan = restoration if restore else proposal
        if plan.writes:
            try:
                self.driver.service(lease.native, writes=plan.writes)
            except OSError:
                self.invalidate()
                raise
        self._observe(lease.native, plan.observations)
        self.provider.accept(plan)
        return ControlReadback(prepared.handle, prepared.field, copy.deepcopy(plan.values))

    def apply_control(self, prepared):
        return ApplyControlOutcome(Outcome.CONFIRMED, self._apply(prepared, False))

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
    return module.create_provider()


def register_native(state, helper, hardware, provider):
    """Borrow the existing hardware fd; never reconnect or restore saved leases."""
    from hardware import DRIVER_PARAM_SAMPLE_RATE

    if helper is None or not hardware.connected or hardware.fd < 0:
        raise ValueError('native hosting needs live PCIe hardware and helper tree')
    fd = hardware.fd
    rate = hardware.get_driver_param(DRIVER_PARAM_SAMPLE_RATE)
    if type(provider.sample_rate) is not int or provider.sample_rate != rate:
        raise ValueError('native provider sample rate does not match hardware')
    publisher = Publisher(state, helper, owner=uuid.uuid4().hex)
    backend = NativePluginBackend(publisher, PluginDriver(fd), provider,
        session_valid=lambda: hardware.connected and hardware.fd == fd
        and hardware.get_driver_param(DRIVER_PARAM_SAMPLE_RATE) == rate)
    return backend, PluginTransactions(publisher, backend)
