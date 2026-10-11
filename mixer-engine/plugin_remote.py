"""Bounded VendorSpecific reads with an optional authoritative backend seam.

No native backend is supplied here. Success requires a separate authoritative
lease provider and fresh adapter readback; no state replay or defaults. Backend
read must retain the instance/mapping until completion. URL values are decimal
strings; response JSON metadata follows the independently recovered codec.
"""

from dataclasses import dataclass
import re
from plugin_lifecycle import PluginResult

_SLOT = re.compile(
    r"^(?P<slot>/devices/\d+/(?:inputs|auxs|outputs)/\d+(?:/preamps/\d+)?/effects/\d+)/VendorSpecific(?:/value)?$"
)
_DECIMAL = re.compile(r"-?[0-9]+\Z", re.ASCII)
MAX_IDS = MAX_COUNT = (
    4096  # Local resource bounds, not native protocol limits.
)


class RemoteQueryError(ValueError):
    def __init__(self, error, detail):
        self.error = error
        super().__init__(detail)


@dataclass(frozen=True)
class RemoteQuery:
    kind: str
    ids: tuple[int, ...] = ()
    opcode: int | None = None
    variable_id: int | None = None
    count: int | None = None
    header_word: int | None = None


def _number(value, name, low=0, high=0xFFFFFFFF):
    if (
        not isinstance(value, str)
        or len(value) > 11
        or not _DECIMAL.fullmatch(value)
    ):
        raise RemoteQueryError(
            "openapollo.invalid_remote_query",
            f"{name} must be one decimal string",
        )
    parsed = int(value)
    if not low <= parsed <= high:
        raise RemoteQueryError(
            "openapollo.invalid_remote_query",
            f"{name} is outside the diagnostic bound",
        )
    return parsed


def parse_remote_query(params):
    """Parse recovered selectors; unrelated keys remain caller-owned metadata.

    Native ID spelling is signed int32 decimal. Positive unsigned uint32 is
    accepted only as a local diagnostic extension; no variable meaning or
    equivalence between spellings is inferred.
    """
    if not isinstance(params, dict):
        raise RemoteQueryError(
            "openapollo.invalid_remote_query",
            "Query parameters must be a dictionary",
        )
    selectors = [k for k in ("ints", "floats", "opcode") if k in params]
    if not selectors:
        if any(k in params for k in ("p0", "p1", "p2")):
            raise RemoteQueryError(
                "openapollo.invalid_remote_query",
                "Remote headers require opcode",
            )
        return None
    if len(selectors) != 1:
        raise RemoteQueryError(
            "openapollo.invalid_remote_query",
            "Remote selectors are mutually exclusive",
        )
    key = selectors[0]
    if key != "opcode":
        if any(k in params for k in ("p0", "p1", "p2")):
            raise RemoteQueryError(
                "openapollo.invalid_remote_query",
                "Array headers require opcode",
            )
        raw = params[key]
        if not isinstance(raw, str) or len(raw) > MAX_IDS * 12:
            raise RemoteQueryError(
                "openapollo.invalid_remote_query",
                "ID list is too large or repeated",
            )
        items = raw.split(",")
        if not 1 <= len(items) <= MAX_IDS:
            raise RemoteQueryError(
                "openapollo.invalid_remote_query",
                "ID count is outside the diagnostic bound",
            )
        return RemoteQuery(
            key, tuple(_number(v, key, -0x80000000) for v in items)
        )
    opcode = _number(params[key], key)
    if opcode not in (516, 517, 518, 519):
        raise RemoteQueryError(
            "openapollo.unsupported_remote_opcode",
            "Only opcodes 516 through 519 are classified",
        )
    if any(k not in params for k in ("p0", "p1", "p2")):
        raise RemoteQueryError(
            "openapollo.invalid_remote_query",
            "Opcode query requires p0, p1, p2",
        )
    return RemoteQuery(
        "opcode",
        opcode=opcode,
        variable_id=_number(params["p0"], "p0", -0x80000000),
        count=_number(params["p1"], "p1", 0, MAX_COUNT),
        header_word=_number(params["p2"], "p2"),
    )


@dataclass(frozen=True)
class RemoteReadback:
    """Adapter-owned observation; query and lease must match the exact request.

    Backend read must pin the actual instance/mapping until completion and supply
    fresh values. Provider fencing cannot establish DMA safety for an adapter.
    """

    handle: object
    query: RemoteQuery
    words: tuple[int, ...]


@dataclass(frozen=True)
class RemoteResponse:
    data: str
    parameters: dict
    error: None = None


class PluginRemote:
    def __init__(self, state, backend=None, lease_provider=None):
        self.state, self.backend, self.lease_provider = (
            state,
            backend,
            lease_provider,
        )

    def _read(self, slot, path, request, params):
        # Import lazily: publication imports lifecycle, codec imports this module.
        from plugin_publication import OwnedHandle
        from plugin_remote_codec import (
            ScalarReply,
            ArrayReply,
            encode_scalar_data,
            encode_array_data,
        )

        backend = self.backend

        def current():
            lease = self.lease_provider.current_remote_handle(slot)
            if (
                not isinstance(lease, OwnedHandle)
                or type(lease.value) is not int
                or not 0 < lease.value <= 0xFFFFFFFFFFFFFFFF
                or not isinstance(lease.owner, str)
                or not lease.owner
                or type(lease.generation) is not int
                or lease.generation < 0
            ):
                raise ValueError("No current owned instance")
            value = self.state.get_value(slot + "/EffectInstance")
            if type(value) is not int or value != lease.value:
                raise ValueError(
                    "Published instance differs from backend lease"
                )
            return lease

        try:
            if self.lease_provider is None:
                raise ValueError("Authoritative lease provider required")
            if request.kind == "opcode" and request.opcode not in (518, 519):
                raise ValueError(
                    "Array metadata opcode response grammar not recovered"
                )
            if request.kind != "opcode" and (
                any(not -0x80000000 <= i <= 0x7FFFFFFF for i in request.ids)
                or len(set(request.ids)) != len(request.ids)
            ):
                raise ValueError(
                    "Authoritative scalar reads require unique signed int32 IDs"
                )
            revision = self.lease_provider.remote_revision
            if type(revision) is not int or revision < 0:
                raise ValueError("Authoritative publication revision required")
            handle = current()
            reply = backend.read(slot, handle, request)
            final_handle = current()
            final_revision = self.lease_provider.remote_revision
            if type(final_revision) is not int or final_revision < 0:
                raise ValueError("Invalid publication revision")
            if final_handle != handle or final_revision != revision:
                raise ValueError(
                    "Instance or publication changed during readback"
                )
            if (
                type(reply) is not RemoteReadback
                or type(reply.handle) is not OwnedHandle
                or type(reply.handle.value) is not int
                or type(reply.handle.owner) is not str
                or type(reply.handle.generation) is not int
                or reply.handle != handle
                or type(reply.query) is not RemoteQuery
                or reply.query != request
                or any(
                    type(getattr(reply.query, field))
                    is not type(getattr(request, field))
                    for field in (
                        "kind",
                        "ids",
                        "opcode",
                        "variable_id",
                        "count",
                        "header_word",
                    )
                )
                or (
                    request.kind != "opcode"
                    and any(type(i) is not int for i in reply.query.ids)
                )
                or type(reply.words) is not tuple
            ):
                raise ValueError("Readback identity or query differs")
            count = (
                len(request.ids) if request.kind != "opcode" else request.count
            )
            if len(reply.words) != count or any(
                type(w) is not int or not 0 <= w <= 0xFFFFFFFF
                for w in reply.words
            ):
                raise ValueError(
                    "Readback must contain the exact uint32 vector"
                )
            metadata = dict(params)
            if request.kind != "opcode":
                data = encode_scalar_data(
                    ScalarReply(path, request.kind, request.ids, reply.words)
                )
            else:
                data = encode_array_data(
                    ArrayReply(
                        path,
                        request.opcode,
                        request.variable_id,
                        request.count,
                        request.header_word,
                        reply.words,
                    )
                )
                # Recovered array response headers are numeric JSON metadata.
                metadata.update(
                    opcode=request.opcode,
                    p0=request.variable_id,
                    p1=request.count,
                    p2=request.header_word,
                )
            return RemoteResponse(data, metadata)
        except Exception as exc:
            # Never replay cached state, fill defaults, or serialize backend errors.
            return PluginResult(
                "openapollo.plugin_remote_readback_unavailable", str(exc)
            )

    def handle(self, path, raw_params):
        """None means unrelated GET; success requires current owned readback."""
        if not isinstance(path, str):
            return None
        canonical = "/" + "/".join(p for p in path.split("/") if p)
        match = _SLOT.fullmatch(canonical)
        if not match:
            return None
        try:
            request = parse_remote_query(
                raw_params if raw_params is not None else {}
            )
        except RemoteQueryError as exc:
            return PluginResult(exc.error, str(exc))
        if request is None:
            return None
        if not self.state.path_exists(match.group("slot")):
            return PluginResult("openapollo.invalid_plugin_slot")
        if self.backend is not None:
            return self._read(
                match.group("slot"), canonical, request, raw_params or {}
            )
        return PluginResult(
            "openapollo.plugin_remote_backend_unavailable",
            "No authoritative native plugin remote-control backend is connected",
        )
