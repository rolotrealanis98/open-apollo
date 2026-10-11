"""What the daemon's connect/init sequence needs to know about a model.

The init sequence used to be a replay of the Apollo x4's macOS DTrace with
the x4's channel numbers inside.  ModelInfo carries the per-model facts (from
devices/<slug>.json) the sequence is derived from, so the x4 produces the
same writes as the replay and every other model gets its own channels:

  route / level channel lists    mixer_inputs[].synch_in
  talkback bus                   talkback_synch_in (Twin MkII: 2, others 10)
  talkback / mirror / CUE        talkback, mirror_to_digital flags
  DigOutMode on connect          only the Freud mixer types (the x4)

Kind-gated monitor params follow the kext's gates (apollo-linux
docs/re-notes/parity/14-mixer-params.md section 2), the same sets as
driver/ua_core.c.
"""

from __future__ import annotations

# Apollo x4 (devices/apollo-x4.json mixer_inputs): the model a router has
# when nothing says otherwise (no descriptor for the attached device, or no
# hardware).  tests/test_init_sequence.py checks this against the JSON.
_X4_INPUTS = (
    ("ANALOG 1", 0), ("ANALOG 2", 1), ("ANALOG 3", 2), ("ANALOG 4", 3),
    ("ADAT 1", 12), ("ADAT 2", 13), ("ADAT 3", 14), ("ADAT 4", 15),
    ("ADAT 5", 20), ("ADAT 6", 21), ("ADAT 7", 22), ("ADAT 8", 23),
    ("S/PDIF L", 8), ("S/PDIF R", 9),
    ("VIRTUAL 1", 24), ("VIRTUAL 2", 25), ("VIRTUAL 3", 26),
    ("VIRTUAL 4", 27), ("VIRTUAL 5", 28), ("VIRTUAL 6", 29),
    ("VIRTUAL 7", 30), ("VIRTUAL 8", 31),
    ("TALKBACK", 10), ("UNAVAILABLE", 11),
)

TALKBACK_BUS_DEFAULT = 0x0A

# Mixer types whose connect sequence is the DTrace-verified x4 one, which
# writes DigOutMode (monitor param 0x21) = S/PDIF.  Every other model leaves
# the optical output as it is: the x8p, 8p and x8p Gen 2 default to ADAT.
DIGOUT_ON_CONNECT_CLASSES = frozenset({"Freud", "FreudG2"})

# The init sequence sends Level 0xA0 to the analog slots 4-7 of a mixer with
# four or more preamps even where the model has fewer analog inputs (x4)
_RESERVED_ANALOG_SLOTS = range(4, 8)
# macOS routes only Virtual 1-4 at connect
_ROUTED_VIRTUALS = 4


def _kinds(*kinds) -> int:
    mask = 0
    for k in kinds:
        mask |= 1 << k
    return mask


# Allowed-kind sets of the kext's param gates (driver/ua_core.c
# UA_KIND_SET_*).  A kind of None (not reported, or a part the kext makes
# no mixer for) is not gated, as in the driver.
_KIND_SET_3E = _kinds(2, 4, 5, 6, 7, 8, 14, 15, 21, 22)
_KIND_SET_41 = _kinds(2, 7, 8, 14, 15, 21, 22)
_KIND_SET_10_22 = _kinds(*range(10, 23))
_KIND_SET_2A_2D = _kinds(1, 5, 13, 16, 20)
_KIND_SET_33_35 = _kinds(1, 3, 5, 13, 16, 20)
_KIND_SET_42 = _kinds(4, 6, 10, 11, 12, 13, 16, 17, 18, 19, 20)
_KIND_SET_49 = _kinds(10, 11, 12, 13, 16, 17, 18, 19, 20)
_KIND_SET_69 = _kinds(10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 22)


# Init writes of the x4 capture that overwrite a user setting with a fixed
# value (SRConvert on, OutPad A/B off, DimLevel 7): not sent where the mixer
# kind is unknown, as the unit's own value is the right one
_X4_FIXED_DEFAULTS = (0x1F, 0x19, 0x1A, 0x43)


def monitor_param_is_gated(param: int) -> bool:
    """Whether some mixer kind refuses monitor param `param`, or the init
    sets it to an x4 default (for a mixer of unknown kind)."""
    return param in _X4_FIXED_DEFAULTS or not all(
        monitor_param_allowed(param, k) for k in range(32))


def monitor_param_allowed(param: int, kind: int | None,
                          gate_unknown: bool = False) -> bool:
    """Whether the mixer of `kind` accepts monitor ("other") param `param`.

    The driver refuses a gated param with EINVAL; the init sequence skips
    it instead of sending it (x4, kind 14, passes every gate it uses).
    A kind of None is not gated, as in the driver, unless `gate_unknown`
    (the USB backend, whose kind is not known): then no gated param passes.
    """
    if kind is None or kind < 0:
        return not (gate_unknown and monitor_param_is_gated(param))
    bit = 1 << kind
    if param in (0x36, 0x37):
        return kind == 3
    if 0x2A <= param <= 0x2D:
        return bool(_KIND_SET_2A_2D & bit)
    if 0x33 <= param <= 0x35:
        return bool(_KIND_SET_33_35 & bit)
    if param == 0x42:
        return bool(_KIND_SET_42 & bit)
    if param == 0x49:
        return bool(_KIND_SET_49 & bit)
    if param == 0x32:
        return kind not in (0, 3)
    if param == 0x3E:
        return bool(_KIND_SET_3E & bit)
    if param == 0x41:
        return bool(_KIND_SET_41 & bit)
    if param == 0x69:
        return bool(_KIND_SET_69 & bit)
    if param in (0x67, 0x68) or 0x87 <= param <= 0x8A:
        return bool(_KIND_SET_10_22 & bit)
    if 0x83 <= param <= 0x86:
        return kind == 16
    return True


class ModelInfo:
    """Per-model facts for the init sequence (devices/<slug>.json)."""

    def __init__(self, slug: str | None, mixer_class: str | None,
                 preamps: int, talkback: bool, talkback_bus: int,
                 mirror_to_digital: bool, default_input: str | None,
                 inputs):
        self.slug = slug
        self.mixer_class = mixer_class
        self.preamps = preamps
        self.talkback = talkback
        self.talkback_bus = talkback_bus
        self.mirror_to_digital = mirror_to_digital
        self.default_input = default_input
        self.inputs = tuple(inputs)       # ((name, synch_in), ...)

    @classmethod
    def from_descriptor(cls, slug, desc: dict) -> "ModelInfo":
        inputs = [(str(e["name"]).strip().upper(), int(e["synch_in"]))
                  for e in desc.get("mixer_inputs") or []
                  if isinstance(e, dict) and e.get("name") is not None
                  and e.get("synch_in") is not None]
        tb = desc.get("talkback_synch_in")
        rules = desc.get("route_rules") or {}
        return cls(
            slug=slug,
            mixer_class=desc.get("mixer_class"),
            preamps=int(desc.get("preamps") or 0),
            talkback=bool(desc.get("talkback")),
            talkback_bus=int(tb) if tb is not None else TALKBACK_BUS_DEFAULT,
            mirror_to_digital=bool(desc.get("mirror_to_digital")),
            default_input=rules.get("default_input"),
            inputs=inputs)

    @classmethod
    def x4(cls) -> "ModelInfo":
        return cls("apollo-x4", "Freud", 4, True, TALKBACK_BUS_DEFAULT, True,
                   "spdif", _X4_INPUTS)

    # ── channel lists of the init sequence ──────────────────────────

    def _synch(self, pred) -> list:
        return [s for n, s in self.inputs if pred(n)]

    @staticmethod
    def _is_virtual_beyond(name: str) -> bool:
        if not name.startswith("VIRTUAL "):
            return False
        try:
            return int(name.split()[1]) > _ROUTED_VIRTUALS
        except (IndexError, ValueError):
            return False

    def route_channels(self) -> list:
        """Input channels that get the route mask (param 0x13) at connect:
        every input the model has (analog, S/PDIF or AES, talkback, ADAT,
        Virtual 1-4), in channel order; x4: 0-3, 8-10, 12-15, 20-27."""
        return sorted(set(self._synch(
            lambda n: n != "UNAVAILABLE" and not self._is_virtual_beyond(n))))

    def spdif_left_channel(self) -> int | None:
        """The first digital pair input (S/PDIF L, AES/EBU 1), which gets
        the wider route mask; None when the model has none."""
        for n, s in self.inputs:
            if n in ("S/PDIF L", "SPDIF L", "AES/EBU 1", "AES 1"):
                return s
        return None

    def level_channels(self) -> list:
        """Input channels that get the default DSP level (param 0x16):
        every synch-in of the model including the unavailable slot, plus
        the reserved analog slots 4-7 of a model with 4+ preamps (the x4
        DTrace sends level to 0-15 and 20-31)."""
        chans = set(s for _, s in self.inputs)
        if self.preamps >= 4:
            chans.update(_RESERVED_ANALOG_SLOTS)
        return sorted(chans)

    def link_channels(self, preamp_inputs) -> list:
        """Preamps that carry the stereo link flag: the first of each
        adjacent pair (x4 0 and 2, x8p 0/2/4/6, Twin 0)."""
        pre = sorted(preamp_inputs)
        return [c for c in pre if c % 2 == 0 and c + 1 in pre]

    def digout_on_connect(self) -> bool:
        return self.mixer_class in DIGOUT_ON_CONNECT_CLASSES
