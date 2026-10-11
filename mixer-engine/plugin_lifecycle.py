"""Fail-closed plugin boundary. openapollo.* errors are local, not UA codes.

This first phase deliberately publishes no successful allocation. A later backend
must publish its authoritative tree transactionally before enabling success here.
"""

from dataclasses import dataclass
import logging
import math
import re
from typing import Protocol

log = logging.getLogger(__name__)
_SLOT = re.compile(
    r"^(?P<slot>/devices/\d+/(?:(?:inputs|auxs|outputs)/\d+)(?:/preamps/\d+)?/effects/\d+)/(?P<field>.+?)(?:/value)?$"
)


def _canonical_path(path: str) -> str:
    # Match StateTree path traversal, including repeated/trailing separators.
    return "/" + "/".join(part for part in path.split("/") if part)


def is_plugin_slot_path(path: str) -> bool:
    """Recognize effect slot property paths without inspecting state or hardware."""
    return (
        isinstance(path, str)
        and _SLOT.fullmatch(_canonical_path(path)) is not None
    )


def plugin_slot_from_path(path: str) -> str | None:
    """Resolve an effect property path without inventing plugin identity."""
    match = (
        _SLOT.fullmatch(_canonical_path(path))
        if isinstance(path, str)
        else None
    )
    return match.group("slot") if match else None


@dataclass(frozen=True)
class PluginResult:
    error: str | None = None
    detail: str = ""
    committed: bool = False
    events: tuple = ()


class PluginBackend(Protocol):
    """Phase 1 diagnostics only: every operation MUST be side-effect-free.

    No real transactional backend can be registered until a publication and
    rollback contract is implemented. This explicit opt-in is for test/diagnostic
    adapters, not a claim that DSP loading is supported.
    """

    diagnostic_only: bool

    def load(self, slot: str, name: str) -> PluginResult: ...
    def unload(self, slot: str) -> PluginResult: ...
    def bypass(self, slot: str, power: bool) -> PluginResult: ...
    def set_parameter(
        self, slot: str, parameter: str, value
    ) -> PluginResult: ...


class UnavailablePluginBackend:
    diagnostic_only = True

    def _unavailable(self, *args):
        return PluginResult(
            "openapollo.plugin_backend_unavailable",
            "No native plugin loader is connected",
        )

    load = unload = bypass = set_parameter = _unavailable


class PluginLifecycle:
    def __init__(
        self,
        state,
        backend: PluginBackend | None = None,
        *,
        helper_tree=None,
        transactions=None,
    ):
        self.state = state
        self.helper_tree = helper_tree
        self.backend = (
            backend if backend is not None else UnavailablePluginBackend()
        )
        self.transactions = transactions
        if transactions is not None:
            from plugin_transactions import PluginTransactions

            if (
                not isinstance(transactions, PluginTransactions)
                or transactions.publisher.state is not state
                or transactions.publisher.helper is not helper_tree
            ):
                raise ValueError(
                    "Plugin transaction publisher must own the daemon state views"
                )

    def handle(self, path, value):
        """None means unrelated SET; PluginResult means intercepted, never mutate."""
        match = _SLOT.fullmatch(_canonical_path(path))
        if not match:
            return None
        slot, field = match.group("slot", "field")
        if not self.state.path_exists(slot):
            return PluginResult("openapollo.invalid_plugin_slot")
        if self.transactions is not None and self.transactions.quarantined:
            return PluginResult(
                "openapollo.plugin_transaction_indeterminate",
                self.transactions.quarantine_reason,
            )
        if field == "EffectName":
            if not isinstance(value, str):
                return PluginResult("openapollo.invalid_plugin_value")
            current = self.state.get_value(slot + "/EffectName/value")
            instance = self.state.get_value(slot + "/EffectInstance/value")
            # Only the captured explicit empty-name/zero-pointer shape proves
            # that removal is already satisfied. Missing/false metadata is
            # unknown state, not evidence that a backend instance is absent.
            if value == "" and (
                not isinstance(current, str)
                or type(instance) is not int
                or instance < 0
            ):
                return PluginResult(
                    "openapollo.plugin_publication_unavailable",
                    "Plugin slot identity metadata is incomplete or invalid",
                )
            if value == "" and current == "" and instance == 0:
                if self.helper_tree is not None:
                    helper_name = self.helper_tree.get_value(
                        slot + "/EffectName"
                    )
                    helper_instance = self.helper_tree.get_value(
                        slot + "/EffectInstance"
                    )
                    # An absent helper slot is allowed; a readable slot must
                    # carry the same explicit empty identity as the state view.
                    helper_absent = (
                        helper_name is None and helper_instance is None
                    )
                    helper_empty = (
                        helper_name == ""
                        and isinstance(helper_name, str)
                        and type(helper_instance) is int
                        and helper_instance == 0
                    )
                    if not helper_absent and not helper_empty:
                        return PluginResult(
                            "openapollo.plugin_publication_unavailable",
                            "State and helper plugin slot views disagree",
                        )
                return PluginResult()  # Empty removal is already satisfied.
            operation = "load" if value else "unload"
            args = (slot, value) if value else (slot,)
            if self.transactions is not None:
                result = (
                    self.transactions.load(slot, value)
                    if value
                    else self.transactions.unload(slot)
                )
                return PluginResult(
                    result.error,
                    result.detail,
                    result.committed,
                    result.events,
                )
        elif field == "Power":
            if not isinstance(value, bool):
                return PluginResult("openapollo.invalid_plugin_value")
            operation, args = "bypass", (slot, value)
        elif re.fullmatch(
            r"parameters/\d+/(NormalizedValue|StepValue)", field
        ):
            if field.endswith("/NormalizedValue"):
                if isinstance(value, bool) or not isinstance(
                    value, (int, float)
                ):
                    return PluginResult("openapollo.invalid_plugin_value")
                try:
                    valid = math.isfinite(value) and 0 <= value <= 1
                except (OverflowError, TypeError):
                    valid = False
                if not valid:
                    return PluginResult("openapollo.invalid_plugin_value")
            else:
                # Step values vary by plugin; preserve string/int representations.
                if isinstance(value, bool) or not isinstance(
                    value, (str, int)
                ):
                    return PluginResult("openapollo.invalid_plugin_value")
            operation, args = "set_parameter", (slot, field, value)
        else:
            return PluginResult("openapollo.plugin_read_only")
        if self.transactions is not None and operation in (
            "bypass",
            "set_parameter",
        ):
            result = self.transactions.control(slot, field, value)
            return PluginResult(
                result.error, result.detail, result.committed, result.events
            )
        if getattr(self.backend, "diagnostic_only", False) is not True:
            return PluginResult(
                "openapollo.plugin_publication_unavailable",
                "Side-effectful backend registration is not supported",
            )
        try:
            result = getattr(self.backend, operation)(*args)
            if not isinstance(result, PluginResult) or result.error is None:
                # No publisher exists in phase 1: fail closed even if a backend
                # accidentally claims success, rather than emitting fake state.
                result = PluginResult(
                    "openapollo.plugin_publication_unavailable"
                )
            else:
                # Diagnostic adapters cannot forge a committed transaction.
                result = PluginResult(result.error, result.detail)
        except Exception:
            log.exception(
                "Plugin backend failed operation=%s slot=%s", operation, slot
            )
            result = PluginResult("openapollo.plugin_backend_failed")
        log.warning(
            "Plugin request operation=%s slot=%s backend=%s error=%s detail=%s",
            operation,
            slot,
            type(self.backend).__name__,
            result.error,
            result.detail,
        )
        return result
