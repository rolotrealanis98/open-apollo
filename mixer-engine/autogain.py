"""Host-side preamp autogain, as the UA Mixer Helper's UAAutoGainAlgo does it.

Spec: apollo-linux docs/re-notes/mixer-helper-autogain-2026-10-11.md.  The
algorithm is host-side and hardware-agnostic: it needs the per-channel
sample peak of the input audio (dB) and a way to read and write a preamp's
Gain in integer dB (and its Pad).  `AutoGain` is that state machine, with
no I/O of its own: a clock, a peak feed and a gain/pad writer are handed in,
so tests drive it with a fake clock (advance()) and the daemon drives it
from its event loop (AutoGainService.run()).

Channel states (the spec's names table): 0 Stopped, 1 Waiting For Audio,
2 Listening To Signal, 3 Adjusting Gain, 6 Auto-Gain Completed, 7 Overload,
8 Done Overload, 9 Adjusting Pad, 10 Done Listening, 11 Level Too Low,
12 Waiting To Restart, 13 Displaying Errors (4 and 5 are never set).

Interpretations where the spec is silent or marked M/L are noted inline.
"""

import asyncio
import heapq
import json
import logging
import math
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

log = logging.getLogger("ua-autogain")

# ── Constants (spec sections 1, 3, 4, 5) ────────────────────────────

FLOOR_DB = -144.0          # "no signal"
LINEAR_FLOOR = 6.3096e-8   # SetPeak: dB = 20 log10(x) above this, else -144

MIN_GAIN = 10              # base plugin limits, dB
MAX_GAIN = 65
GAIN_STEP = 1

# Channel states
STOPPED, WAITING, LISTENING, ADJUSTING, COMPLETED = 0, 1, 2, 3, 6
OVERLOAD, DONE_OVERLOAD, ADJUSTING_PAD = 7, 8, 9
DONE_LISTENING, LEVEL_TOO_LOW, WAITING_RESTART, DISPLAY_ERROR = 10, 11, 12, 13

STATE_NAMES = {
    0: "Stopped", 1: "Waiting For Audio", 2: "Listening To Signal",
    3: "Adjusting Gain", 4: "Max Gain Reached", 5: "Min Gain Reached",
    6: "Auto-Gain Completed", 7: "Overload", 8: "Done Overload",
    9: "Adjusting Pad", 10: "Done Listening", 11: "Level Too Low",
    12: "Waiting To Restart", 13: "Displaying Errors",
}

# Manager status (the /auto_gain `status` property)
S_UNKNOWN = "unknown"
S_STOPPED = "stopped"
S_WAITING = "waiting_for_signal"
S_LISTENING = "listening"
S_OVERLOAD = "overload"
S_ADJUSTING = "adjusting_gain"
S_DONE = "done_listening"
S_OVERLOAD_ERROR = "overload_error"
S_LOW_SIGNAL = "low_signal_error"

STATUS_STR = {
    S_UNKNOWN: "",
    S_STOPPED: "READY",
    S_WAITING: "WAITING FOR AUDIO",
    S_LISTENING: "LISTENING FOR {n} SECONDS",
    S_DONE: "DONE LISTENING",
    S_OVERLOAD: "CHANNEL OVERLOADED",
    S_ADJUSTING: "ADJUSTING GAIN",
    S_OVERLOAD_ERROR: "CHANNEL OVERLOAD",
    S_LOW_SIGNAL: "LEVEL TOO LOW",
}

# `info` texts.  The overload and done prompts are the Mixer Helper's;
# the others only paraphrase it (the report quotes only their openings).
INFO = {
    S_UNKNOWN: "",
    S_STOPPED: "",
    S_WAITING: "Input the busiest, loudest sounds you will be recording.",
    S_LISTENING: "Input the busiest, loudest sounds you will be recording.",
    S_ADJUSTING: "Adjusting preamp gain.",
    S_DONE: "Preamp gain has been automatically set for recording.",
    S_OVERLOAD: "Overload detected. Auto-Gain will adjust and restart "
                "the process.",
    S_OVERLOAD_ERROR: "The input still overloads at the minimum gain. "
                      "Lower the level of the source.",
    S_LOW_SIGNAL: "No signal was detected. Raise the level of the source "
                  "and try again.",
}

# count_down special codes
CD_DONE = 0xFF
CD_WAITING = 0xFE
CD_LOW_SIGNAL = 0xFC
CD_OVERLOAD_ERROR = 0xFB


@dataclass
class Settings:
    """User settings (JSON "settings" of the "auto_gain" prefs folder) plus
    the hidden feature-key tunables, all with the Mixer Helper's defaults."""
    duration: int = 10                  # s of listening
    dynamic_auto_gain: bool = True      # WHILE-LISTENING mode
    peak_target: float = -8.0           # dBFS
    threshold: float = -60.0            # dBFS
    # hidden tunables
    timeout_ms: int = 20000             # wait-for-signal timeout (c618)
    overload_db: float = -3.0           # overload while listening (c5d0)
    listen_ms: int = 2000               # WHILE-LISTENING window (c540)
    ramp_ms: int = 3000                 # total ramp, normal mode (c4f8)
    ramp_dyn_ms: int = 1000             # total ramp, WHILE-LISTENING (c4b0)
    pad_window_db: float = 6.0          # pad "near target" window (c588)

    def update(self, data: dict):
        """Take the keys of a settings JSON object that name a field."""
        for key, value in data.items():
            if key in self.__dataclass_fields__ and value is not None:
                want = type(getattr(self, key))
                try:
                    setattr(self, key, want(value))
                except (TypeError, ValueError):
                    log.warning("autogain: bad setting %s=%r", key, value)


def linear_to_db(x: float) -> float:
    """UAAutoGainMgr::SetPeak: 20 log10(x) above 6.3096e-8, else -144."""
    if x is None or x <= LINEAR_FLOOR:
        return FLOOR_DB
    return 20.0 * math.log10(x)


def round_half_away(x: float) -> int:
    """Integer rounding, halves away from zero (-2.5 -> -3, 2.5 -> 3)."""
    return int(math.floor(abs(x) + 0.5)) * (1 if x >= 0 else -1)


# ── Timers on an injectable clock ───────────────────────────────────

class _Timers:
    """Keyed timers; re-arming a key cancels the previous one (the UATimer
    destructor stops it).  run_until() fires them in deadline order and
    re-arms relative to the deadline, so a coarse clock step still replays
    the ramp ticks one by one."""

    def __init__(self, now: float):
        self.now = now
        self._heap: list = []
        self._live: dict = {}       # key -> (seq, deadline)
        self._seq = 0

    def arm(self, key, delay_ms: float, fn: Callable, periodic=False):
        self._seq += 1
        deadline = self.now + delay_ms / 1000.0
        self._live[key] = (self._seq, deadline)
        heapq.heappush(self._heap, (deadline, self._seq, key, fn,
                                    delay_ms if periodic else None))

    def cancel(self, key):
        self._live.pop(key, None)

    def armed(self, key) -> bool:
        return key in self._live

    def run_until(self, t: float):
        while self._heap and self._heap[0][0] <= t:
            deadline, seq, key, fn, period = heapq.heappop(self._heap)
            live = self._live.get(key)
            if live is None or live[0] != seq:
                continue                    # cancelled or re-armed
            self.now = max(self.now, deadline)
            if period is None:
                del self._live[key]
            else:
                self._seq += 1
                nxt = deadline + period / 1000.0
                self._live[key] = (self._seq, nxt)
                heapq.heappush(self._heap, (nxt, self._seq, key, fn, period))
            fn()
        self.now = max(self.now, t)


class Channel:
    def __init__(self, index: int):
        self.index = index
        self.state = STOPPED
        self.text = ""
        self.peak = FLOOR_DB        # latest peak fed
        self.max_peak = FLOOR_DB
        self.last_peak = FLOOR_DB
        self.delta = 0              # requested change, dB
        self.applied = 0            # change the ramp makes (dst - cur)
        self.dst = 0
        self.step = 0


class AutoGain:
    """The autogain manager and per-channel algorithm.

    `io` supplies the plugin side:
        get_gain(ch) -> float dB      set_gain(ch, db)
        can_use_pad(ch) -> bool       set_pad(ch, on)
        exists(ch) -> bool
    `on_change(name, value)` is told of `status`, `status_str`, `info` and
    `count_down` changes.  Time comes only from advance()/step().
    """

    def __init__(self, io, settings: Settings | None = None,
                 on_change: Callable[[str, Any], None] | None = None,
                 clock: float = 0.0,
                 min_gain: int = MIN_GAIN, max_gain: int = MAX_GAIN):
        self.io = io
        self.settings = settings or Settings()
        self.on_change = on_change or (lambda name, value: None)
        self.min_gain = min_gain
        self.max_gain = max_gain
        self.t = _Timers(clock)
        self.channels: dict[int, Channel] = {}
        self.status = S_STOPPED
        self.count_down = CD_DONE
        self.apply_requested = False
        self.duration_override = 0
        self._published: dict[str, Any] = {}
        self._publish()

    # ── Properties ──────────────────────────────────────────────

    def properties(self) -> dict:
        status_str = STATUS_STR[self.status].format(n=self.count_down)
        return {"status": self.status, "status_str": status_str,
                "info": INFO[self.status], "count_down": self.count_down}

    def _publish(self):
        for name, value in self.properties().items():
            if self._published.get(name) != value:
                self._published[name] = value
                self.on_change(name, value)

    def _set_status(self, status: str, count_down: int | None = None):
        self.status = status
        if count_down is not None:
            self.count_down = count_down
        self._publish()

    @property
    def active(self) -> bool:
        return bool(self.channels) or self.status != S_STOPPED

    # ── Time ────────────────────────────────────────────────────

    def advance(self, t: float):
        """Run every timer due at or before clock time `t` (seconds)."""
        self.t.run_until(t)

    # ── Commands ────────────────────────────────────────────────

    def add_channel(self, index: int) -> Channel:
        c = self.channels.get(index)
        if c is None:
            c = self.channels[index] = Channel(index)
        return c

    def start_all(self):
        """StartAll: every armed channel waits for audio."""
        self.apply_requested = False
        self.t.cancel(("m", "stop_all"))
        self._set_status(S_WAITING, CD_WAITING)
        for c in self.channels.values():
            self._start_waiting(c)

    def stop_channel(self, index: int):
        """StopAutoGain: take one channel out."""
        c = self.channels.pop(index, None)
        if c is None:
            return
        self._stop_channel_timers(c)
        c.state = STOPPED
        if not self.channels:
            self.t.cancel(("m", "tick"))
            self._set_status(S_STOPPED, CD_DONE)

    def stop_all(self, delay_ms: float = 10):
        """StopAll(remove): after a short delay, every channel is stopped
        and removed (the stop command delays it by 10 ms)."""
        self.t.arm(("m", "stop_all"), delay_ms,
                   lambda: self._do_stop_all(True))

    def apply(self):
        """start_auto_gain apply=true: stop listening now and adjust from
        the maximum measured so far (normal mode); WHILE-LISTENING stops."""
        if self._wl_available():
            self.stop_all()
            return
        self.apply_requested = True
        self._set_status(S_ADJUSTING)
        for c in list(self.channels.values()):
            if c.state == LISTENING:
                self._cancel(c, "listen")
                self._start_adjusting(c)

    # ── Peak feed ───────────────────────────────────────────────

    def on_peak(self, index: int, db: float):
        """A new peak (dBFS) for a channel.  The Mixer Helper's property
        only notifies on change; callers feed changes."""
        c = self.channels.get(index)
        if c is None or c.state == STOPPED:
            return
        c.peak = db
        self._threshold_check(c)
        st = c.state
        if st == WAITING:
            if db > self.settings.threshold:
                self._cancel(c, "timeout")
                c.max_peak = db
                self._start_listening_all()
        elif st == LISTENING:
            if db > c.max_peak:
                c.max_peak = db
            self._listening_check(c)
        elif st == ADJUSTING:
            if db >= 0.0:
                self._start_overload(c)
            elif db > self.settings.peak_target and \
                    c.max_peak < self.settings.peak_target:
                c.max_peak = db
                self._cancel(c, "ramp")
                self._start_adjusting(c)
        elif st == COMPLETED:
            if db >= 0.0:
                self._cancel(c, "settle")
                self._start_overload(c)

    # ── Per-channel timers ──────────────────────────────────────

    def _arm(self, c, name, delay_ms, fn, periodic=False):
        self.t.arm((c.index, name), delay_ms, fn, periodic)

    def _cancel(self, c, name):
        self.t.cancel((c.index, name))

    def _stop_channel_timers(self, c):
        for key in [k for k in self.t._live if k[0] == c.index]:
            self.t.cancel(key)

    # ── 1/2: waiting for signal ─────────────────────────────────

    def _start_waiting(self, c):
        self._stop_channel_timers(c)
        c.state = WAITING
        c.max_peak = c.last_peak = FLOOR_DB
        self._arm(c, "timeout", self.settings.timeout_ms,
                  lambda: self._level_too_low(c))

    def _threshold_check(self, c):
        """ThresholdCheck: a peak under the threshold arms a 1 s reset, one
        at or over it cancels it.  (M: the reset only fires after 1 s with
        no further sub-threshold update.)  Applied to listening channels
        only (the signal vanished): in the other states a reset would
        interrupt a ramp or an overload, or, while waiting, restart the 20 s
        timeout so that it never fires."""
        if c.state != LISTENING:
            return
        if c.peak < self.settings.threshold:
            self._arm(c, "threshold", 1000, lambda: self._threshold_reset(c))
        else:
            self._cancel(c, "threshold")

    def _threshold_reset(self, c):
        if c.state == LISTENING:
            self._start_waiting(c)
            if all(x.state == WAITING for x in self.channels.values()):
                self.t.cancel(("m", "tick"))
                self._set_status(S_WAITING, CD_WAITING)

    # ── 3: listening ────────────────────────────────────────────

    def _wl_available(self) -> bool:
        s = self.settings
        return (s.dynamic_auto_gain and len(self.channels) <= 2
                and s.duration * 1000 // s.listen_ms >= 2)

    def _start_listening_all(self):
        for c in list(self.channels.values()):
            if c.state == WAITING:
                self._start_listening(c)

    def _start_listening(self, c):
        s = self.settings
        self._stop_channel_timers(c)
        c.state = LISTENING
        # The peak property's latest value counts as a measurement (it is
        # not re-notified when the signal sits steady)
        if c.peak > c.max_peak:
            c.max_peak = c.peak
        wl = self._wl_available()
        delay = s.listen_ms if wl else s.duration * 1000
        self._arm(c, "listen", delay, lambda: self._start_adjusting(c))
        if not self.t.armed(("m", "tick")):
            # New listening session: count_down is the duration override if
            # given (consumed), else the duration, and ticks down each
            # second.  A WHILE-LISTENING loop keeps one countdown.
            self.count_down = self.duration_override or s.duration
            self.duration_override = 0
            self.t.arm(("m", "tick"), 1000, self._countdown_tick,
                       periodic=True)
        self._set_status(S_LISTENING)
        # The measurement in hand counts at once: a steady signal sends no
        # further peak update to trigger the pad or overload check
        self._listening_check(c)

    def _listening_check(self, c):
        """On each peak while listening: engage the pad if that is called
        for, else treat maxPeak at or over the overload level (-3 dBFS, with
        a lower target) as an overload."""
        if self._should_engage_pad(c):
            self._start_pad(c)
        elif (c.max_peak >= self.settings.overload_db
              and self.settings.peak_target < self.settings.overload_db):
            self._set_status(S_OVERLOAD)
            self._start_overload(c)

    def _countdown_tick(self):
        if self.status in (S_LISTENING, S_ADJUSTING, S_OVERLOAD) and \
                0 < self.count_down < CD_OVERLOAD_ERROR:
            self.count_down -= 1
            self._publish()

    # ── 4: adjusting gain ───────────────────────────────────────

    def _start_adjusting(self, c):
        s = self.settings
        self._cancel(c, "listen")
        if c.max_peak < s.threshold:
            self._level_too_low(c)
            return
        c.delta = round_half_away(s.peak_target - c.max_peak)
        if c.delta == 0:
            self._done_listening(c)
            return
        cur = round_half_away(self.io.get_gain(c.index))
        dst = max(self.min_gain, min(self.max_gain, cur + c.delta))
        if dst == cur or c.max_peak == c.last_peak:
            self._done_listening(c)         # at a limit, or no new signal
            return
        c.last_peak = c.max_peak
        c.dst = dst
        c.applied = dst - cur
        c.step = GAIN_STEP if c.delta > 0 else -GAIN_STEP
        c.state = ADJUSTING
        c.text = f"{dst - cur}dB"
        n = abs(dst - cur)
        wl = self._wl_available()
        total = s.ramp_dyn_ms if wl else s.ramp_ms
        interval = 100 if n < 3 else total / n
        self._arm(c, "ramp", interval, lambda: self._ramp_tick(c),
                  periodic=True)
        if all(x.state in (ADJUSTING, COMPLETED)
               for x in self.channels.values()):
            self._set_status(S_ADJUSTING)

    def _ramp_tick(self, c):
        if not self.io.exists(c.index):
            self._cancel(c, "ramp")
            c.state = STOPPED
            c.text = "The channel no longer exists"
            return
        cur = round_half_away(self.io.get_gain(c.index))
        nxt = c.dst if abs(cur - c.dst) < abs(c.step) else cur + c.step
        self.io.set_gain(c.index, nxt)
        if nxt == c.dst:
            self._cancel(c, "ramp")
            c.state = COMPLETED
            self._arm(c, "settle", 1000, lambda: self._settled(c))

    def _settled(self, c):
        if c.applied < 0:
            c.max_peak += c.applied         # predict the new peak
        self._done_listening(c)

    def _done_listening(self, c):
        c.state = DONE_LISTENING
        self._check_completion()

    # ── 7: completion ───────────────────────────────────────────

    def _check_completion(self):
        chans = list(self.channels.values())
        if not chans or any(c.state not in (DONE_LISTENING, LEVEL_TOO_LOW)
                            for c in chans):
            return
        if (not self.apply_requested and self._wl_available()
                and 1 <= self.count_down < CD_OVERLOAD_ERROR):
            for c in chans:
                self._start_listening(c)    # the dynamic loop
        elif any(c.state == LEVEL_TOO_LOW for c in chans):
            self.t.cancel(("m", "tick"))
            self._set_status(S_LOW_SIGNAL, CD_LOW_SIGNAL)
            for c in chans:
                if c.state == LEVEL_TOO_LOW:
                    self._display_error(c)
        else:
            self.t.cancel(("m", "tick"))
            self._set_status(S_DONE, CD_DONE)
            self.t.arm(("m", "stop_all"), 2000,
                       lambda: self._do_stop_all(True))

    def _display_error(self, c):
        """State 13 for 3 s, then stopped; channels stay armed (2 s later)."""
        c.state = DISPLAY_ERROR
        self._arm(c, "error", 3000, self._error_done)

    def _error_done(self):
        self._set_status(S_STOPPED)
        self.t.arm(("m", "stop_all"), 2000, lambda: self._do_stop_all(False))

    def _do_stop_all(self, remove: bool):
        for c in list(self.channels.values()):
            self._stop_channel_timers(c)
            c.state = STOPPED
        if remove:
            self.channels.clear()
        self.t.cancel(("m", "tick"))
        self.apply_requested = False
        self._set_status(S_STOPPED, CD_DONE)

    # ── 8: overload ─────────────────────────────────────────────

    def _start_overload(self, c):
        for name in ("listen", "ramp", "settle"):
            self._cancel(c, name)
        c.state = OVERLOAD
        if self.status != S_OVERLOAD:
            self._set_status(S_OVERLOAD)
        self._overload_tick(c)              # decrement at once, then ...
        if c.state == OVERLOAD:
            self._arm(c, "overload", 100, lambda: self._overload_tick(c),
                      periodic=True)        # ... every 100 ms

    def _overload_tick(self, c):
        if c.state != OVERLOAD:
            self._cancel(c, "overload")
            return
        cur = round_half_away(self.io.get_gain(c.index))
        if cur > self.min_gain and c.peak > self.settings.peak_target:
            self.io.set_gain(c.index, max(cur - GAIN_STEP, self.min_gain))
            return
        if c.peak <= self.settings.peak_target:
            self._cancel(c, "overload")
            c.state = DONE_OVERLOAD
            self._arm(c, "restart", 1000, lambda: self._overload_done(c))
        elif self._should_engage_pad(c):    # at min gain, still loud
            self._cancel(c, "overload")
            self._start_pad(c)
        else:
            log.info("autogain ch%d: hit min gain but still overload",
                     c.index)
            self._cancel(c, "overload")
            self.t.cancel(("m", "tick"))
            self._set_status(S_OVERLOAD_ERROR, CD_OVERLOAD_ERROR)
            c.state = DISPLAY_ERROR
            self._arm(c, "error", 3000, self._error_done)

    def _overload_done(self, c):
        c.max_peak = c.peak
        c.state = WAITING_RESTART
        self._try_restart()

    def _try_restart(self):
        """Restart listening once no channel is in overload, done-overload
        or adjusting-pad."""
        if any(x.state in (OVERLOAD, DONE_OVERLOAD, ADJUSTING_PAD)
               for x in self.channels.values()):
            return
        for c in list(self.channels.values()):
            if c.state == WAITING_RESTART:
                self._start_listening(c)

    # ── Pad ─────────────────────────────────────────────────────

    def _should_engage_pad(self, c) -> bool:
        """ShouldEngagePad: the pad is usable and either turning the gain
        down cannot absorb the excess, or the signal is within the pad
        window of the target while the gain is at or below 17 dB."""
        if not self.io.can_use_pad(c.index):
            return False
        s = self.settings
        cur = round_half_away(self.io.get_gain(c.index))
        if c.max_peak > s.peak_target and \
                (c.max_peak - s.peak_target) > (cur - self.min_gain):
            return True
        return (c.max_peak >= s.peak_target - s.pad_window_db
                and cur <= (self.max_gain + self.min_gain) // 2 - 20)

    def _start_pad(self, c):
        for name in ("listen", "ramp", "settle", "overload"):
            self._cancel(c, name)
        c.state = ADJUSTING_PAD
        self.io.set_pad(c.index, True)
        self._arm(c, "restart", 1000, lambda: self._pad_done(c))

    def _pad_done(self, c):
        c.max_peak = FLOOR_DB
        c.state = WAITING_RESTART
        self._try_restart()

    # ── 9: level too low ────────────────────────────────────────

    def _level_too_low(self, c):
        self._stop_channel_timers(c)
        c.state = LEVEL_TOO_LOW
        self._check_completion()


# ── Daemon glue ─────────────────────────────────────────────────────

PREAMP = "/devices/0/inputs/{ch}/preamps/0"
START_CMD = "start_auto_gain"
STOP_CMD = "stop_auto_gain"
FOLDER = "/auto_gain"


class DaemonIO:
    """Gain, pad and peak access through the daemon: a gain write is a
    state-tree SET (hardware write via the router, subscribers told), so
    clients see it as they would a client SET."""

    def __init__(self, daemon):
        self.d = daemon

    def exists(self, ch: int) -> bool:
        return self.d.state.get_value(
            PREAMP.format(ch=ch) + "/Gain/value") is not None

    def get_gain(self, ch: int) -> float:
        v = self.d.state.get_value(PREAMP.format(ch=ch) + "/Gain/value")
        return float(v) if v is not None else float(MIN_GAIN)

    def set_gain(self, ch: int, db: float):
        # The daemon has no touch concept (SetTouch is not implemented), so
        # there is no touch wrapper around the write.
        from hardware import preamp_db_to_tapered
        base = PREAMP.format(ch=ch)
        self.d.state.set(f"{base}/Gain/value", float(db))
        # companion, as for a client Gain SET (state + helper tree, no
        # second hardware write)
        self.d._push_to_clients(f"{base}/GainTapered",
                                preamp_db_to_tapered(float(db)))
        now = time.monotonic()
        for p in (f"{base}/Gain", f"{base}/Gain/value",
                  f"{base}/GainTapered", f"{base}/GainTapered/value"):
            self.d._readback_suppress[p] = now
        log.info("autogain: input %d Gain = %d dB", ch, int(db))

    def can_use_pad(self, ch: int) -> bool:
        """CanUsePad: the input type is "Mic" (the Helper reads "Hi-Z"
        while HiZ is on), the Pad control exists and Pad is off.  The
        engine's input IOType is the Mic/Line switch ("Mic" or "Line" on a
        preamp input in the captured x4 and x8p maps; the readback keeps it
        current), so a Line-mode input never gets the pad."""
        st = self.d.state
        base = PREAMP.format(ch=ch)
        pad = st.get_value(f"{base}/Pad/value")
        if pad is None or pad or st.get_value(f"{base}/HiZ/value"):
            return False
        iotype = st.get_value(f"/devices/0/inputs/{ch}/IOType/value")
        return iotype is None or iotype == "Mic"

    def set_pad(self, ch: int, on: bool):
        base = PREAMP.format(ch=ch)
        self.d.state.set(f"{base}/Pad/value", bool(on))
        self.d._readback_suppress[f"{base}/Pad"] = time.monotonic()
        log.info("autogain: input %d Pad = %s", ch, on)

    def peak_db(self, ch: int) -> float:
        """Instantaneous peak (dBFS) of input `ch` from the daemon's meter.

        That is the meter's per-buffer `level` (not the 2 s peak hold): on
        the PipeWire/ALSA meters it is the sample peak of the capture
        stream, which is what UA uses; on the x4 family the default
        ReadbackMeter reads the DSP's input meters instead (pass
        --alsa-device for the capture-stream peak).

        A stereo (linked) input gives the louder of its two channels, as
        the Helper's analysis keeps one running max over every channel of
        the input."""
        from metering import meter_channel
        d = self.d
        base = PREAMP.format(ch=ch).rsplit("/preamps", 1)[0]
        meter = d._metering
        name = d.state.get_value(f"{base}/Name/value")
        stereo = bool(d.state.get_value(f"{base}/Stereo/value"))
        ids = list(meter.channel_ids)
        best = FLOOR_DB
        for side in ((0, 1) if stereo else (0,)):
            cap = meter_channel("inputs", ch, side, name, stereo, ids)
            if cap is not None:
                best = max(best, float(meter.get_channel_meter(cap)[0]))
        return best


class AutoGainService:
    """Serves /auto_gain on the Mixer Helper tree and runs the algorithm."""

    def __init__(self, daemon, any_model: bool = False,
                 clock: Callable[[], float] = time.monotonic,
                 io: DaemonIO | None = None):
        self.d = daemon
        self.any_model = any_model      # --autogain-any-model
        self.clock = clock
        self.io = io or DaemonIO(daemon)
        self.settings = Settings()
        self.ag = AutoGain(self.io, self.settings, self._changed,
                           clock=clock())
        self._last: dict[int, float] = {}
        self._install_properties()

    # -- gating --

    def preamp_inputs(self) -> list[int]:
        out = []
        for i in range(8):
            if self.d.state.get_value(
                    PREAMP.format(ch=i) + "/Gain/value") is not None:
                out.append(i)
        return out

    def supported(self) -> bool:
        """UA offers autogain on Gen 2 models: the map's SupportsAutogain.
        --autogain-any-model allows any model with preamps."""
        if not self.preamp_inputs():
            return False
        if self.any_model:
            return True
        return bool(self.d.state.get_value("/devices/0/SupportsAutogain/value"))

    # -- helper tree --

    def _node(self):
        tree = self.d.helper_tree
        return tree._resolve(FOLDER) if tree else None

    def _install_properties(self):
        node = self._node()
        if node is None:
            return
        props = node.setdefault("properties", {})
        for name, value in self.ag.properties().items():
            props[name] = {"read_only": True, "value": value}

    def _changed(self, name: str, value):
        tree = self.d.helper_tree
        node = self._node()
        if node is None:
            return
        meta = node.setdefault("properties", {}).setdefault(
            name, {"read_only": True})
        meta["value"] = value
        tree._notify(f"{FOLDER}/{name}", value)

    # -- settings --

    def load_settings(self, directory: Path | str | None):
        """auto_gain_prefs.json in the daemon's store directory, if any:
        {"settings": {...}} or the bare object."""
        if directory is None:
            return
        path = Path(directory) / "auto_gain_prefs.json"
        try:
            data = json.loads(path.read_text())
        except (OSError, ValueError):
            return
        if isinstance(data, dict):
            inner = data.get("settings")
            self.settings.update(inner if isinstance(inner, dict) else data)

    # -- commands --

    def owns(self, path: str) -> bool:
        return path.rstrip("/") in (f"{FOLDER}/{START_CMD}",
                                    f"{FOLDER}/{STOP_CMD}")

    def execute(self, path: str, params: dict) -> tuple[Any, str | None]:
        """(data, None) or (None, error text); params from parse_cmdparams."""
        from commands import ERR_FAILED
        stop = path.rstrip("/").endswith(STOP_CMD)
        self.ag.t.now = max(self.ag.t.now, self.clock())
        if not self.supported():
            log.info("autogain: %s refused: model has no SupportsAutogain "
                     "(use --autogain-any-model) or no preamps", path)
            return None, ERR_FAILED
        want_all = bool(params.get("all"))
        index = self._channel_index(params)
        if stop:
            if want_all:
                self.ag.stop_all()
            elif index is not None:
                self.ag.stop_channel(index)
            else:
                return None, ERR_FAILED
            return True, None
        if self.ag.status == S_STOPPED and not self.ag.channels:
            self.load_settings(getattr(self.d.commands, "store_dir", None))
        if params.get("apply"):
            self.ag.apply()
            return True, None
        inputs = self.preamp_inputs()
        if int(params.get("duration") or 0) > 0:
            self.ag.duration_override = int(params["duration"])
        if want_all:
            if not self.ag.channels:
                for i in inputs:
                    self.ag.add_channel(i)
            self.ag.start_all()
        elif index is not None and index in inputs:
            self.ag.add_channel(index)
            if params.get("auto_start"):
                self.ag.start_all()
        else:
            return None, ERR_FAILED
        self._last.clear()
        return True, None

    @staticmethod
    def _channel_index(params: dict) -> int | None:
        for key in ("channel_index", "channel"):
            v = params.get(key)
            if v is None or isinstance(v, bool):
                continue
            try:
                return int(v)
            except (TypeError, ValueError):
                continue
        return None

    # -- the loop --

    def step(self, now: float | None = None):
        """Feed changed peaks, then run due timers (one pump)."""
        now = self.clock() if now is None else now
        self.ag.advance(now)
        for ch in list(self.ag.channels):
            db = self.io.peak_db(ch)
            if self._last.get(ch) != db:
                self._last[ch] = db
                self.ag.on_peak(ch, db)

    async def run(self, interval: float = 0.02):
        """Poll while a session is active (the daemon runs this task)."""
        while True:
            if self.ag.active:
                try:
                    self.step()
                except Exception:
                    log.exception("autogain step failed")
                await asyncio.sleep(interval)
            else:
                await asyncio.sleep(0.25)
