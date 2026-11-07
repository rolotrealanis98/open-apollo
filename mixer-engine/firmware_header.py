"""Universal Audio firmware update containers, read-only.

UA ships device firmware as ``FirmwareUpdate*.bin`` under
``/Library/Application Support/Universal Audio/Firmware/UAD-2`` on macOS.
The first 0x40 bytes are a cleartext container header; everything after is
encrypted and is decrypted only by the device.  The header carries the same
words the device reports over BAR0, so a local file can be compared with a
connected unit without touching the payload.

Layout (little-endian), from the macOS Meter app's
``_FWFirmwareUpdatePlainHeader`` (``CUAD2Info::LoadBinFile``) and
``CUAD2Info::CompareFirmwareVersion``::

    0x00 char[4] magic          FBUT / GBUT / HBUT
    0x04 u32     build_time     Unix seconds (GBUT/HBUT), 7 (FBUT)
    0x08 u32     format_tag     10 = FBUT, 42 = GBUT/HBUT (never used)
    0x0C u32     board_info     product selector (high) + board version
    0x10 u32     fw_release     major << 24 | minor << 16 | build  (BAR0 0x0008)
    0x14 u32     boot_release   bootloader version                 (BAR0 0x0004)
    0x18 u32     payload_dwords (file size - 0x40) / 4
    0x1C u32     dev_info       device info word                   (BAR0 0x01AC)
    0x20 32 B    opaque         device-only crypto/signature data
    0x40 ...     encrypted payload (sent whole, header included)

The words are not identity constants.  ``0x0b0fc9b3`` (11.15.51635) is a
release shared by every model built from it; the per-model byte is the
selector in ``board_info`` and, less precisely, ``dev_info``.  ``0`` and
``0xFFFFFFFF`` are wildcards in the host's comparison.
"""

from __future__ import annotations

import argparse
import datetime
import json
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

HEADER_SIZE = 0x40
MAGICS = (b"FBUT", b"GBUT", b"HBUT")
FORMAT_TAG_BY_MAGIC = {"FBUT": 10, "GBUT": 42, "HBUT": 42}
WILDCARD = 0xFFFFFFFF
DEFAULT_DIR = Path("/Library/Application Support/Universal Audio/Firmware/UAD-2")

# Selectors the Meter app's LoadFirmwareUpdate switch maps to a mixer card
# type (selector -> product name, card type).  Only entries actually traced.
PRODUCTS = {
    0x1F: ("Apollo x4", 0x2A),
    0x2D: ("Apollo x4", 0x2A),
    0x36: ("Apollo x4 Gen 2", 0x2F),
    0x3F: ("Apollo x4 Gen 2 TB4", 0x37),
    0x27: ("Apollo Twin X - DUO", 0x28),
    0x28: ("Apollo Twin X - QUAD", 0x29),
}

# dev_info words seen in the installed images (BAR0 0x01AC).
DEV_INFO_PRODUCTS = {
    0x0322007A: "Apollo x4",
    0x0322001D: "Apollo x4 Gen 2",
    0x03220065: "Apollo Twin X",
}

VERDICTS = {
    2: "major-upgrade",
    1: "upgrade",
    0: "same",
    -1: "older",
    -2: "major-downgrade",
}


def _is_wildcard(value: int | None) -> bool:
    return value is None or value in (0, WILDCARD)


def _selector(board_info: int) -> int:
    """Product selector: bits 30..24 in the extended form, else 31..28."""
    if (board_info & 0x60) == 0x20:
        return (board_info >> 24) & 0x7F
    return (board_info >> 28) & 0xF


@dataclass(frozen=True)
class Header:
    """The 0x40-byte container header of one FirmwareUpdate*.bin."""

    magic: str
    build_time: int
    format_tag: int
    board_info: int
    fw_release: int
    boot_release: int
    payload_dwords: int
    dev_info: int
    size: int
    source: str = ""

    @property
    def fw_major(self) -> int:
        return (self.fw_release >> 24) & 0xFF

    @property
    def fw_minor(self) -> int:
        return (self.fw_release >> 16) & 0xFF

    @property
    def fw_build(self) -> int:
        return self.fw_release & 0xFFFF

    @property
    def version(self) -> str:
        """The firmware release as the Mixer Engine prints it."""
        return "%d.%02d.%03d" % (self.fw_major, self.fw_minor, self.fw_build)

    @property
    def built(self) -> str | None:
        """Build time as UTC text, None for the FBUT build counter."""
        if self.magic == "FBUT" or self.build_time < 1_000_000_000:
            return None
        return datetime.datetime.fromtimestamp(
            self.build_time, datetime.timezone.utc
        ).strftime("%Y-%m-%d %H:%M")

    @property
    def selector(self) -> int:
        return _selector(self.board_info)

    @property
    def board_mask(self) -> int:
        return 0xFFFFFF if (self.board_info & 0x60) == 0x20 else 0x0FFFFFFF

    @property
    def board_version(self) -> int:
        return self.board_info & self.board_mask

    @property
    def product(self) -> str | None:
        known = PRODUCTS.get(self.selector)
        if known:
            return known[0]
        return DEV_INFO_PRODUCTS.get(self.dev_info)

    @property
    def payload_bytes(self) -> int:
        return self.payload_dwords * 4

    @property
    def payload_matches_size(self) -> bool:
        return self.payload_bytes + HEADER_SIZE == self.size


@dataclass(frozen=True)
class Comparison:
    """How one image compares with a connected unit, UA's semantics."""

    score: int
    reasons: tuple[str, ...] = ()

    @property
    def verdict(self) -> str:
        return VERDICTS[self.score]

    @property
    def needs_update(self) -> bool:
        return self.score > 0

    @property
    def action(self) -> str:
        """What UA's update loop does when the user has not forced it."""
        if self.score > 0:
            return "install"
        if self.score == -2:
            return "confirm-downgrade"
        return "skip"


def parse(data: bytes, source: str = "") -> Header:
    """Parse one container header; raises ValueError when it is not one."""
    if len(data) < HEADER_SIZE:
        raise ValueError(
            "%s: shorter than the 0x40-byte firmware header" % (source or "input")
        )
    magic = bytes(data[:4])
    if magic not in MAGICS:
        raise ValueError(
            "%s: not a UA firmware container (magic %r)" % (source or "input", magic)
        )
    (
        _,
        build_time,
        format_tag,
        board_info,
        fw_release,
        boot_release,
        payload_dwords,
        dev_info,
    ) = struct.unpack_from("<8I", data, 0)
    return Header(
        magic.decode("ascii"),
        build_time,
        format_tag,
        board_info,
        fw_release,
        boot_release,
        payload_dwords,
        dev_info,
        len(data),
        source,
    )


def read(path: str | Path) -> Header:
    path = Path(path)
    return parse(path.read_bytes(), str(path))


def problems(header: Header) -> list[str]:
    """Structural faults the host does not check but a reader should."""
    found = []
    expected = FORMAT_TAG_BY_MAGIC[header.magic]
    if header.format_tag != expected:
        found.append(
            "format tag %d for %s (expected %d)"
            % (header.format_tag, header.magic, expected)
        )
    if not header.payload_matches_size:
        found.append(
            "payload_dwords %d does not match %d-byte file"
            % (header.payload_dwords, header.size)
        )
    return found


def compare(
    header: Header,
    *,
    fw_release: int | None = None,
    boot_release: int | None = None,
    dev_info: int | None = None,
    fpga_rev: int | None = None,
) -> Comparison:
    """Compare an image with a unit's live words, as UA's Meter app does.

    The major firmware byte decides first (``±2``, upgrade/downgrade); when
    it matches, every component that is present on both sides is compared
    and any newer (``1``) or older (``-1``) word decides.  ``None``, ``0``
    and ``0xFFFFFFFF`` are wildcards.
    """
    if (
        not _is_wildcard(header.fw_release)
        and fw_release is not None
        and not _is_wildcard(fw_release)
    ):
        image_major = (header.fw_release >> 24) & 0xFF
        device_major = (fw_release >> 24) & 0xFF
        if image_major != device_major:
            reason = "major %d in the file, the unit runs %d" % (
                image_major,
                device_major,
            )
            return Comparison(2 if image_major > device_major else -2, (reason,))

    board = (
        None
        if fpga_rev is None or _is_wildcard(fpga_rev)
        else fpga_rev & header.board_mask
    )
    components = (
        ("firmware", header.fw_release, fw_release),
        ("board", header.board_version, board),
        ("bootloader", header.boot_release, boot_release),
        ("device info", header.dev_info, dev_info),
    )
    newer, older, reasons = False, False, []
    for name, image, device in components:
        if image is None or device is None:
            continue
        if _is_wildcard(image) or _is_wildcard(device):
            continue
        if image > device:
            newer = True
            reasons.append("%s 0x%08x is newer than 0x%08x" % (name, image, device))
        elif image < device:
            older = True
            reasons.append("%s 0x%08x is older than 0x%08x" % (name, image, device))
    return Comparison(1 if newer else -1 if older else 0, tuple(reasons))


def _int(text: str) -> int:
    return int(text, 0)


def _device_words() -> dict:
    """The connected unit's words through the driver (None without one)."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        import hardware
    except ImportError as e:  # pragma: no cover - layout
        raise SystemExit("cannot import the mixer engine's hardware module: %s" % e)
    backend = hardware.HardwareBackend()
    if not backend.open():
        raise SystemExit("no Apollo device node; use --fw-release and friends")
    return backend.get_system_info() or {}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Inspect Universal Audio FirmwareUpdate*.bin containers "
        "and compare them with a connected unit."
    )
    parser.add_argument(
        "files",
        nargs="*",
        type=Path,
        help="firmware files (default: the installed UAD set)",
    )
    parser.add_argument("--json", action="store_true", help="JSON output")
    parser.add_argument(
        "--device",
        action="store_true",
        help="compare with the connected unit through the driver",
    )
    parser.add_argument("--fw-release", type=_int, help="BAR0 0x0008 to compare")
    parser.add_argument("--boot-release", type=_int, help="BAR0 0x0004 to compare")
    parser.add_argument("--dev-info", type=_int, help="BAR0 0x01AC to compare")
    parser.add_argument("--fpga-rev", type=_int, help="BAR0 0x2218 to compare")
    args = parser.parse_args(argv)

    paths = args.files or sorted(DEFAULT_DIR.glob("FirmwareUpdate*.bin"))
    if not paths:
        parser.error("no firmware files; pass paths or install the UAD software")

    device = None
    if args.device:
        device = _device_words()
    words = {
        "fw_release": args.fw_release,
        "boot_release": args.boot_release,
        "dev_info": args.dev_info,
        "fpga_rev": args.fpga_rev,
    }
    if device is not None:
        words.update({k: device.get(k) for k in words})

    rows = []
    for path in paths:
        header = read(path)
        row = {
            "file": path.name,
            "magic": header.magic,
            "built": header.built,
            "product": header.product,
            "selector": "0x%02x" % header.selector,
            "version": header.version,
            "fw_release": "0x%08x" % header.fw_release,
            "boot_release": "0x%08x" % header.boot_release,
            "dev_info": "0x%08x" % header.dev_info,
            "payload_ok": header.payload_matches_size,
            "problems": problems(header),
        }
        if device is not None or any(v is not None for v in words.values()):
            result = compare(header, **words)
            row["compare"] = {
                "score": result.score,
                "verdict": result.verdict,
                "action": result.action,
                "reasons": list(result.reasons),
            }
        rows.append(row)

    if args.json:
        json.dump(rows, sys.stdout, indent=2)
        sys.stdout.write("\n")
        return 0
    columns = (
        "file",
        "magic",
        "built",
        "product",
        "selector",
        "version",
        "dev_info",
        "payload_ok",
    )
    print("  ".join(columns))
    for row in rows:
        print("  ".join(str(row[c] if row[c] is not None else "-") for c in columns))
        for problem in row["problems"]:
            print("  ! %s" % problem)
        if "compare" in row:
            print(
                "  -> %s (%s)" % (row["compare"]["verdict"], row["compare"]["action"])
            )
            for reason in row["compare"]["reasons"]:
                print("     %s" % reason)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
