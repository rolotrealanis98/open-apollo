"""Software audio metering via ALSA PCM capture.

Reads PCM samples from the ALSA capture device and computes per-channel
peak amplitudes, converting to dBFS.  Replaces the non-functional hardware
readback meter registers (rb_data[8]-[17] are static 0xf0000000).

The real UA Mixer Engine on macOS computes meters in its CMxMeter class
from PCM sample buffers in the same way.

Usage:
    meter = AlsaMeter(device='hw:2,0', capture_channels=22)
    meter.start()
    level, peak, clip = meter.get_input_meter(0)  # channel 0
    meter.stop()

If the ALSA device is unavailable (--no-hardware, pyalsaaudio not
installed, or wrong device), use NullMeter which returns silence (-77 dB)
for all channels.
"""

import array
import glob
import json
import logging
import math
import os
import re
import subprocess
import threading
import time

log = logging.getLogger("ua-metering")

SILENCE_DB = -77.0   # UA Mixer Engine silence floor
PEAK_HOLD_SEC = 2.0  # seconds to hold peak before decay
PEAK_DECAY_DB_SEC = 20.0  # dB/sec decay after hold expires
CLIP_HOLD_SEC = 2.0  # seconds to hold clip indicator
S32_MAX = 2147483647  # 2^31 - 1


def find_alsa_device():
    """Auto-detect the UA Apollo ALSA capture device.

    Scans /proc/asound/cards for the 'ua_apollo' driver and returns
    the hw:N,0 device string, or None if not found.
    """
    try:
        with open('/proc/asound/cards') as f:
            for line in f:
                # Format: " 2 [ua_apollo    ]: ua_apollo - Apollo x4"
                if 'ua_apollo' in line:
                    parts = line.strip().split()
                    if parts:
                        try:
                            card_num = int(parts[0])
                            return f'hw:{card_num},0'
                        except ValueError:
                            pass
    except (FileNotFoundError, PermissionError):
        pass
    return None


class AlsaMeter:
    """Software audio metering from ALSA PCM capture.

    Runs a background thread that continuously reads from the ALSA capture
    device, computing per-channel peak levels in dBFS.  Thread-safe access
    via get_input_meter() and get_output_meter().

    Apollo x4 capture provides 22 channels (hardware inputs).
    Playback metering (24 channels) is not yet supported — output meters
    return silence.  Future: tap PipeWire monitor source or add a driver
    ioctl for playback DMA buffer peek.
    """

    channel_ids: list = []   # record channel IDs (see read_record_channel_ids)

    def __init__(self, device=None, capture_channels=22,
                 rate=48000, period_frames=480):
        self._device = device or find_alsa_device()
        self._capture_ch = capture_channels
        self._rate = rate
        self._period_frames = period_frames

        # Per-channel state (capture inputs)
        self._levels = [SILENCE_DB] * capture_channels
        self._peaks = [SILENCE_DB] * capture_channels
        self._peak_times = [0.0] * capture_channels
        self._clips = [False] * capture_channels
        self._clip_times = [0.0] * capture_channels

        self._lock = threading.Lock()
        self._running = False
        self._thread = None
        self._available = False

    @property
    def available(self):
        """True if ALSA device opened successfully."""
        return self._available

    def start(self):
        """Start the background capture-and-meter thread."""
        if not self._device:
            log.warning("No ALSA device specified or detected — "
                        "meters will show silence")
            return
        self._running = True
        self._thread = threading.Thread(
            target=self._capture_loop, daemon=True, name="alsa-meter")
        self._thread.start()

    def stop(self):
        """Stop the background capture thread."""
        self._running = False
        if self._thread:
            self._thread.join(timeout=2.0)
            self._thread = None

    def get_input_meter(self, channel):
        """Get meter values for an input (capture) channel.

        Args:
            channel: 0-based channel index (0..capture_channels-1)

        Returns:
            (level_db, peak_hold_db, clip) — dBFS floats + clip bool
        """
        now = time.monotonic()
        with self._lock:
            if channel < 0 or channel >= len(self._levels):
                return (SILENCE_DB, SILENCE_DB, False)
            level = self._levels[channel]

            peak = self._held_peak(channel, now)

            # Clip hold
            clip = self._clips[channel]
            if clip and now - self._clip_times[channel] > CLIP_HOLD_SEC:
                clip = False

        return (level, peak, clip)

    def get_channel_meter(self, channel):
        """(level_db, peak_hold_db, clip) of ALSA record channel `channel`."""
        return self.get_input_meter(channel)

    def _held_peak(self, c, now):
        """Peak-hold reading of channel c (caller holds the lock): the peak,
        held PEAK_HOLD_SEC after the level last reached it, then decaying,
        never below the current level."""
        peak = self._peaks[c]
        elapsed = now - self._peak_times[c]
        if elapsed > PEAK_HOLD_SEC:
            peak -= PEAK_DECAY_DB_SEC * (elapsed - PEAK_HOLD_SEC)
        return max(self._levels[c], peak, SILENCE_DB)

    def get_output_meter(self, channel):
        """Get meter values for an output (playback) channel.

        Not yet implemented — returns silence.  Output metering requires
        tapping the playback stream (PipeWire monitor or driver ioctl).
        """
        return (SILENCE_DB, SILENCE_DB, False)

    def _capture_loop(self):
        """Background thread: open ALSA capture, read frames, compute peaks."""
        try:
            import alsaaudio
        except ImportError:
            log.error("pyalsaaudio not installed — meters disabled "
                      "(pip install pyalsaaudio)")
            return

        pcm = None
        try:
            pcm = alsaaudio.PCM(
                type=alsaaudio.PCM_CAPTURE,
                mode=alsaaudio.PCM_NONBLOCK,
                device=self._device,
                channels=self._capture_ch,
                rate=self._rate,
                format=alsaaudio.PCM_FORMAT_S32_LE,
                periodsize=self._period_frames)
            self._available = True
            log.info("ALSA metering started: device=%s ch=%d rate=%d "
                     "period=%d",
                     self._device, self._capture_ch, self._rate,
                     self._period_frames)
        except Exception as e:
            log.error("Failed to open ALSA capture %s: %s",
                      self._device, e)
            return

        overrun_count = 0
        while self._running:
            try:
                length, data = pcm.read()
            except Exception as e:
                log.error("ALSA read error: %s", e)
                time.sleep(0.1)
                continue

            if length > 0:
                self._process_capture(data, length)
            elif length == -32:  # -EPIPE (buffer overrun)
                overrun_count += 1
                if overrun_count % 100 == 1:
                    log.debug("ALSA capture overrun (count=%d)", overrun_count)
            else:
                # No data ready (non-blocking), sleep briefly
                time.sleep(0.002)

        pcm.close()
        log.info("ALSA metering stopped")

    def _process_capture(self, data, frames):
        """Compute per-channel peak from interleaved S32_LE PCM data."""
        ch = self._capture_ch
        expected_bytes = frames * ch * 4
        if len(data) < expected_bytes:
            return

        # Parse interleaved S32_LE samples
        samples = array.array('i')  # signed 32-bit
        samples.frombytes(data[:expected_bytes])

        now = time.monotonic()

        # Per-channel peak via fast slice + min/max (C-level operations)
        with self._lock:
            for c in range(ch):
                channel_data = samples[c::ch]
                if not channel_data:
                    continue
                peak_pos = max(channel_data)
                peak_neg = min(channel_data)
                peak_abs = max(peak_pos, -peak_neg)

                # Convert to dBFS
                if peak_abs > 0:
                    linear = peak_abs / S32_MAX
                    db = max(SILENCE_DB, 20.0 * math.log10(linear))
                else:
                    db = SILENCE_DB

                # Clip: within 0.1 dB of 0 dBFS
                self._update_channel(c, db, db >= -0.1, now)

    def _update_channel(self, c, db, clip, now):
        """Take a new level for channel c (caller holds the lock)."""
        # Peak hold: reaching the held peak again restarts the hold (a
        # steady signal keeps it), so it only decays once the level has
        # dropped for PEAK_HOLD_SEC
        if db >= self._held_peak(c, now):
            self._peaks[c] = db
            self._peak_times[c] = now
        self._levels[c] = db
        if clip:
            self._clips[c] = True
            self._clip_times[c] = now


def find_apollo_card():
    """ALSA card number of the first ua_apollo device, or None."""
    for card in sorted(glob.glob("/sys/class/sound/card*")):
        driver = os.path.realpath(os.path.join(card, "device", "driver"))
        if os.path.basename(driver) == "ua_apollo":
            try:
                return int(card.rsplit("card", 1)[1])
            except ValueError:
                pass
    return None


def read_record_channel_ids(card):
    """Channel IDs of the Apollo's ALSA record channels, in ALSA order.

    Read from the driver's /proc/asound/cardN/io_map (both layouts list
    them).  An ID is (type << 8) | number: type 0x00/0x01/0x03 analog
    inputs, 0x02 AUX buses, 0x04 ADAT, 0x05 S/PDIF, 0x07 Virtual, 0x09
    MON, 0x0b Talkback.  Returns [] when the file can't be read.
    """
    ids = []
    try:
        with open(f"/proc/asound/card{card}/io_map") as f:
            in_record = False
            for line in f:
                if line.startswith("record:"):
                    in_record = True
                    continue
                if in_record:
                    m = re.match(r"\s+ch\s+\d+\s+slot\s+\d+\s+0x([0-9a-f]+)",
                                 line)
                    if not m:
                        break
                    ids.append(int(m.group(1), 16))
    except OSError:
        return []
    return ids


def apollo_stream_owner(card):
    """(pid, process name) holding the Apollo's PCM streams, or None."""
    for status in glob.glob(f"/proc/asound/card{card}/pcm*/sub*/status"):
        try:
            with open(status) as f:
                m = re.search(r"owner_pid\s*:\s*(\d+)", f.read())
            if m:
                pid = int(m.group(1))
                with open(f"/proc/{pid}/comm") as f:
                    return pid, f.read().strip()
        except OSError:
            continue
    return None


def pipewire_env(pid):
    """Environment that reaches the PipeWire instance running as `pid`'s
    user, also from a daemon running as root."""
    try:
        uid = os.stat(f"/proc/{pid}").st_uid
    except OSError:
        return None
    return dict(os.environ, XDG_RUNTIME_DIR=f"/run/user/{uid}")


def find_pipewire_capture_node(env):
    """node.name of the Apollo's PipeWire capture node, or None."""
    try:
        out = subprocess.run(["pw-dump"], env=env, capture_output=True,
                             timeout=10).stdout
        objs = json.loads(out or b"[]")
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None
    for obj in objs:
        props = (obj.get("info") or {}).get("props") or {}
        if (props.get("alsa.driver_name") == "ua_apollo"
                and props.get("media.class") == "Audio/Source"):
            return props.get("node.name")
    return None


class PipeWireMeter(AlsaMeter):
    """Software metering from PipeWire while PipeWire holds the Apollo.

    The ALSA capture device can't be opened a second time while PipeWire
    streams from it, so this records the Apollo's capture node through
    pw-record (raw S32, every record channel) instead, and computes the
    same per-channel peaks as AlsaMeter.  Meters are per ALSA record
    channel; channel_ids says which input or bus each one carries.
    """

    def __init__(self, card, env, rate=48000, period_frames=480):
        self._card = card
        self._env = env
        self.channel_ids = read_record_channel_ids(card)
        super().__init__(device=f"card{card}",
                         capture_channels=len(self.channel_ids),
                         rate=rate, period_frames=period_frames)

    def _capture_loop(self):
        while self._running:
            node = find_pipewire_capture_node(self._env)
            ch = self._capture_ch
            if not node or not ch:
                time.sleep(2.0)
                continue
            cmd = ["pw-record", "--target", node, "--raw",
                   "--format", "s32", "--channels", str(ch),
                   "--rate", str(self._rate), "--latency", "10ms", "-"]
            try:
                proc = subprocess.Popen(cmd, env=self._env,
                                        stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL)
            except OSError as e:
                log.error("pw-record failed: %s — meters disabled", e)
                return
            self._available = True
            log.info("PipeWire metering started: node=%s ch=%d rate=%d",
                     node, ch, self._rate)
            period = self._period_frames * ch * 4
            next_check = time.monotonic() + 5.0
            while self._running:
                data = proc.stdout.read(period)
                if len(data) < period:
                    break       # pw-record exited (PipeWire restart, ...)
                self._process_capture(data, self._period_frames)
                if time.monotonic() >= next_check:
                    next_check = time.monotonic() + 5.0
                    if read_record_channel_ids(self._card) != self.channel_ids:
                        break   # layout changed (rate band, TOSLink mode)
            proc.terminate()
            try:
                proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                proc.kill()
            self._available = False
            ids = read_record_channel_ids(self._card)
            if ids and ids != self.channel_ids:
                with self._lock:
                    self._resize(ids)
            if self._running:
                time.sleep(1.0)
        log.info("PipeWire metering stopped")

    def _resize(self, ids):
        """Take a new record channel list (caller holds the lock)."""
        n = len(ids)
        self.channel_ids = ids
        self._capture_ch = n
        self._levels = [SILENCE_DB] * n
        self._peaks = [SILENCE_DB] * n
        self._peak_times = [0.0] * n
        self._clips = [False] * n
        self._clip_times = [0.0] * n


_ANALOG_TYPES = (0x00, 0x01, 0x03)   # MIC/LINE/HIZ, MIC/LINE, LINE


def meter_channel(kind, index, meter_idx, name, stereo, channel_ids):
    """ALSA record channel that carries a device-map meter, or None.

    kind is 'inputs', 'outputs' or 'auxs'; name is the channel's Name in
    the device map ("ANALOG 3", "S/PDIF L", "VIRTUAL 2", "TALKBACK",
    "MONITOR", ...).  meters/1 of a stereo input reads the right channel.
    Only what the Apollo records has a meter here: analog, ADAT, S/PDIF,
    Virtual and Talkback inputs, the monitor bus and the AUX buses.
    """
    name = (name or "").upper()
    want = []
    if kind == "inputs":
        m = re.match(r"(ANALOG|ADAT|VIRTUAL) (\d+)$", name)
        if m:
            n = int(m.group(2)) + (1 if stereo and meter_idx == 1 else 0)
            types = {"ANALOG": _ANALOG_TYPES, "ADAT": (0x04,),
                     "VIRTUAL": (0x07,)}[m.group(1)]
            want = [(t << 8) | n for t in types]
        elif name in ("S/PDIF L", "S/PDIF R"):
            right = name.endswith("R") or (stereo and meter_idx == 1)
            want = [0x0502 if right else 0x0501]
        elif name == "TALKBACK":
            want = [0x0b01 + min(meter_idx, 1)]
    elif kind == "outputs":
        if name == "MONITOR" and meter_idx < 2:
            want = [0x0901 + meter_idx]
    elif kind == "auxs" and meter_idx < 2:
        want = [0x0200 | (index * 2 + 1 + meter_idx)]
    for cid in want:
        if cid in channel_ids:
            return channel_ids.index(cid)
    return None


# Level meters the DSP writes into the hardware readback block, as the Mixer
# Engine reads them (CMxMixer::MeterTimerCallback_Base, through the kext's
# readback copy; Apollo x4).  (word, shift, bits, channel ID): the 16-bit
# meters carry the even channel in bits 31:16 and the odd one in 15:0, the
# 12-bit bus meters use bits 27:16 and 11:0.  Channel IDs as in the io_map.
X4_READBACK_METERS = (
    (8, 16, 16, 0x0001), (8, 0, 16, 0x0002),        # analog 1-4
    (9, 16, 16, 0x0003), (9, 0, 16, 0x0004),
    (12, 16, 16, 0x0401), (12, 0, 16, 0x0402),      # ADAT 1-8
    (13, 16, 16, 0x0403), (13, 0, 16, 0x0404),
    (14, 16, 16, 0x0405), (14, 0, 16, 0x0406),
    (15, 16, 16, 0x0407), (15, 0, 16, 0x0408),
    (16, 16, 16, 0x0501), (16, 0, 16, 0x0502),      # S/PDIF L/R
    (17, 16, 16, 0x0b01),                           # talkback
    (18, 16, 12, 0x0901), (18, 0, 12, 0x0902),      # monitor L/R
    (19, 16, 12, 0x0201), (19, 0, 12, 0x0202),      # AUX 1 L/R
    (20, 16, 12, 0x0203), (20, 0, 12, 0x0204),      # AUX 2 L/R
    (22, 16, 16, 0x0701), (22, 0, 16, 0x0702),      # Virtual 1-8
    (23, 16, 16, 0x0703), (23, 0, 16, 0x0704),
    (24, 16, 16, 0x0705), (24, 0, 16, 0x0706),
    (25, 16, 16, 0x0707), (25, 0, 16, 0x0708),
)


class ReadbackMeter(AlsaMeter):
    """Hardware meters: the levels the DSP writes into readback words 8-25.

    What the Mixer Engine shows on macOS: no audio capture involved, so it
    works whoever holds the streams, and it covers every input (Virtual
    included) and the monitor and AUX buses.  The daemon feeds it each
    readback it polls (feed()).
    """

    def __init__(self, table=X4_READBACK_METERS):
        self._table = table
        self.channel_ids = [m[3] for m in table]
        super().__init__(device="readback", capture_channels=len(table))
        self._available = True

    def start(self):
        pass

    def stop(self):
        pass

    def feed(self, words):
        """Take one readback snapshot (40 words)."""
        # The Mixer Engine skips a snapshot whose check words read all-ones
        if len(words) < 40 or 0xFFFFFFFF in (words[38], words[39]):
            return
        now = time.monotonic()
        with self._lock:
            for c, (word, shift, bits, _cid) in enumerate(self._table):
                full = (1 << bits) - 1
                v = (words[word] >> shift) & full
                if bits == 16:
                    clip = v >= 0xFFFE
                    lin = v / 65535.0
                else:
                    clip = v == full
                    lin = v / 4095.0
                db = 20.0 * math.log10(lin) if lin >= 1e-4 else SILENCE_DB
                self._update_channel(c, max(SILENCE_DB, db), clip, now)


def make_meter(alsa_device=None, device_type=None):
    """The meter that can work right now.

    ReadbackMeter on the Apollo x4 (device type 0x1F), whose readback meter
    layout is known; else PipeWireMeter when PipeWire holds the Apollo's
    streams (the usual desktop case), else AlsaMeter on the capture device
    directly.
    """
    if device_type == 0x1F and not alsa_device:
        return ReadbackMeter()
    card = find_apollo_card()
    if card is not None and not alsa_device:
        owner = apollo_stream_owner(card)
        if owner and owner[1] == "pipewire":
            env = pipewire_env(owner[0])
            if env and read_record_channel_ids(card):
                return PipeWireMeter(card, env)
    ids = read_record_channel_ids(card) if card is not None else []
    meter = AlsaMeter(device=alsa_device, capture_channels=len(ids) or 22)
    meter.channel_ids = ids
    return meter


class NullMeter:
    """Stub meter returning silence for all channels.

    Used when --no-hardware or ALSA is unavailable.
    """

    @property
    def available(self):
        return False

    def start(self):
        pass

    def stop(self):
        pass

    def get_input_meter(self, channel):
        return (SILENCE_DB, SILENCE_DB, False)

    def get_output_meter(self, channel):
        return (SILENCE_DB, SILENCE_DB, False)

    def get_channel_meter(self, channel):
        return (SILENCE_DB, SILENCE_DB, False)

    channel_ids: list = []
