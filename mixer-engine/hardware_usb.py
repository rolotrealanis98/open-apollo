"""
USB hardware backend for Apollo Solo/Twin/Twin X USB devices.

Provides the same interface as HardwareBackend (hardware.py) but
communicates via USB bulk endpoints instead of ioctl. Uses the
0xDC/0xDD command/response protocol over EP1 IN/OUT.

Protocol:
  Command (OUT):  [word_count:u16LE][type:u8][0xDC][payload]
  Response (IN):  [word_count:u16LE][type:u8][0xDD][payload]
  Sub-commands:   [opcode:u16LE][param:u16LE][value:u32LE]
"""

# STAGED: USB hardware backend. Intentionally not wired into ua_mixer_daemon.py yet —
# mirrors HardwareBackend's interface for when USB capture lands (see CLAUDE.md Status,
# docs/usb-apollo-re/page.md). Zero importers is expected, not evidence of deadness.

import logging
import struct
import time
import usb.core
import usb.util

log = logging.getLogger(__name__)

UA_VID = 0x2B5A
LIVE_PIDS = {
    0x000D: "Apollo Solo USB",
    0x0002: "Twin USB",
    0x000F: "Twin X USB",
}

EP_BULK_OUT = 0x01
EP_BULK_IN = 0x81
EP_INTR_IN = 0x86

MAGIC_CMD = 0xDC
MAGIC_RSP = 0xDD

# DSP register params (from USB capture analysis)
REG_INIT = 0x0023       # Write 1 to activate FPGA
REG_CONFIG_A = 0x0010   # Config word A
REG_CONFIG_B = 0x0011   # Config word B
REG_STATUS = 0x0027     # Status query (opcode 0x01)

# Opcodes
OP_QUERY = 0x0001
OP_READWRITE = 0x0002

# Vendor control requests (mixer settings, from USBPcap RE)
VREQ_SETTINGS_WRITE = 0x03   # Write mixer settings block
VREQ_COMMIT_A = 0x0C         # Commit/trigger (pre-write)
VREQ_COMMIT_B = 0x0D         # Commit/trigger (post-write)

# Settings FPGA addresses (wValue in vendor request 0x03)
SETTINGS_SEQ = 0x0602        # 4 bytes: sequence counter (u32 LE)
SETTINGS_MASK = 0x062D       # 128 bytes: settings mask+value buffer
SETTINGS_VALUES = 0x064F     # 128 bytes: gain value buffer
SETTINGS_EXT = 0x0670        # 48 bytes: extended settings

# Settings layout in the 128-byte buffer at 0x062d/0x064f:
#   Each setting is 8 bytes (2 × u32 LE words: wordA + wordB).
#   128 / 8 = 16 settings max.
#
#   Word format: (changed_mask[15:0] << 16) | value[15:0]
#   If mask is 0, the DSP ignores that setting.
#
#   Setting map (from mixer-knobs.pcap analysis):
#     set[0] (+0):  Preamp ch0 — gain in wordA, flags (48V=bit3, MicLine=bit0)
#     set[1] (+8):  Preamp ch1 — same layout as set[0]
#     set[2] (+16): Monitor — level in wordA (raw=192+dB*2), mute/mono in wordB
#     set[3] (+24): Gain C — tracks with gain (val_c + 0x41 base offset)
SETTING_PREAMP_CH0 = 0       # offset +0
SETTING_PREAMP_CH1 = 1       # offset +8
SETTING_MONITOR = 2          # offset +16
SETTING_GAIN_C = 3           # offset +24


class HardwareUSB:
    """USB bulk endpoint backend for Apollo USB devices.

    Drop-in replacement for HardwareBackend when device is USB.
    """

    def __init__(self):
        self.dev = None
        self.connected = False
        self.device_name = ""
        self._claimed = False
        self._seq = 0  # Mixer settings sequence counter

    def open(self) -> bool:
        """Find and open the Apollo USB device."""
        for pid, name in LIVE_PIDS.items():
            self.dev = usb.core.find(idVendor=UA_VID, idProduct=pid)
            if self.dev:
                self.device_name = name
                break

        if not self.dev:
            log.warning("No UA USB device found")
            return False

        # Detach kernel driver from DSP interface
        try:
            if self.dev.is_kernel_driver_active(0):
                self.dev.detach_kernel_driver(0)
        except Exception:
            pass

        try:
            usb.util.claim_interface(self.dev, 0)
            self._claimed = True
        except usb.core.USBError as e:
            log.error("Cannot claim DSP interface: %s", e)
            return False

        self.connected = True
        log.info("Opened USB device: %s", self.device_name)
        return True

    def close(self):
        """Release USB interface."""
        if self._claimed and self.dev:
            try:
                usb.util.release_interface(self.dev, 0)
            except Exception:
                pass
            self._claimed = False
        self.connected = False

    # ── Low-level protocol ──

    def _send_cmd(self, cmd_type, subcmds):
        """Send a 0xDC command with sub-command pairs.
        subcmds: list of (opcode, param, value) tuples.
        Returns list of response packets (each a bytes object).
        """
        word_count = len(subcmds) * 2
        header = struct.pack("<HBB", word_count, cmd_type, MAGIC_CMD)
        payload = b""
        for opcode, param, value in subcmds:
            payload += struct.pack("<HHI", opcode, param, value)

        self.dev.write(EP_BULK_OUT, header + payload, timeout=1000)

        # Read response(s) — multi-packet responses use sequential type numbers
        responses = []
        for _ in range(20):  # max 20 response packets
            try:
                data = bytes(self.dev.read(EP_BULK_IN, 1024, timeout=500))
                responses.append(data)
                # Last packet is shorter than max USB bulk size
                if len(data) < 508:
                    break
            except usb.core.USBTimeoutError:
                break

        return responses

    def _send_query(self, param):
        """Send single-word query (opcode 0x01)."""
        header = struct.pack("<HBB", 1, 1, MAGIC_CMD)
        payload = struct.pack("<HH", OP_QUERY, param)
        self.dev.write(EP_BULK_OUT, header + payload, timeout=1000)

        try:
            data = bytes(self.dev.read(EP_BULK_IN, 1024, timeout=500))
            return data
        except usb.core.USBTimeoutError:
            return None

    def _parse_response_words(self, packets):
        """Extract all 32-bit words from response packets."""
        words = []
        for pkt in packets:
            wc = struct.unpack_from("<H", pkt, 0)[0]
            for i in range(4, min(len(pkt), 4 + wc * 4), 4):
                words.append(struct.unpack_from("<I", pkt, i)[0])
        return words

    # ── DSP init ──

    def dsp_init(self):
        """Send DSP init command to activate FPGA."""
        responses = self._send_cmd(0, [
            (OP_READWRITE, REG_INIT, 0x00000001),
            (OP_READWRITE, REG_CONFIG_A, 0x01B71448),
        ])
        log.info("DSP init: %d response packets", len(responses))
        return len(responses) > 0

    def dsp_readback(self):
        """Request full mixer state readback."""
        responses = self._send_cmd(1, [
            (OP_READWRITE, REG_CONFIG_A, 0x01B71448),
            (OP_READWRITE, REG_CONFIG_B, 0x0AE52420),
        ])
        return self._parse_response_words(responses)

    def dsp_status(self):
        """Query DSP status."""
        resp = self._send_query(REG_STATUS)
        if resp and len(resp) >= 12:
            words = self._parse_response_words([resp])
            return words
        return None

    # ── Clock control ──

    def set_sample_rate(self, rate=48000):
        """Set UAC2 clock frequency via SET_CUR."""
        # Detach audio control interface temporarily
        try:
            if self.dev.is_kernel_driver_active(1):
                self.dev.detach_kernel_driver(1)
        except Exception:
            pass

        try:
            self.dev.ctrl_transfer(0x21, 0x01, 0x0100, 0x8001,
                                   struct.pack("<I", rate), timeout=2000)
        except usb.core.USBError:
            pass  # May timeout, OK

        # Read back
        try:
            data = self.dev.ctrl_transfer(0xA1, 0x01, 0x0100, 0x8001,
                                          4, timeout=1000)
            return struct.unpack("<I", bytes(data))[0]
        except usb.core.USBError:
            return 0

    # ── Mixer state ──

    def read_mixer_state(self):
        """Read the full mixer routing matrix (770 words)."""
        return self.dsp_readback()

    def write_mixer_word(self, param, value):
        """Write a single mixer register."""
        self._send_cmd(0, [(OP_READWRITE, param, value)])

    # ── Mixer settings (vendor control request 0x03) ──

    def _vendor_write(self, request, wvalue, data=b""):
        """Send a vendor control OUT request (bmRequestType=0x41)."""
        self.dev.ctrl_transfer(0x41, request, wvalue, 0, data, timeout=1000)

    def _write_settings(self, mask_buf, value_buf=None, ext_buf=None):
        """Write a mixer settings batch via vendor request 0x03.

        Protocol (from USBPcap RE of mixer-knobs.pcap):
          1. Write 128-byte mask buffer to FPGA address 0x062D
          2. Optionally write 128-byte value buffer to 0x064F
          3. Optionally write 48-byte extended buffer to 0x0670
          4. Write 4-byte sequence counter to 0x0602
        """
        self._vendor_write(VREQ_SETTINGS_WRITE, SETTINGS_MASK, mask_buf)
        if value_buf and any(b != 0 for b in value_buf):
            self._vendor_write(VREQ_SETTINGS_WRITE, SETTINGS_VALUES, value_buf)
        if ext_buf and any(b != 0 for b in ext_buf):
            self._vendor_write(VREQ_SETTINGS_WRITE, SETTINGS_EXT, ext_buf)
        self._seq += 1
        self._vendor_write(VREQ_SETTINGS_WRITE, SETTINGS_SEQ,
                           struct.pack("<I", self._seq))

    def _setting_word(self, mask, value):
        """Encode a settings word: (mask << 16) | (value & 0xFFFF)."""
        return ((mask & 0xFFFF) << 16) | (value & 0xFFFF)

    def set_preamp_gain(self, channel, gain_db):
        """Set preamp gain for a channel (0 or 1).

        Encoding (from mixer-knobs.pcap correlation):
          val_a = max(0, min(54, gain_dB - 10))  → setting[ch].wordA, mask=0x00FF
          val_c = val_a + 0x41                   → setting[3].wordA, mask=0x003F
        USB uses a +0x41 base offset for val_c (vs +1 on Thunderbolt).
        """
        gain_db = int(round(gain_db))
        val_a = max(0, min(54, gain_db - 10))
        val_c_encoded = val_a + 0x41  # USB base offset (verified across full range)

        mask_buf = bytearray(128)
        value_buf = bytearray(128)

        # Gain A in value buffer at setting[channel]
        setting_off = (SETTING_PREAMP_CH0 + channel) * 8
        word = self._setting_word(0x00FF, val_a)
        struct.pack_into("<I", value_buf, setting_off, word)

        # Gain C in mask buffer at setting[3]
        gc_word = self._setting_word(0x003F, val_c_encoded)
        struct.pack_into("<I", mask_buf, SETTING_GAIN_C * 8, gc_word)

        log.info("USB GAIN: ch%d = %d dB → val_a=%d val_c=0x%02x seq=%d",
                 channel, gain_db, val_a, val_c_encoded, self._seq + 1)
        self._write_settings(mask_buf, value_buf)

    def set_phantom_power(self, channel, enabled):
        """Toggle 48V phantom power (bit 3 of setting[ch].wordA)."""
        mask_buf = bytearray(128)
        setting_off = (SETTING_PREAMP_CH0 + channel) * 8
        val = 0x0008 if enabled else 0x0000
        word = self._setting_word(0x0008, val)
        struct.pack_into("<I", mask_buf, setting_off, word)

        log.info("USB 48V: ch%d = %s seq=%d", channel, enabled, self._seq + 1)
        self._write_settings(mask_buf)

    def set_mic_line(self, channel, line_mode):
        """Switch Mic/Line input (bit 0 of setting[ch].wordA)."""
        mask_buf = bytearray(128)
        setting_off = (SETTING_PREAMP_CH0 + channel) * 8
        val = 0x0001 if line_mode else 0x0000
        word = self._setting_word(0x0001, val)
        struct.pack_into("<I", mask_buf, setting_off, word)

        log.info("USB MIC/LINE: ch%d = %s seq=%d",
                 channel, "Line" if line_mode else "Mic", self._seq + 1)
        self._write_settings(mask_buf)

    def set_monitor_level(self, level_db):
        """Set monitor level in dB (-96 to 0).

        Encoding: raw = 192 + (dB * 2), range 0x00-0xC0.
        """
        raw = max(0, min(0xC0, int(192 + level_db * 2)))
        mask_buf = bytearray(128)
        word = self._setting_word(0x00FF, raw)
        struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8, word)

        log.info("USB MONITOR: level=%.1f dB → raw=0x%02x seq=%d",
                 level_db, raw, self._seq + 1)
        self._write_settings(mask_buf)

    def set_monitor_mute(self, muted):
        """Toggle monitor mute (bit 1 of setting[2].wordB, value 2=muted)."""
        mask_buf = bytearray(128)
        val = 0x0002 if muted else 0x0000
        word = self._setting_word(0x0003, val)
        struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8 + 4, word)

        log.info("USB MONITOR: mute=%s seq=%d", muted, self._seq + 1)
        self._write_settings(mask_buf)

    def set_monitor_mono(self, mono):
        """Toggle mono (bit 0 of setting[2].wordB)."""
        mask_buf = bytearray(128)
        val = 0x0001 if mono else 0x0000
        word = self._setting_word(0x0003, val)
        struct.pack_into("<I", mask_buf, SETTING_MONITOR * 8 + 4, word)

        log.info("USB MONITOR: mono=%s seq=%d", mono, self._seq + 1)
        self._write_settings(mask_buf)

    # ── Vendor control requests ──

    def read_device_info(self):
        """Read the 512-byte device info block (vendor request 0x10)."""
        try:
            data = self.dev.ctrl_transfer(0xC0, 0x10, 0, 0, 512, timeout=500)
            return bytes(data)
        except usb.core.USBError:
            return None

    def read_protocol_version(self):
        """Read protocol version (vendor request 0x00)."""
        try:
            data = self.dev.ctrl_transfer(0xC0, 0x00, 0, 0, 4, timeout=300)
            return bytes(data)
        except usb.core.USBError:
            return None

    # ── Interrupt endpoint (metering/notifications) ──

    def read_interrupt(self, timeout=100):
        """Read from interrupt endpoint (EP6 IN). Returns bytes or None."""
        try:
            data = self.dev.read(EP_INTR_IN, 512, timeout=timeout)
            return bytes(data)
        except usb.core.USBTimeoutError:
            return None
        except usb.core.USBError:
            return None
