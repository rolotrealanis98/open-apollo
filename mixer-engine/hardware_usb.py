"""USB backend for the Apollo Solo USB, Twin USB and Twin X USB (N33/U39, J2).

The daemon talks to a Thunderbolt Apollo through /dev/ua_apollo0 ioctls
(hardware.HardwareBackend).  A USB Apollo has no kernel driver for its
vendor interface (interface 0), so this backend does in user space what
the PCIe driver does in the kernel, with the same method names, so the
HardwareRouter and the daemon run unchanged on it:

  set_mixer_param()   the PCIe driver's SetMixerParam mapping
                      (tools/uausb/mixer.py) into the 38-setting one-shot
                      mailbox, flushed with the SEQ_WR/SEQ_RD handshake over
                      vendor requests 0x03/0x04 (settings at 0x062D...)
  get_hw_readback()   the 40-word readback, request 0x10 (or 0x0604/0x0605
                      on an old protocol word), check words verified; the
                      daemon's readback decode and ReadbackMeter take it
                      as they take the PCIe one (J2)
  get_device_info()   identity from the vendor requests, with a device_type
                      outside the PCIe range (USB_DEVICE_TYPE_BASE | PID)
                      that find_descriptor() resolves to devices/<model>.json

Off by default, opt-in with a constructor flag (daemon options):
  dsp_params      send mixer bus coefficients and DSP input params as DSP
                  commands ({0x1D0004...}, {0x1E0004...}) on the bulk pipe,
                  the way the UA driver's command layer would.  The USB
                  captures contain no such command: unverified.
  knob_writeback  when the readback shows the front panel moved the
                  monitor level/mute/mono/dim without a host write, write
                  that value back into setting 2 (issue #59).  No capture
                  shows what the UA driver does on a knob turn: unverified.

The daemon picks this backend with --usb, or by itself when no
/dev/ua_apollo* node exists and a USB Apollo (VID 0x2B5A, PID 0x000D,
0x0002 or 0x000F) is on the bus (select_backend()).  It starts talking to
the device only after the udev DSP init has finished
(/run/ua-usb-dsp-init.done), and it does not claim interface 0 for its
vendor requests (usbfs needs no claim for a vendor-type control
transfer), so tools/usb-full-init.py keeps the interface to itself.  The
user running the daemon needs access to the USB device node (root, or a
uaccess udev rule for 2b5a:000d/0002/000f).

UNTESTED ON HARDWARE: every path runs against a fake device in
mixer-engine/tests/test_hardware_usb.py; the settings writes are checked
byte for byte against a Windows capture of a Solo USB.
"""

from __future__ import annotations

import glob
import json
import logging
import os
import struct
import sys
import threading
import time
from pathlib import Path

log = logging.getLogger(__name__)

_TOOLS = Path(__file__).resolve().parent.parent / "tools"
if _TOOLS.is_dir() and str(_TOOLS) not in sys.path:
    sys.path.append(str(_TOOLS))   # last: shadows nothing

from uausb import device as udev  # noqa: E402
from uausb import dspcmd  # noqa: E402
from uausb import mixer as umix  # noqa: E402

DEVICES_DIR = Path(__file__).resolve().parent.parent / "devices"

UA_VID = 0x2B5A
# live PID -> (model name, devices/<slug>.json or None)
USB_MODELS = {
    0x000D: ("Apollo Solo USB", "apollo-solo-usb"),
    0x0002: ("Apollo Twin USB", None),          # no descriptor exists yet
    0x000F: ("Apollo Twin X USB", "apollo-twin-x-usb"),
}
LIVE_PIDS = {pid: name for pid, (name, _slug) in USB_MODELS.items()}

# device_type the daemon sees for a USB Apollo: above every PCIe type
USB_DEVICE_TYPE_BASE = 0x10000

INIT_STAMP = "/run/ua-usb-dsp-init.done"
# Open anyway when the device has been on the bus this long without the
# stamp (no udev init installed, or it failed and the user ran it by hand)
INIT_STAMP_GRACE = 60.0

EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
INTERFACE = 0

FLUSH_INTERVAL = 0.01           # retry a busy flush every 10 ms
FLUSH_GIVE_UP = 3.0             # ... for this long, then wait for the next trigger


def usb_device_type(pid: int) -> int:
    return USB_DEVICE_TYPE_BASE | (pid & 0xFFFF)


def is_usb_device_type(device_type) -> bool:
    return isinstance(device_type, int) and \
        (device_type & ~0xFFFF) == USB_DEVICE_TYPE_BASE


def descriptor_for_device_type(device_type) -> tuple:
    """(slug, model) of a USB device_type, as find_descriptor() returns."""
    if not is_usb_device_type(device_type):
        return None, None
    name, slug = USB_MODELS.get(device_type & 0xFFFF, (None, None))
    if slug and not (DEVICES_DIR / f"{slug}.json").exists():
        slug = None
    return slug, name


def find_usb_apollos(sysfs: str = "/sys/bus/usb/devices") -> list:
    """[(pid, busnum, devnum, sysfs path)] of live USB Apollos (no pyusb)."""
    found = []
    for d in sorted(glob.glob(os.path.join(sysfs, "*"))):
        try:
            with open(os.path.join(d, "idVendor")) as f:
                if int(f.read().strip(), 16) != UA_VID:
                    continue
            with open(os.path.join(d, "idProduct")) as f:
                pid = int(f.read().strip(), 16)
            busnum = devnum = None
            with open(os.path.join(d, "busnum")) as f:
                busnum = int(f.read().strip())
            with open(os.path.join(d, "devnum")) as f:
                devnum = int(f.read().strip())
        except (OSError, ValueError):
            continue
        if pid in USB_MODELS:
            found.append((pid, busnum, devnum, d))
    return found


def select_backend(device: str | None, force_usb: bool = False,
                   pcie_nodes=None, usb_apollos=None) -> str:
    """'usb' or 'pcie': --usb, or no /dev/ua_apollo* node and a USB Apollo
    on the bus (an explicit --device always means PCIe)."""
    if force_usb:
        return "usb"
    if device:
        return "pcie"
    nodes = glob.glob("/dev/ua_apollo*") if pcie_nodes is None else pcie_nodes
    if nodes:
        return "pcie"
    apollos = find_usb_apollos() if usb_apollos is None else usb_apollos
    return "usb" if apollos else "pcie"


def load_model(slug: str | None) -> dict:
    if not slug:
        return {}
    try:
        desc = json.loads((DEVICES_DIR / f"{slug}.json").read_text())
        return desc if isinstance(desc, dict) else {}
    except (OSError, ValueError):
        return {}


class PyUsbOpener:
    """Finds and opens a live USB Apollo with pyusb.  Returns an object with
    ctrl_transfer/read/write (the pyusb device) plus claim()/release().

    Interface 0 is not claimed for the vendor control requests: usbfs lets
    a vendor-type request through without a claim (check_ctrlrecip), so the
    daemon never holds the interface tools/usb-full-init.py needs.  Only
    the bulk pipe (--usb-dsp-params) claims it, on first use."""

    _warned = False

    def _warn_once(self, msg):
        # opened again on every hot-plug poll: one line, not one per poll
        if not self._warned:
            self._warned = True
            log.error(msg)

    def __call__(self, pids):
        try:
            import usb.core
            import usb.util
        except ImportError:
            self._warn_once("USB backend needs pyusb (pip install pyusb)")
            return None
        for pid in pids:
            try:
                dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
            except Exception as e:  # noqa: BLE001
                if not isinstance(e, getattr(usb.core, "NoBackendError", ())):
                    raise
                # pyusb is there but libusb is not: no USB device to open
                self._warn_once("USB backend needs libusb (install "
                                "libusb-1.0): no USB Apollo can be opened")
                return None
            if dev is not None:
                return _PyUsbHandle(dev, pid, usb)
        return None


class _PyUsbHandle:
    def __init__(self, dev, pid, usb):
        self.dev = dev
        self.pid = pid
        self._usb = usb
        self.claimed = False
        self.bus = getattr(dev, "bus", None)
        self.address = getattr(dev, "address", None)

    def ctrl_transfer(self, *a, **kw):
        return self.dev.ctrl_transfer(*a, **kw)

    def claim(self):
        if self.claimed:
            return
        try:
            if self.dev.is_kernel_driver_active(INTERFACE):
                self.dev.detach_kernel_driver(INTERFACE)
        except (self._usb.core.USBError, NotImplementedError):
            pass
        self._usb.util.claim_interface(self.dev, INTERFACE)
        self.claimed = True

    def write(self, *a, **kw):
        return self.dev.write(*a, **kw)

    def read(self, *a, **kw):
        return self.dev.read(*a, **kw)

    def release(self):
        if self.claimed:
            try:
                self._usb.util.release_interface(self.dev, INTERFACE)
            except Exception:  # noqa: BLE001 - device may be gone
                pass
            self.claimed = False
        try:
            self._usb.util.dispose_resources(self.dev)
        except Exception:  # noqa: BLE001
            pass


class HardwareUSB:
    """The daemon's hardware backend for a USB Apollo (see module doc)."""

    def __init__(self, device: str | None = None, safe_mode: bool = False,
                 opener=None, auto_flush: bool = True,
                 knob_writeback: bool = False, dsp_params: bool = False,
                 init_stamp: str | None = INIT_STAMP,
                 stamp_grace: float = INIT_STAMP_GRACE,
                 clock=time.monotonic, sleep=time.sleep):
        self.device_path = None
        self.safe_mode = safe_mode
        self.connected = False
        self.lost = False
        self.paused = False
        self.last_errno = 0
        self.pid = None
        self.model_name = ""
        self.slug = None
        self.model = {}
        self.num_preamps = 2
        self.num_hiz = 1
        self.protocol = None
        self.versions = {}
        self.knob_writeback = knob_writeback
        self.dsp_params = dsp_params
        self.init_stamp = init_stamp
        self.stamp_grace = stamp_grace
        self._opener = opener or PyUsbOpener()
        self._handle = None
        self._regs = None
        self._mailbox = umix.SettingsMailbox()
        self._lock = threading.RLock()
        self._auto_flush = auto_flush
        self._flusher = None
        self._clock = clock
        self._sleep = sleep
        self._first_seen = None
        self._bulk_seq = 0
        self._rb_last = None
        self._host_write_t = 0.0
        self._knob_seen = {}
        self._warned = set()

    # ── open / close / hot-plug ───────────────────────────────────────

    def _init_done(self) -> bool:
        """The udev DSP init has finished, there is none to wait for, or the
        device has been present for stamp_grace seconds without it."""
        if not self.init_stamp or os.path.exists(self.init_stamp):
            return True
        now = self._clock()
        if self._first_seen is None:
            self._first_seen = now
        if now - self._first_seen >= self.stamp_grace:
            self._warn_once("stamp", "%s not present after %.0f s: opening the "
                            "USB Apollo anyway (udev DSP init missing?)",
                            self.init_stamp, self.stamp_grace)
            return True
        return False

    def open(self, device_path: str | None = None) -> bool:
        with self._lock:
            # Finding the device sends nothing to it (no claim, no request)
            handle = self._opener(list(USB_MODELS))
            if handle is None:
                self._first_seen = None     # grace counts while it is present
                return False
            if not self._init_done():
                log.info("USB Apollo: waiting for the DSP init (%s)",
                         self.init_stamp)
                try:
                    handle.release()
                except Exception:  # noqa: BLE001
                    pass
                return False
            return self._attach(handle)

    def _attach(self, handle) -> bool:
        self._handle = handle
        self._regs = udev.UsbRegs(handle)
        self.pid = getattr(handle, "pid", None)
        self.model_name, self.slug = USB_MODELS.get(self.pid, ("USB Apollo", None))
        self.model = load_model(self.slug)
        self.num_preamps = int(self.model.get("preamps", 2) or 2)
        self.num_hiz = int(self.model.get("hiz", 1) or 0)
        bus, addr = getattr(handle, "bus", None), getattr(handle, "address", None)
        self.device_path = f"usb:{UA_VID:04x}:{(self.pid or 0):04x}" + (
            f"@{bus}-{addr}" if bus is not None else "")
        try:
            self.versions = self._regs.versions()
            self.protocol = self.versions.get("protocol")
            self._mailbox = umix.SettingsMailbox()
            self._mailbox.adopt_seq(self._regs)
        except Exception as e:  # noqa: BLE001 - any transport error
            log.error("USB Apollo %s: no answer on interface 0: %s",
                      self.device_path, e)
            self._drop_handle()
            return False
        self.connected = True
        self.lost = False
        self._rb_last = None
        self._knob_seen = {}
        log.info("Opened %s (%s): protocol %#010x, versions %#010x/%#010x, "
                 "SEQ %d", self.model_name, self.device_path,
                 self.protocol or 0, self.versions.get("version_a", 0),
                 self.versions.get("version_b", 0), self._mailbox.seq_wr)
        return True

    def _drop_handle(self):
        if self._handle is not None:
            try:
                self._handle.release()
            except Exception:  # noqa: BLE001
                pass
        self._handle = None
        self._regs = None

    def close(self):
        with self._lock:
            self._drop_handle()
            self.connected = False

    def probe(self) -> bool:
        """The device still answers (one 4-byte vendor read)."""
        with self._lock:
            if not self.connected or self._regs is None:
                return False
            try:
                self._regs.protocol_word()
                return True
            except Exception:  # noqa: BLE001 - gone or wedged
                return False

    def mark_lost(self, reason: str = ""):
        with self._lock:
            self._drop_handle()
            self.connected = False
            self.lost = True
            self._first_seen = None
        log.warning("USB Apollo %s lost%s", self.device_path,
                    f" ({reason})" if reason else "")

    def reopen(self) -> bool:
        return self.open()

    def _warn_once(self, key, msg, *args):
        if key not in self._warned:
            self._warned.add(key)
            log.warning(msg, *args)

    # ── identity ─────────────────────────────────────────────────────

    def get_device_info(self) -> dict | None:
        if not self.connected:
            return None
        return {
            "fpga_rev": self._read_quiet(udev.ID_VERSION) or 0,
            "device_type": usb_device_type(self.pid or 0),
            "subsystem_id": self.pid or 0,
            "num_dsps": 1 if self.pid == 0x000D else 0,
            "fw_version": self.versions.get("version_a", 0),
            "fw_v2": False,
            "serial": 0,
            "ext_caps": 0,
        }

    def get_system_info(self) -> dict | None:
        """The real serial (DSP 0 window +0x20) once the DSP wrote it."""
        if not self.connected:
            return None
        with self._lock:
            try:
                serial = self._regs.serial(0)
            except Exception:  # noqa: BLE001
                serial = None
        return {"serial": serial} if serial else None

    def _read_quiet(self, addr):
        with self._lock:
            try:
                return self._regs.read1(addr)
            except Exception:  # noqa: BLE001
                return None

    # The mixer kind of the USB parts is not known (devices/*.json carry
    # only mixer_type_id, which is not the kext's kind: x6 9 vs kind 10), so
    # the init sequence gates conservatively (HardwareRouter.gate_unknown_kind)
    gate_unknown_kind = True

    def features(self) -> int:
        return 0

    def has_feature(self, bit: int) -> bool:
        return False

    def get_version(self):
        return None

    def get_clock_sources(self):
        """None: the firmware menu (request 0x08) lists S/PDIF and ADAT on a
        Solo USB, which has neither, so the model's own list is used."""
        return None

    def read_clock_menu(self) -> list:
        """The firmware's clock menu (request 0x08), for diagnostics."""
        with self._lock:
            if not self.connected:
                return []
            return self._regs.clock_menu()

    # ── driver params ────────────────────────────────────────────────

    def get_driver_param(self, param_id: int):
        if not self.connected:
            return None
        if param_id == 0:               # sample rate, request 0x06
            with self._lock:
                try:
                    st = self._regs.status_block()
                except Exception:  # noqa: BLE001
                    return None
            return st["rate"] if st and st["rate"] else None
        return None

    def set_driver_param(self, param_id: int, value: int) -> bool:
        """Rate and clock changes are not sent over USB: a rate change needs
        the DSP init re-run and snd-usb-audio's streams moved with it."""
        self._warn_once(("drv", param_id),
                        "USB Apollo: driver param %d not supported over USB "
                        "(rate/clock changes need the USB init re-run)",
                        param_id)
        return False

    # ── mixer settings ───────────────────────────────────────────────

    def set_mixer_param(self, channel_type: int, channel_idx: int,
                        param_id: int, value: int) -> bool:
        if self.paused:
            return False
        if not self.connected:
            log.info("HW MIXER_PARAM (USB, no device): type=%d ch=%d "
                     "param=0x%02x value=%d", channel_type, channel_idx,
                     param_id, value)
            return False
        self.last_errno = 0
        try:
            res = umix.map_param(channel_type, channel_idx, param_id, value,
                                 self.num_preamps, self.num_hiz)
        except umix.MixerParamError as e:
            self.last_errno = 22        # EINVAL, as the PCIe ioctl
            log.debug("USB MIXER_PARAM rejected: %s", e)
            return False
        if res is None:
            return True
        if isinstance(res, umix.DspInputParam):
            return self._send_dsp_input_param(res)
        with self._lock:
            self._mailbox.merge(res.setting, res.value, res.mask)
            self._host_write_t = self._clock()
            log.debug("USB mixer[%d] value=0x%08x mask=0x%08x (type=%d ch=%d "
                      "param=0x%02x)", res.setting, res.value, res.mask,
                      channel_type, channel_idx, param_id)
        self._schedule_flush()
        return True

    def flush_settings(self) -> bool | None:
        """One flush attempt (see SettingsMailbox.flush)."""
        with self._lock:
            if not self.connected:
                return None
            try:
                return self._mailbox.flush(self._regs)
            except Exception as e:  # noqa: BLE001
                log.error("USB settings flush failed: %s", e)
                return False

    def _schedule_flush(self):
        """Flush from a short-lived thread FLUSH_INTERVAL from now, as the
        UA driver flushes from its readback timer: the three writes of a
        gain change then go out in one batch (one SEQ bump), as in the
        Windows capture.  Without auto_flush the caller flushes
        (flush_settings(), or get_hw_readback())."""
        if not self._auto_flush:
            return
        with self._lock:
            if self._flusher is not None and self._flusher.is_alive():
                return
            self._flusher = threading.Thread(target=self._flush_loop,
                                             name="usb-settings-flush",
                                             daemon=True)
            self._flusher.start()

    def _flush_loop(self):
        deadline = self._clock() + FLUSH_GIVE_UP
        while self._clock() < deadline:
            self._sleep(FLUSH_INTERVAL)
            r = self.flush_settings()
            if r is None or (r and not self._mailbox.pending):
                return
        log.warning("USB settings: SEQ_RD did not catch up in %.1f s; "
                    "the change goes out with the next one", FLUSH_GIVE_UP)

    @property
    def settings_pending(self) -> bool:
        return self._mailbox.pending

    # ── bus params / DSP commands (opt-in) ───────────────────────────

    def set_mixer_bus_param(self, bus_id: int, sub_param: int,
                            value_float: float, side: int = 0,
                            solo_mute: bool = False) -> bool:
        if self.paused or not self.connected:
            return False
        if not self.dsp_params:
            self._warn_once("bus", "USB Apollo: mixer bus params are not sent "
                            "(start the daemon with --usb-dsp-params to send "
                            "them as DSP commands; unverified on USB)")
            return False
        if solo_mute:
            value_float = 0.0
        value = struct.unpack("<I", struct.pack("<f", value_float))[0]
        return self._send_dsp([dspcmd.bus_coeff(bus_id, sub_param, value, side)])

    def set_bus_fader(self, bus_id: int, linear_gain: float,
                      mix_coeff: float = 0.0, mix_coeff_r: float | None = None,
                      solo_mute: bool = False) -> bool:
        ok = self.set_mixer_bus_param(bus_id, 0, mix_coeff, solo_mute=solo_mute)
        if mix_coeff_r is not None:
            ok = self.set_mixer_bus_param(bus_id, 0, mix_coeff_r, side=1,
                                          solo_mute=solo_mute) and ok
        return ok

    def set_bus_pan(self, bus_id: int, mix_coeff: float,
                    mix_coeff_r: float | None = None) -> bool:
        ok = self.set_mixer_bus_param(bus_id, 0, mix_coeff)
        if mix_coeff_r is not None:
            ok = self.set_mixer_bus_param(bus_id, 0, mix_coeff_r, side=1) and ok
        return ok

    def _send_dsp_input_param(self, p: umix.DspInputParam) -> bool:
        if not self.dsp_params:
            self._warn_once("dspin", "USB Apollo: DSP input params (route, "
                            "span, scale) are not sent without --usb-dsp-params")
            return False
        return self._send_dsp([dspcmd.input_param(p.input, p.param, p.value)])

    def _send_dsp(self, cmds) -> bool:
        with self._lock:
            if not self.connected:
                return False
            frames = dspcmd.frame_stream(dspcmd.encode_commands(cmds),
                                         self._bulk_seq)
            try:
                claim = getattr(self._handle, "claim", None)
                if claim:
                    claim()             # the bulk pipe needs interface 0
                for f in frames:
                    self._handle.write(EP_BULK_OUT, f.to_bytes(), timeout=1000)
                    self._bulk_seq = (self._bulk_seq + 1) & 0xFF
                self._drain_bulk_in()
            except Exception as e:  # noqa: BLE001
                log.error("USB DSP command failed: %s", e)
                return False
        return True

    def _drain_bulk_in(self, limit: int = 8):
        for _ in range(limit):
            try:
                data = self._handle.read(EP_BULK_IN, 1024, timeout=2)
            except Exception:  # noqa: BLE001 - timeout: nothing queued
                return
            if not data:
                return

    # ── readback (J2) ────────────────────────────────────────────────

    def get_hw_readback(self):
        """(status, 40 words) like the PCIe GET_HW_READBACK; also runs a
        pending settings flush, as the UA driver does from its readback."""
        if not self.connected:
            return None
        with self._lock:
            try:
                status, words = self._regs.readback(self.protocol)
            except Exception as e:  # noqa: BLE001
                log.debug("USB readback failed: %s", e)
                return None
            if self._mailbox.pending:
                try:
                    self._mailbox.flush(self._regs)
                except Exception as e:  # noqa: BLE001
                    log.debug("USB settings flush (readback) failed: %s", e)
        if status == 1 and self.knob_writeback:
            self._knob_writeback(words)
        if status == 1:
            self._rb_last = words
        if status != 1 and len(words) >= udev.READBACK_WORDS:
            # A snapshot whose check words are wrong is a dropped snapshot,
            # as on PCIe (both check words all ones): report it that way so
            # poll_hw_readback sets readback_skipped and init retries (and
            # keeps the unit's settings) instead of pushing the defaults
            words = list(words)
            words[38] = words[39] = 0xFFFFFFFF
            return 1, words
        return status, (words if words else [0] * udev.READBACK_WORDS)

    # setting 2 fields the front panel changes: level, mute+mono, dim
    _KNOB_FIELDS = (0x000000FF, 0x00030000, 0x80000000)

    def _knob_writeback(self, words):
        """Mirror a front-panel change of setting 2 into the mailbox."""
        if len(words) < 3:
            return
        if self._rb_last is None:
            # Adopt what the hardware has; nothing is written for it
            with self._lock:
                fields = 0
                for mask in self._KNOB_FIELDS:
                    fields |= mask
                v = self._mailbox.value[2]
                self._mailbox.value[2] = (v & ~fields) | (words[2] & fields)
            return
        now = self._clock()
        with self._lock:
            if now - self._host_write_t < 0.5 or self._mailbox.mask[2]:
                self._knob_seen = {}
                return
            cached = self._mailbox.value[2]
            for mask in self._KNOB_FIELDS:
                hw = words[2] & mask
                if hw == (cached & mask):
                    self._knob_seen.pop(mask, None)
                    continue
                # the same new value in two snapshots in a row: the panel moved
                if self._knob_seen.get(mask) == hw and \
                        (self._rb_last[2] & mask) == hw:
                    self._mailbox.merge(2, hw, mask)
                    self._knob_seen.pop(mask, None)
                    log.info("USB knob write-back: setting 2 field 0x%08x "
                             "= 0x%08x", mask, hw)
                else:
                    self._knob_seen[mask] = hw
        if self._mailbox.pending:
            self._schedule_flush()
