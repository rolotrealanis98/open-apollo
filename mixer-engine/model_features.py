"""Per-model facts from devices/<model>.json that the daemon acts on.

The tables come from the UA Mixer Engine's MixerType* classes (apollo-linux
docs/re-notes/me-model-rows-2026-10-11.md has the derivation):

* monitor_taper: the control-room level taper (48 or 54 steps).

Pure functions: nothing here touches the hardware or the state tree.
"""

import json
from pathlib import Path

DEVICES_DIR = Path(__file__).resolve().parent.parent / "devices"


def load_descriptor(slug: str | None) -> dict:
    """devices/<slug>.json as a dict ({} when missing or unreadable)."""
    if not slug:
        return {}
    try:
        d = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
    except (OSError, ValueError):
        return {}
    return d if isinstance(d, dict) else {}


# ── Monitor taper (N28) ──────────────────────────────────────────────

def _f32(x: float) -> float:
    import struct
    return struct.unpack("<f", struct.pack("<f", x))[0]


def taper_step(tapered: float, steps: int) -> int:
    """CMxOutput::CRPGATaperTodB (0x10007b580): step = (uint)(t*steps +
    0.5) in float, clamped to steps; negative t is 0."""
    t = 0.0 if tapered < 0 else float(tapered)
    return min(steps, int(_f32(_f32(t) * steps) + 0.5))


def taper_db_to_tapered(db: float, table: list) -> float:
    """CMxOutput::dBToCRPGATaper (0x10007b608): the first step whose dB is
    >= db, as step/steps (1.0 when none)."""
    steps = len(table) - 1
    for i, v in enumerate(table):
        if v >= db:
            return min(1.0, i / steps)
    return 1.0


def _raw(db: float) -> int:
    return max(0, min(192, int(round(192 + db * 2))))


def taper_luts(table: list) -> tuple[tuple, tuple]:
    """(forward, reverse) lookup tables of hardware.py's monitor taper for
    a dB table: forward[i] = raw level for tapered i/360, reverse[raw] =
    the middle of the tapered positions that land on that raw level, gaps
    interpolated.  For the 54-step table this reproduces the LUTs captured
    from the x4's Mixer Engine exactly."""
    steps = len(table) - 1
    fwd = tuple(_raw(table[taper_step(i / 360.0, steps)]) for i in range(361))
    pos: dict = {}
    for i, v in enumerate(fwd):
        pos.setdefault(v, []).append(i / 360.0)
    rev = [None] * 193
    for v, ps in pos.items():
        rev[v] = (min(ps) + max(ps)) / 2
    known = [v for v in range(193) if rev[v] is not None]
    for v in range(193):
        if rev[v] is None:
            lo = max(k for k in known if k < v)
            hi = min(k for k in known if k > v)
            rev[v] = rev[lo] + (rev[hi] - rev[lo]) * (v - lo) / (hi - lo)
    return fwd, tuple(round(x, 6) for x in rev)


def monitor_taper_table(desc: dict) -> list | None:
    """The model's monitor taper dB table (steps + 1 values ending at 0 dB),
    None without one."""
    t = desc.get("monitor_taper") if isinstance(desc, dict) else None
    if not isinstance(t, dict):
        return None
    db = t.get("db")
    if not isinstance(db, list) or len(db) != int(t.get("steps", 0)) + 1:
        return None
    return [float(v) for v in db]
