"""Injectable plugin replacement coordinator; no live backend is registered.

Backend confirmations are internal adapter attestations, not evidence supplied
by Console. A real adapter must own the DSP allocation and all parameters, retain
its old lease through publication, and prove retirement independently of this
coordinator. No handles, DSP results, or parameters are generated.
"""

import copy
import logging
from contextlib import contextmanager
from dataclasses import dataclass
from enum import Enum
from threading import RLock, get_ident
from typing import Any, Protocol
from plugin_publication import (
    LoadedPlugin,
    OwnedHandle,
    Publisher,
    PublicationError,
)



log = logging.getLogger(__name__)

class Outcome(Enum):
    CONFIRMED = "confirmed"
    FAILED = "failed"
    INDETERMINATE = "indeterminate"


@dataclass(frozen=True)
class PreparedPlugin:
    token: Any
    loaded: LoadedPlugin
    previous: OwnedHandle | None = None


@dataclass(frozen=True)
class PrepareOutcome:
    status: Outcome
    prepared: PreparedPlugin | None = None
    detail: str = ""


@dataclass(frozen=True)
class ActivateOutcome:
    status: Outcome
    previous_retained: bool = False
    detail: str = ""


@dataclass(frozen=True)
class RollbackOutcome:
    status: Outcome
    previous_restored: bool = False
    candidate_retired: bool = False
    detail: str = ""


@dataclass(frozen=True)
class FinalizeOutcome:
    status: Outcome
    detail: str = ""


@dataclass(frozen=True)
class RetireOutcome:
    """Retirement attestation; native unload return0 alone is insufficient.

    FAILED requires previous_retained=True and retired=False (no change).
    CONFIRMED requires retired=True and previous_retained=False. The adapter
    must establish terminal DMA/port/instance retirement for the exact lease.
    """

    status: Outcome
    handle: OwnedHandle
    previous_retained: bool = False
    retired: bool = False
    detail: str = ""


@dataclass(frozen=True)
class PreparedControl:
    """Side-effect-free token and exact prior runtime readback for an owned lease."""

    token: Any
    handle: OwnedHandle
    field: str
    current_values: dict


@dataclass(frozen=True)
class PrepareControlOutcome:
    status: Outcome
    prepared: PreparedControl | None = None
    detail: str = ""


@dataclass(frozen=True)
class ApplyControlOutcome:
    status: Outcome
    readback: Any = None
    unchanged: bool = False
    detail: str = ""
    coupled: tuple = ()  # ControlReadbacks of other parameters the setter moved


@dataclass(frozen=True)
class RestoreControlOutcome:
    status: Outcome
    readback: Any = None
    detail: str = ""


class TransactionalPluginBackend(Protocol):
    """Prepare FAILED with no token attests no acquired resources.

    Any acquired/partially acquired candidate MUST return its token even on
    failure. activate CONFIRMED attests the candidate is active and the previous
    allocation remains owned/reversible. rollback CONFIRMED must attest BOTH
    previous restoration and candidate retirement. finalize CONFIRMED attests
    previous retirement after publication. Timeouts/disconnects without proof
    are INDETERMINATE, never confirmations. Token cleanup is backend-owned.
    """

    def prepare(self, slot: str, name: str) -> PrepareOutcome: ...
    def activate(self, prepared: PreparedPlugin) -> ActivateOutcome: ...
    def rollback(self, prepared: PreparedPlugin) -> RollbackOutcome: ...
    def finalize(self, prepared: PreparedPlugin) -> FinalizeOutcome: ...


@dataclass(frozen=True)
class TransactionResult:
    error: str | None = None
    detail: str = ""
    committed: bool = False
    events: tuple = ()
    quarantined: bool = False



def slot_views(publisher, slot):
    """Publication serial plus this slot's subtree in both views. A control transaction can
    run beside the daemon's event loop, which keeps the rest of the tree current; only a
    publication or an edit to this slot invalidates it."""
    node, _ = publisher.state._resolve_path(slot)
    helper = publisher.helper.root
    for part in slot.strip("/").split("/"):
        helper = next(
            (c for c in helper.get("children", []) if c.get("path") == part), None
        )
        if helper is None:
            break
    return copy.deepcopy(
        (publisher.version, publisher.generation, node, helper)
    )

class PluginTransactions:
    """Replacement, confirmed retirement, and owned-lease control transactions.

    Calls serialize here, but all tree publishers must still use the daemon's
    single event-loop publication owner (creator-thread enforced here).
    Reentrant requests are rejected. Publisher's optimistic check is not a
    replacement for a global threading contract. Dispatch/persistence belong to
    that owner after a committed result, including retirement-error results.
    A revoked publisher session suppresses its stale events and quarantines
    retained ownership. committed then records a historical tree swap only;
    it does not certify a current device allocation or allow a reconnect.
    """

    def __init__(
        self,
        publisher: Publisher,
        backend: TransactionalPluginBackend,
        *,
        publication_fault=None,
    ):
        self.publisher = publisher
        self.backend = backend
        self.publication_fault = publication_fault
        # Runs each publication (stage + commit) on the thread that owns the views. The daemon
        # sets it to hop onto its event loop while a transaction runs on its worker thread.
        self.publish_on = lambda publish: publish()
        self.quarantined = False
        self.quarantine_reason = ""
        # Retain unresolved tokens; never silently forget potentially live DMA.
        self.unresolved: list[PreparedPlugin | OwnedHandle] = []
        self._lock = RLock()
        self._owner_thread = get_ident()
        self._busy = False
        self._current_leases = {}  # Only successful coordinator publication establishes authority.

    @contextmanager
    def owned_by_current_thread(self):
        """Run one operation on another thread (the daemon's native worker: a load runs the
        plug-in's code and reads back thousands of DSP words). Meanwhile the creator thread
        gets owner-mismatch refusals and no current lease, as during any busy transaction."""
        with self._lock:
            previous, self._owner_thread = self._owner_thread, get_ident()
        try:
            yield self
        finally:
            with self._lock:
                self._owner_thread = previous

    @property
    def remote_revision(self):
        """Publication serial fences numeric handle reuse within one session."""
        return self.publisher.version

    def current_remote_handle(self, slot):
        """Return an established current lease for readback, never acquire one.

        The read backend must pin this exact lease through its I/O. A second
        lookup after readback fences publication/session changes; it does not
        itself prove DMA completion or protect native allocation lifetime.
        """
        if get_ident() != self._owner_thread or not isinstance(slot, str):
            return None
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        with self._lock:
            p = self.publisher
            if self._busy or self.quarantined or not p.connected:
                return None
            lease = self._current_leases.get(slot)
            if (
                type(lease) is not OwnedHandle
                or lease.owner != p.owner
                or type(lease.generation) is not int
                or lease.generation != p.generation
            ):
                return None
            try:
                p.snapshot_control(slot, lease, "Power")
            except PublicationError:
                return None
            return lease

    def _quarantine(
        self, detail, prepared=None, *, committed=False, events=()
    ):
        if not self.quarantined:
            # Every later transaction answers with this until the daemon restarts (it closes the
            # device, which frees whatever the unconfirmed operation left): say so once, loudly.
            log.error("Native plug-in transactions stopped (quarantined): %s. Plug-ins stay "
                      "unavailable until the mixer daemon restarts.", detail)
        self.quarantined = True
        self.quarantine_reason = detail
        if prepared is not None:
            self.unresolved.append(prepared)
        return TransactionResult(
            "openapollo.plugin_transaction_indeterminate",
            detail,
            committed,
            tuple(events),
            True,
        )

    def _rollback(self, prepared, detail):
        try:
            result = self.backend.rollback(prepared)
        except Exception as exc:
            return self._quarantine(
                f"{detail}; rollback raised {type(exc).__name__}", prepared
            )
        if (
            not isinstance(result, RollbackOutcome)
            or result.status is not Outcome.CONFIRMED
            or result.previous_restored is not True
            or result.candidate_retired is not True
        ):
            return self._quarantine(
                f"{detail}; rollback/retirement unconfirmed", prepared
            )
        return TransactionResult(
            "openapollo.plugin_transaction_failed", detail
        )

    def _validate_identity(self, slot, name, prepared):
        p = self.publisher
        if not isinstance(prepared, PreparedPlugin) or prepared.token is None:
            raise PublicationError("backend-owned preparation token required")
        p.validate(prepared.loaded)
        if prepared.loaded.name != name:
            raise PublicationError(
                "backend result does not match requested effect"
            )
        current_name = p.state.get_value(slot + "/EffectName/value")
        current = p.state.get_value(slot + "/EffectInstance/value")
        helper_name = p.helper.get_value(slot + "/EffectName")
        helper_handle = p.helper.get_value(slot + "/EffectInstance")
        if current_name == "" and type(current) is int and current == 0:
            if prepared.previous is not None:
                raise PublicationError(
                    "unexpected previous lease for empty slot"
                )
            if not (
                (helper_name is None and helper_handle is None)
                or (
                    isinstance(helper_name, str)
                    and helper_name == ""
                    and type(helper_handle) is int
                    and helper_handle == 0
                )
            ):
                raise PublicationError("empty slot views disagree")
        else:
            previous = prepared.previous
            if (
                not isinstance(current_name, str)
                or not current_name
                or type(current) is not int
                or current <= 0
                or not isinstance(previous, OwnedHandle)
                or type(previous.value) is not int
                or not 0 < previous.value <= 0xFFFFFFFFFFFFFFFF
                or previous.value != current
                or previous.owner != p.owner
                or type(previous.generation) is not int
                or previous.generation != p.generation
                or type(helper_handle) is not int
                or (helper_name, helper_handle) != (current_name, current)
            ):
                raise PublicationError(
                    "current owned identity must agree in both views"
                )
            if prepared.loaded.handle.value == current:
                raise PublicationError(
                    "replacement must retain a distinct previous lease"
                )

    def load(self, slot: str, name: str) -> TransactionResult:
        if get_ident() != self._owner_thread:
            return TransactionResult(
                "openapollo.plugin_publication_owner_mismatch",
                "Plugin publication must run on its creator thread",
            )
        with self._lock:
            if self._busy:
                return TransactionResult(
                    "openapollo.plugin_transaction_reentrant",
                    "A plugin transaction is already in progress",
                )
            self._busy = True
            try:
                return self._load(slot, name)
            finally:
                self._busy = False

    def _load(self, slot: str, name: str) -> TransactionResult:
        if self.quarantined:
            return TransactionResult(
                "openapollo.plugin_transaction_indeterminate",
                self.quarantine_reason,
                quarantined=True,
            )
        if not self.publisher.connected:
            return TransactionResult(
                "openapollo.plugin_backend_unavailable",
                "Publisher disconnected",
            )
        if not isinstance(name, str) or not name:
            return TransactionResult("openapollo.invalid_plugin_value")
        # Reject non-slots BEFORE calling a potentially side-effectful backend.
        from plugin_lifecycle import is_plugin_slot_path

        if (
            not isinstance(slot, str)
            or not is_plugin_slot_path(slot + "/EffectName")
            or not self.publisher.state.path_exists(slot)
        ):
            return TransactionResult("openapollo.invalid_plugin_slot")
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        try:
            outcome = self.backend.prepare(slot, name)
        except Exception as exc:
            return self._quarantine(
                f"Preparation raised {type(exc).__name__}; ownership unknown"
            )
        if not isinstance(outcome, PrepareOutcome):
            return self._quarantine(
                "Invalid preparation outcome; ownership unknown"
            )
        prepared = outcome.prepared
        if outcome.status is not Outcome.CONFIRMED:
            if isinstance(prepared, PreparedPlugin):
                return self._rollback(
                    prepared, outcome.detail or "Preparation failed"
                )
            if outcome.status is Outcome.FAILED and prepared is None:
                return TransactionResult(
                    "openapollo.plugin_transaction_failed", outcome.detail
                )
            return self._quarantine("Preparation ownership unconfirmed")
        if not isinstance(prepared, PreparedPlugin):
            return self._quarantine(
                "Confirmed preparation lacks cleanup token"
            )
        try:
            self._validate_identity(slot, name, prepared)
            transaction = self.publisher.stage(slot, prepared.loaded)
            sealed_identity = copy.deepcopy(
                (prepared.loaded, prepared.previous)
            )
        except Exception as exc:
            return self._rollback(
                prepared,
                f"Publication validation failed: {type(exc).__name__}",
            )
        try:
            activated = self.backend.activate(prepared)
        except Exception as exc:
            return self._rollback(
                prepared, f"Activation raised {type(exc).__name__}"
            )
        if (
            not isinstance(activated, ActivateOutcome)
            or activated.status is not Outcome.CONFIRMED
            or activated.previous_retained is not True
        ):
            return self._rollback(
                prepared, "Activation or previous ownership unconfirmed"
            )
        if (prepared.loaded, prepared.previous) != sealed_identity:
            return self._rollback(
                prepared, "Prepared identity changed during activation"
            )
        try:
            # Stage again against the views as they are now: activation can take seconds
            # beside the daemon's event loop, which keeps its views current meanwhile.
            # Validation already passed before activating.
            events = self.publish_on(
                lambda: self.publisher.stage(slot, prepared.loaded).commit(
                    fault=self.publication_fault
                )
            )
        except Exception as exc:
            return self._rollback(
                prepared, f"Publication failed: {type(exc).__name__}"
            )
        self._current_leases[slot] = copy.deepcopy(prepared.loaded.handle)
        session = (
            prepared.loaded.handle.owner,
            prepared.loaded.handle.generation,
        )

        def session_current():
            p = self.publisher
            return p.connected and (p.owner, p.generation) == session

        if not session_current():
            # Publication may already have occurred; never finalize an old
            # device session or dispatch its now stale success notifications.
            return self._quarantine(
                "Published; backend session invalidated",
                prepared,
                committed=True,
            )
        try:
            finalized = self.backend.finalize(prepared)
        except Exception as exc:
            return self._quarantine(
                f"Published; retirement raised {type(exc).__name__}",
                prepared,
                committed=True,
                events=events if session_current() else (),
            )
        if not session_current():
            return self._quarantine(
                "Published; backend session invalidated during retirement",
                prepared,
                committed=True,
            )
        if (
            not isinstance(finalized, FinalizeOutcome)
            or finalized.status is not Outcome.CONFIRMED
        ):
            return self._quarantine(
                "Published; previous retirement unconfirmed",
                prepared,
                committed=True,
                events=events,
            )
        return TransactionResult(committed=True, events=tuple(events))

    def unload(self, slot: str) -> TransactionResult:
        """Publish empty only after exact owned allocation retires conclusively."""
        if get_ident() != self._owner_thread:
            return TransactionResult(
                "openapollo.plugin_publication_owner_mismatch"
            )
        with self._lock:
            if self._busy:
                return TransactionResult(
                    "openapollo.plugin_transaction_reentrant"
                )
            self._busy = True
            try:
                return self._unload(slot)
            finally:
                self._busy = False

    def _unload(self, slot):
        if self.quarantined:
            return TransactionResult(
                "openapollo.plugin_transaction_indeterminate",
                self.quarantine_reason,
                quarantined=True,
            )
        p = self.publisher
        if not p.connected:
            return TransactionResult("openapollo.plugin_backend_unavailable")
        from plugin_lifecycle import is_plugin_slot_path

        if (
            not isinstance(slot, str)
            or not is_plugin_slot_path(slot + "/EffectName")
            or not p.state.path_exists(slot)
        ):
            return TransactionResult("openapollo.invalid_plugin_slot")
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        retire = getattr(self.backend, "retire", None)
        if not callable(retire):
            return TransactionResult(
                "openapollo.plugin_backend_unavailable",
                "Backend does not implement confirmed retirement",
            )
        name = p.state.get_value(slot + "/EffectName")
        value = p.state.get_value(slot + "/EffectInstance")
        if (
            type(name) is not str
            or not name
            or type(value) is not int
            or not 0 < value <= 0xFFFFFFFFFFFFFFFF
            or type(p.generation) is not int
            or type(p.helper.get_value(slot + "/EffectInstance")) is not int
            or (
                p.helper.get_value(slot + "/EffectName"),
                p.helper.get_value(slot + "/EffectInstance"),
            )
            != (name, value)
        ):
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                "Current owned identity must agree in both views",
            )
        lease = OwnedHandle(value, p.owner, p.generation)
        try:
            transaction = p.stage(slot, None)
        except Exception as exc:
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                f"Empty publication staging failed: {type(exc).__name__}",
            )
        try:
            outcome = retire(slot, lease)
        except Exception as exc:
            return self._quarantine(
                f"Retirement raised {type(exc).__name__}", lease
            )
        if (
            not isinstance(outcome, RetireOutcome)
            or not isinstance(outcome.handle, OwnedHandle)
            or type(outcome.handle.value) is not int
            or type(outcome.handle.generation) is not int
            or outcome.handle != lease
        ):
            return self._quarantine(
                "Exact retirement ownership unconfirmed", lease
            )
        if (
            outcome.status is Outcome.FAILED
            and outcome.previous_retained is True
            and outcome.retired is False
        ):
            return TransactionResult(
                "openapollo.plugin_transaction_failed", outcome.detail
            )
        if (
            outcome.status is not Outcome.CONFIRMED
            or outcome.retired is not True
            or outcome.previous_retained is not False
        ):
            return self._quarantine("Terminal retirement unconfirmed", lease)
        self._current_leases.pop(slot, None)
        try:
            # Stage again: retirement can run beside the daemon's event loop.
            events = self.publish_on(
                lambda: p.stage(slot, None).commit(fault=self.publication_fault)
            )
        except Exception as exc:
            # Hardware is already retired. Never claim rollback to the old
            # allocation just because its stale tree could not be cleared.
            return self._quarantine(
                f"Retired; empty publication failed: {type(exc).__name__}",
                lease,
            )
        if not p.connected or (p.owner, p.generation) != (
            lease.owner,
            lease.generation,
        ):
            return self._quarantine(
                "Retired; backend session invalidated during publication",
                lease,
                committed=True,
            )
        return TransactionResult(committed=True, events=tuple(events))

    def control(self, slot, field, value):
        """Apply only through a coordinator-owned lease and authoritative readback.

        Backend prepare_control MUST be side-effect-free, including failures.
        apply_control is the sole mutation point. Its return is not readback proof
        by itself: adapters must attest device/session/parameter correspondence.
        restore_control must conclusively restore the exact prior values; an
        unknown result quarantines ownership and blocks subsequent operations.
        """
        if get_ident() != self._owner_thread:
            return TransactionResult(
                "openapollo.plugin_publication_owner_mismatch"
            )
        with self._lock:
            if self._busy:
                return TransactionResult(
                    "openapollo.plugin_transaction_reentrant"
                )
            self._busy = True
            try:
                return self._control(slot, field, value)
            finally:
                self._busy = False

    def _control(self, slot, field, value):
        from plugin_publication import ControlReadback
        import math
        import re

        p = self.publisher
        if self.quarantined:
            return TransactionResult(
                "openapollo.plugin_transaction_indeterminate",
                self.quarantine_reason,
                quarantined=True,
            )
        if not p.connected:
            return TransactionResult("openapollo.plugin_backend_unavailable")
        if not isinstance(slot, str) or not isinstance(field, str):
            return TransactionResult("openapollo.invalid_plugin_value")
        slot = "/" + "/".join(part for part in slot.split("/") if part)
        if field == "Power":
            valid = type(value) is bool
        elif re.fullmatch(r"parameters/\d+/NormalizedValue", field):
            try:
                valid = (
                    type(value) in (int, float)
                    and math.isfinite(value)
                    and 0 <= value <= 1
                )
            except OverflowError:
                valid = False
        elif re.fullmatch(r"parameters/\d+/StepValue", field):
            valid = type(value) in (str, int)
        else:
            valid = False
        if not valid:
            return TransactionResult("openapollo.invalid_plugin_value")
        lease = self._current_leases.get(slot)
        if lease is None or (lease.owner, lease.generation) != (
            p.owner,
            p.generation,
        ):
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                "No coordinator-owned current lease",
            )
        try:
            observed_prior = p.snapshot_control(slot, lease, field)
            meta = p.state.get(slot + "/" + field)
            if isinstance(meta, dict) and meta.get("read_only") is True:
                return TransactionResult("openapollo.plugin_read_only")
            if field.endswith("/StepValue"):
                if (
                    not isinstance(meta, dict)
                    or (meta.get("type") == "int" and type(value) is not int)
                    or (
                        meta.get("type") == "string" and type(value) is not str
                    )
                    or meta.get("type") not in ("int", "string")
                    or value not in meta.get("values", [])
                ):
                    return TransactionResult("openapollo.invalid_plugin_value")
            before = slot_views(p, slot)
        except Exception as exc:
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                f"Current control invalid: {type(exc).__name__}",
            )
        methods = [
            getattr(self.backend, name, None)
            for name in ("prepare_control", "apply_control", "restore_control")
        ]
        if not all(callable(method) for method in methods):
            return TransactionResult(
                "openapollo.plugin_backend_unavailable",
                "Backend does not implement confirmed control transactions",
            )

        def same_session():
            return p.connected and (p.owner, p.generation) == (
                lease.owner,
                lease.generation,
            )

        def exact_handle(handle):
            return (
                isinstance(handle, OwnedHandle)
                and type(handle.value) is int
                and type(handle.generation) is int
                and handle == lease
            )

        try:
            outcome = methods[0](
                slot, copy.deepcopy(lease), field, copy.deepcopy(value)
            )
        except Exception as exc:
            return TransactionResult(
                "openapollo.plugin_transaction_failed",
                f"Side-effect-free control preparation raised {type(exc).__name__}",
            )
        if (
            not isinstance(outcome, PrepareControlOutcome)
            or outcome.status is not Outcome.CONFIRMED
        ):
            return TransactionResult(
                "openapollo.plugin_transaction_failed",
                "Side-effect-free control preparation not confirmed",
            )
        prepared = outcome.prepared
        try:
            if (
                not isinstance(prepared, PreparedControl)
                or prepared.token is None
                or not exact_handle(prepared.handle)
                or prepared.field != field
            ):
                raise PublicationError("Prepared control identity mismatch")
            prior = ControlReadback(
                copy.deepcopy(lease),
                field,
                copy.deepcopy(prepared.current_values),
            )
            if prior != observed_prior:
                raise PublicationError(
                    "Prepared prior values disagree with published state"
                )
            p.stage_control(
                slot, prior
            )  # Validate prior schema and both views before any write.
            sealed = copy.deepcopy(
                (prepared.handle, prepared.field, prepared.current_values)
            )
        except Exception as exc:
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                f"Control preparation invalid: {type(exc).__name__}",
            )

        def restore(detail):
            try:
                restore_prepared = PreparedControl(
                    prepared.token,
                    copy.deepcopy(prior.handle),
                    prior.field,
                    copy.deepcopy(prior.values),
                )
                restored = methods[2](restore_prepared)
                if (
                    not isinstance(restored, RestoreControlOutcome)
                    or restored.status is not Outcome.CONFIRMED
                    or not isinstance(restored.readback, ControlReadback)
                    or not exact_handle(restored.readback.handle)
                    or restored.readback != prior
                    or not same_session()
                ):
                    raise PublicationError(
                        "Exact prior restoration unconfirmed"
                    )
                p.stage_control(slot, restored.readback)
                if before != slot_views(p, slot):
                    raise PublicationError(
                        "Publication changed during control/restore"
                    )
            except Exception as exc:
                return self._quarantine(
                    f"{detail}; restore unconfirmed: {type(exc).__name__}",
                    prepared,
                )
            return TransactionResult(
                "openapollo.plugin_transaction_failed", detail
            )

        def views_unchanged():
            return before == slot_views(p, slot)

        if not same_session() or not views_unchanged():
            return TransactionResult(
                "openapollo.plugin_publication_unavailable",
                "Publication changed during side-effect-free preparation",
            )
        try:
            applied = methods[1](prepared)
        except Exception as exc:
            return restore(f"Control apply raised {type(exc).__name__}")
        if (
            prepared.handle,
            prepared.field,
            prepared.current_values,
        ) != sealed:
            return restore("Prepared control changed during apply")
        if not same_session():
            return self._quarantine(
                "Control session invalidated during apply", prepared
            )
        if not views_unchanged():
            return restore("Publication changed during control application")
        if (
            not isinstance(applied, ApplyControlOutcome)
            or applied.status is not Outcome.CONFIRMED
        ):
            if (
                isinstance(applied, ApplyControlOutcome)
                and applied.status is Outcome.FAILED
                and applied.unchanged is True
                and isinstance(applied.readback, ControlReadback)
                and exact_handle(applied.readback.handle)
                and applied.readback == prior
            ):
                return TransactionResult(
                    "openapollo.plugin_transaction_failed", applied.detail
                )
            return restore("Control application unconfirmed")
        try:
            if (
                not isinstance(applied.readback, ControlReadback)
                or not exact_handle(applied.readback.handle)
                or applied.readback.field != field
            ):
                raise PublicationError("Control readback identity mismatch")
            for coupled in applied.coupled:
                if (
                    not isinstance(coupled, ControlReadback)
                    or not exact_handle(coupled.handle)
                    or coupled.field == field
                ):
                    raise PublicationError("Coupled readback identity mismatch")

            def publish():
                events = list(
                    p.stage_control(slot, applied.readback).commit(
                        fault=self.publication_fault
                    )
                )
                for coupled in applied.coupled:
                    events += p.stage_control(slot, coupled).commit(
                        fault=self.publication_fault
                    )
                return events

            events = self.publish_on(publish)
        except Exception as exc:
            return restore(f"Control publication failed: {type(exc).__name__}")
        if not same_session():
            return self._quarantine(
                "Control published; session invalidated",
                prepared,
                committed=True,
            )
        return TransactionResult(committed=True, events=tuple(events))
