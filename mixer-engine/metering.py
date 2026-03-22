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
import logging
import math
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
        if channel < 0 or channel >= self._capture_ch:
            return (SILENCE_DB, SILENCE_DB, False)

        now = time.monotonic()
        with self._lock:
            level = self._levels[channel]

            # Peak hold with decay
            peak = self._peaks[channel]
            elapsed = now - self._peak_times[channel]
            if elapsed > PEAK_HOLD_SEC:
                decay = PEAK_DECAY_DB_SEC * (elapsed - PEAK_HOLD_SEC)
                peak = max(level, peak - decay)
                if peak <= SILENCE_DB:
                    peak = SILENCE_DB

            # Clip hold
            clip = self._clips[channel]
            if clip and now - self._clip_times[channel] > CLIP_HOLD_SEC:
                clip = False

        return (level, peak, clip)

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

                self._levels[c] = db

                # Clip detection (within 0.1 dB of 0 dBFS)
                if db >= -0.1:
                    self._clips[c] = True
                    self._clip_times[c] = now

                # Peak hold: new peak resets hold timer
                if db > self._peaks[c]:
                    self._peaks[c] = db
                    self._peak_times[c] = now
                elif (now - self._peak_times[c] >
                      PEAK_HOLD_SEC + 5.0):
                    # After hold + 5s grace, snap to current level
                    self._peaks[c] = db
                    self._peak_times[c] = now


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
