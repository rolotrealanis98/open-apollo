"""Consumer for the driver's event channel (parity L1).

/dev/ua_apolloN delivers `struct ua_event {u32 code; u32 arg; u64 time_ns}`
records through read()/poll() on any open file (driver/ua_ioctl.h).  The
daemon opens the node a second time, read-only and non-blocking, and feeds
the fd to the asyncio loop, so a rate change, a clock-lock change or an
unplug reaches the clients when it happens instead of at the next poll.

DriverEvents only reads and decodes; what each event means is the daemon's
business (ua_mixer_daemon.MixerDaemon._on_driver_event).  An old driver has
no read() on the node (EINVAL), or an open that fails: open() then returns
False and the daemon keeps polling exactly as before.
"""

import errno
import logging
import os
import struct

log = logging.getLogger("ua-mixer-daemon")

EVENT = struct.Struct("=IIQ")   # code, arg, time_ns (CLOCK_MONOTONIC)

# Kext codes (arg means what it does there)
UA_EVT_RATE_INFO = 0x64       # firmware rate/lock info changed; arg = raw status
UA_EVT_IODESC_IN = 0x65       # input IO descriptor republished
UA_EVT_IODESC_OUT = 0x66      # output IO descriptor republished
UA_EVT_RATE_CHANGED = 0x67    # arg = rate index (1 = 44.1k .. 6 = 192k)
UA_EVT_CLOCK_CHANGED = 0x68   # arg = clock source (UA_CLOCK_*)
UA_EVT_NOTIF_STATUS = 0x6E    # raw notification status
# Linux-only codes
UA_EVT_DEVICE_OFFLINE = 0x1000   # arg 1 = surprise removal, 0 = unbind
UA_EVT_CONNECTED = 0x1001        # probe, resume, reconnect
UA_EVT_LAYOUT_CHANGED = 0x1002   # arg = (play_hw_channels << 16) | rec_hw_channels
UA_EVT_EVENTS_LOST = 0x1003      # arg = events dropped
UA_EVT_CLOCK_LOCK = 0x1004       # arg = 1 locked, 0 unlocked
UA_EVT_PAUSED = 0x1005           # arg = the UA_PAUSE_* bit that was set
UA_EVT_RESUMED = 0x1006          # arg = the UA_PAUSE_* bit that was cleared

# Pause reasons (driver/ua_ioctl.h).  UA_PAUSE_DEVICE is system suspend and
# the moment before a unit is removed; the others are the driver's own
# audio housekeeping (rate change, phase, idle) and do not concern the mixer.
UA_PAUSE_PHASE = 0x1
UA_PAUSE_RATE = 0x2
UA_PAUSE_DEVICE = 0x4
UA_PAUSE_IDLE = 0x8

EVENT_NAMES = {
    UA_EVT_RATE_INFO: "RATE_INFO",
    UA_EVT_IODESC_IN: "IODESC_IN",
    UA_EVT_IODESC_OUT: "IODESC_OUT",
    UA_EVT_RATE_CHANGED: "RATE_CHANGED",
    UA_EVT_CLOCK_CHANGED: "CLOCK_CHANGED",
    UA_EVT_NOTIF_STATUS: "NOTIF_STATUS",
    UA_EVT_DEVICE_OFFLINE: "DEVICE_OFFLINE",
    UA_EVT_CONNECTED: "CONNECTED",
    UA_EVT_LAYOUT_CHANGED: "LAYOUT_CHANGED",
    UA_EVT_EVENTS_LOST: "EVENTS_LOST",
    UA_EVT_CLOCK_LOCK: "CLOCK_LOCK",
    UA_EVT_PAUSED: "PAUSED",
    UA_EVT_RESUMED: "RESUMED",
}

# read() errors that mean "the device went away"
_GONE_ERRNOS = (errno.ENODEV, errno.ENXIO, errno.EIO, errno.EBADF)
# ... and "this driver has no event channel" (no read() op)
_UNSUPPORTED_ERRNOS = (errno.EINVAL, errno.ENOSYS, errno.ENOTTY,
                       errno.EOPNOTSUPP)

# drain() results
OK = "ok"
OFFLINE = "offline"
UNSUPPORTED = "unsupported"

_READ_EVENTS = 32      # records per read()
_MAX_READS = 16        # per drain(), so a flood cannot starve the loop


def decode_events(data: bytes) -> list[tuple[int, int, int]]:
    """Split a read() result into (code, arg, time_ns); a partial trailing
    record (never produced by the driver) is dropped."""
    return [EVENT.unpack_from(data, off)
            for off in range(0, len(data) - EVENT.size + 1, EVENT.size)]


class DriverEvents:
    """One read-only, non-blocking open of the device node's event channel."""

    def __init__(self, on_event):
        self.on_event = on_event     # on_event(code, arg, time_ns)
        self.fd = -1
        self.path: str | None = None

    @property
    def active(self) -> bool:
        return self.fd >= 0

    def open(self, path: str) -> bool:
        """Open `path` and check that it delivers events.  False (nothing
        left open) when the open fails or the driver has no event channel;
        events already queued are dispatched."""
        self.close()
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
        except OSError as e:
            log.debug("event channel: open %s: %s", path, e)
            return False
        self.fd = fd
        self.path = path
        # An old driver opens fine and then answers read() with EINVAL;
        # poll() on it would report "readable" forever.  Reading once tells.
        if self.drain() != OK:
            log.debug("event channel: %s has no event channel", path)
            self.close()
            return False
        return True

    def close(self):
        if self.fd >= 0:
            try:
                os.close(self.fd)
            except OSError:
                pass
        self.fd = -1

    def drain(self) -> str:
        """Read and dispatch everything queued.  OK when the queue is empty,
        OFFLINE when the device is gone (the fd stays open: close() it),
        UNSUPPORTED when read() is not implemented."""
        if self.fd < 0:
            return OFFLINE
        for _ in range(_MAX_READS):
            if self.fd < 0:     # a handler closed us (device lost)
                return OFFLINE
            try:
                data = os.read(self.fd, EVENT.size * _READ_EVENTS)
            except BlockingIOError:
                return OK
            except InterruptedError:
                continue
            except OSError as e:
                if e.errno in _GONE_ERRNOS:
                    return OFFLINE
                if e.errno in _UNSUPPORTED_ERRNOS:
                    return UNSUPPORTED
                log.warning("event channel: read: %s", e)
                return OFFLINE
            if not data:
                return OFFLINE      # EOF
            for code, arg, ns in decode_events(data):
                try:
                    self.on_event(code, arg, ns)
                except Exception:
                    log.exception("event handler failed for 0x%x", code)
        return OK
