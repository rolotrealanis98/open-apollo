"""Per-model facts from devices/<model>.json that the daemon acts on.

The tables come from the UA Mixer Engine's MixerType* classes (apollo-linux
docs/re-notes/me-model-rows-2026-10-11.md has the derivation):

* latency: converter (ADC/DAC) delays per sample rate and analog board,
  and what the engine derives from them: the ADC delay-comp setting (other
  param 0x6a), the per-output DAC delay-comp frames (SEL189) and the
  "additional latency" it reports to CoreAudio (SEL164);
* monitor_taper: the control-room level taper (48 or 54 steps);
* the engine feature flags: line reference level inputs, line PGA disable,
  ES9028 outputs, the function switch, front-panel switches, direct outputs,
  surround modes.

Pure functions: nothing here touches the hardware or the state tree.
"""

import json
from pathlib import Path

RATES = (44100, 48000, 88200, 96000, 176400, 192000)

# CMxRoot::SetMaximumDelayCompFromEnum (jump table 0x1005d4530) and the
# GlobalSettings defaults (ReadMixerServerSettings: DelayCompShortSamples
# 100, Medium 200, MediumLong 300, Long 1000).  Off sends 0.
MAX_DELAY_COMP_SAMPLES = {"Off": 0, "Short": 100, "Medium": 200,
                          "Medium-Long": 300, "Long": 1000}

# MixerTypeBase::GetAsioLatencyForDelayComp: the input side adds the DSP
# processing block (64 frames, 128 above 100 kHz) and 8 frames, the output
# side 4 frames (ExtraInput/OutputLatencySamples default 0).
ASIO_INPUT_PAD = 8
ASIO_OUTPUT_PAD = 4


def dsp_block(rate: int) -> int:
    return 128 if rate > 100000 else 64


# Classes whose Lines34AreReallyHP() is true (vtable slot 0x390: the Twin
# family): GetOutputDelays gives their cue outputs the delay-comp figure
# itself, so the DAC delay comp never delays them
LINES34_HP_CLASSES = frozenset({"Armstrong", "Stretch", "Lavern", "LavernG2",
                                "Shirley"})


# kPropFuncSwitchMode values (DescartesFuncSwitchMode, CMxMixer::
# GetFuncSwitchModeTypeInfo 0x10006a9b4): the value goes to other param
# 0x42 (switch 1) or 0x83 (switch 2, x16D only)
FUNC_SWITCH_VALUES = {"none": 0, "alt 2": 1, "mono": 2, "dim": 3,
                      "talkback": 4, "alt 1": 5}

# kPropSurroundMonitorMode index (CMxMixer::GetSurroundModeTypeInfo; the
# order of the captured x8p list).  8-10 are the Atmos modes of the x16 /
# x16D; their names are not in the captured trees.
SURROUND_MODES = ("STEREO", "2.1", "LCR", "LCRS", "QUAD", "5.1", "6.1",
                  "7.1")

# /ClipHold and /PeakHold: the value index goes to other params 0x13 / 0x14
# (CMxRoot::UpdateHardwareState 0x100095640 / 0x100095548); the durations
# are CMxRoot::Clip/PeakHoldDurationMs (table 0x1005d4780, -1 = infinite)
HOLD_NAMES = ("NONE", "1 SEC", "3 SEC", "5 SEC", "10 SEC", "INFINITE")
HOLD_MS = (0, 1000, 3000, 5000, 10000, -1)

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


def rate_index(rate) -> int | None:
    try:
        return RATES.index(int(rate))
    except (TypeError, ValueError):
        return None


def table_row(table: dict | None, board) -> list | None:
    """The per-rate row of a {"default", "boards"} table for an analog
    board id (kPropAnalogBoardID, readback word 33 [28:26]); the default
    row for board 0, an unknown board or no board."""
    if not isinstance(table, dict):
        return None
    try:
        b = int(board)
    except (TypeError, ValueError):
        b = 0
    for key, row in (table.get("boards") or {}).items():
        if str(b) in key.split(","):
            return row
    return table.get("default")


def table_frames(table: dict | None, rate, board=0) -> float | None:
    row = table_row(table, board)
    i = rate_index(rate)
    if row is None or i is None or i >= len(row):
        return None
    return float(row[i])


def latency_figures(latency: dict | None, rate, board=0,
                    max_delay_comp: int = 0,
                    lines34_hp: bool = False) -> dict | None:
    """What the Mixer Engine derives from the converter tables for one
    unit at `rate` (frames).  None without model data or for a rate the
    tables don't cover.

    adc / dac: GetInputDelay and GetOutputDelayForDelayComp;
    adc_comp: other param 0x6a (CMxMixer::UpdateADCDelayComp sends
        (int)(max input delay + 0.5); one unit's max is its own);
    dac_comp: per output kind, the frames SEL189 delays it by to line up
        with the slowest output (UpdateDACDelayComp: clamp(int(max - own
        + 0.5), 0, 255));
    additional_input / additional_output: SEL164 SetAudioAdditionalLatency
        as CMxRoot::SetMaximumDelayCompInDevice computes it (input + DSP
        block + MaxDelayComp + 8, output + 4, rounded so the two add up to
        the rounded total).
    """
    if not isinstance(latency, dict):
        return None
    adc = table_frames(latency.get("input"), rate, board)
    dac = table_frames(latency.get("output_delay_comp"), rate, board)
    if adc is None or dac is None:
        return None
    outs = latency.get("output") or {}
    out = {k: table_frames(outs.get(k), rate, board)
           for k in ("line", "cue", "monitor", "dsp")}
    dac_comp = {k: max(0, min(255, int(dac - v + 0.5)))
                for k, v in out.items() if v is not None and k != "dsp"}
    if lines34_hp and "cue" in dac_comp:
        dac_comp["cue"] = 0
    add_in = adc + dsp_block(int(rate)) + max(0, int(max_delay_comp)) \
        + ASIO_INPUT_PAD
    add_out = dac + ASIO_OUTPUT_PAD
    total = int(add_in + add_out + 0.5)
    in_r = int(add_in + 0.5)
    return {
        "adc": adc, "dac": dac, "outputs": out,
        "adc_comp": int(adc + 0.5),
        "dac_comp": dac_comp,
        "additional_input": in_r,
        "additional_output": total - in_r,
    }


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


# ── Direct outputs (IP 0x13 route word, CMxInput::UpdateHardwareState) ─

DIRECT_OUT_NONE = 0xFF       # no direct-output signal / destination
ROUTE_DEFAULT = 0x1FFFF      # ClearOutputDestinationOnHardware


def direct_out_route(signal: int, dest_synch: int, monitor_mix: bool,
                     stereo_leader: bool = False) -> int:
    """Input param 0x13 value (0x100063840): [7:0] direct-output signal
    index (0xff = none), [15:8] destination synch-out (0xff = none), bit 16
    the input still feeds the monitor mix, bit 17 set for the leader (even
    synch-in) of a stereo pair."""
    v = (signal & 0xFF) | ((dest_synch & 0xFF) << 8)
    if monitor_mix:
        v |= 0x10000
    if stereo_leader:
        v |= 0x20000
    return v


def output_destination_synch(name) -> int | None:
    """Synch-out id of an input OutputDestination value (the candidate list
    at 0x1005d41a0: Monitor = -1 (no direct out), Line 1-8 = 8-15, ADAT 1-8
    = 16-23, S/PDIF L/R = 6/7).  None for an unknown value."""
    if not isinstance(name, str):
        return None
    n = name.strip().upper()
    if n in ("MONITOR", "NONE", ""):
        return -1
    for prefix, base in (("LINE ", 8), ("ADAT ", 16)):
        if n.startswith(prefix) and n[len(prefix):].isdigit():
            k = int(n[len(prefix):])
            if 1 <= k <= 8:
                return base + k - 1
    if n in ("S/PDIF L", "SPDIF L"):
        return 6
    if n in ("S/PDIF R", "SPDIF R"):
        return 7
    return None
