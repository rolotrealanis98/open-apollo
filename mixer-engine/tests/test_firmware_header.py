"""FirmwareUpdate*.bin container headers: parsing, validation, comparison.

Synthetic headers only (no UA files needed).  The real-image test runs when
the user has the UAD software installed and skips otherwise.
"""

import json
import struct
from pathlib import Path

import pytest

import firmware_header as fw

REAL = fw.DEFAULT_DIR / "FirmwareUpdateFreud.bin"


def image(
    magic="GBUT",
    build_time=1700000000,
    format_tag=42,
    board_info=0x9F0EA929,
    fw_release=0x0B0FC9B3,
    boot_release=0x05010004,
    payload_dwords=4,
    dev_info=0x0322007A,
):
    """One container: 32-byte plain header, 32-byte trailer, payload."""
    head = magic.encode("ascii") + struct.pack(
        "<7I",
        build_time,
        format_tag,
        board_info,
        fw_release,
        boot_release,
        payload_dwords,
        dev_info,
    )
    return head + b"\xaa" * 32 + b"\x00" * (payload_dwords * 4)


def test_parse_fields_and_version():
    header = fw.parse(image(), "Freud.bin")
    assert header.magic == "GBUT"
    assert header.source == "Freud.bin"
    assert header.fw_release == 0x0B0FC9B3
    assert (header.fw_major, header.fw_minor, header.fw_build) == (11, 15, 51635)
    assert header.version == "11.15.51635"
    assert header.boot_release == 0x05010004
    assert header.dev_info == 0x0322007A
    assert header.selector == 0x1F
    assert header.board_version == 0x0EA929
    assert header.product == "Apollo x4"
    assert header.payload_matches_size
    assert header.built == "2023-11-14 22:13"


def test_extended_and_plain_selectors_and_mask():
    extended = fw.parse(image(board_info=0x9F0EA929))
    assert extended.selector == 0x1F
    assert extended.board_mask == 0xFFFFFF
    plain = fw.parse(image(magic="FBUT", format_tag=10, board_info=0x22159C16))
    assert plain.selector == 0x2
    assert plain.board_mask == 0x0FFFFFFF
    assert plain.board_version == 0x0215_9C16


def test_build_time_is_none_for_fbut():
    header = fw.parse(image(magic="FBUT", format_tag=10, build_time=7))
    assert header.built is None


def test_unknown_selector_falls_back_to_dev_info():
    header = fw.parse(image(board_info=0x00000000))
    assert header.product == "Apollo x4"
    assert fw.parse(image(dev_info=0xDEADBEEF, board_info=0)).product is None


def test_bad_magic_and_short_file_raise():
    with pytest.raises(ValueError, match="not a UA firmware container"):
        fw.parse(b"not-a-firmware" + b"\0" * 64)
    with pytest.raises(ValueError, match="shorter than"):
        fw.parse(b"GBUT")


def test_problems_reports_payload_and_tag():
    header = fw.parse(image(format_tag=10))
    assert any("format tag" in p for p in fw.problems(header))
    header = fw.parse(image()[:-4])
    assert any("does not match" in p for p in fw.problems(header))
    assert fw.problems(fw.parse(image())) == []


def test_compare_same_unit():
    result = fw.compare(
        fw.parse(image()),
        fw_release=0x0B0FC9B3,
        boot_release=0x05010004,
        dev_info=0x0322007A,
        fpga_rev=0x0EA929,
    )
    assert result.score == 0
    assert result.verdict == "same"
    assert result.action == "skip"
    assert not result.needs_update


def test_compare_newer_and_older_minor():
    base = fw.parse(image(fw_release=0x0B0FC9B3))
    newer = fw.compare(base, fw_release=0x0B0FC9B2)
    assert (newer.score, newer.verdict, newer.action) == (1, "upgrade", "install")
    assert newer.needs_update
    older = fw.compare(base, fw_release=0x0B0FC9B4)
    assert (older.score, older.verdict, older.action) == (-1, "older", "skip")


def test_compare_major_wins_before_components():
    header = fw.parse(image(fw_release=0x0C000001))
    up = fw.compare(header, fw_release=0x0BFFFFFF, dev_info=0x11111111)
    assert (up.score, up.verdict) == (2, "major-upgrade")
    header = fw.parse(image(fw_release=0x0A000001))
    down = fw.compare(header, fw_release=0x0B000001)
    assert (down.score, down.verdict, down.action) == (
        -2,
        "major-downgrade",
        "confirm-downgrade",
    )


def test_compare_wildcards_and_board_mask():
    header = fw.parse(image(dev_info=0xFFFFFFFF, fw_release=0xFFFFFFFF))
    assert fw.compare(header, fw_release=0x0B0FC9B3, dev_info=0x12345678).score == 0
    extended = fw.parse(image(board_info=0x9F0EA929))
    assert fw.compare(extended, fpga_rev=0x0B0EA929).score == 0
    assert fw.compare(extended, fpga_rev=0x0B0EA928).score == 1


def test_compare_ignores_absent_device_words():
    assert fw.compare(fw.parse(image())).score == 0


def test_cli_lists_and_json(tmp_path, capsys):
    path = tmp_path / "FirmwareUpdateFreud.bin"
    path.write_bytes(image())
    assert fw.main([str(path), "--json"]) == 0
    rows = json.loads(capsys.readouterr().out)
    assert rows[0]["file"] == "FirmwareUpdateFreud.bin"
    assert rows[0]["product"] == "Apollo x4"
    assert rows[0]["version"] == "11.15.51635"
    assert rows[0]["payload_ok"] is True

    assert (
        fw.main([str(path), "--fw-release", "0x0b0fc9b3", "--dev-info", "0x0322007a"])
        == 0
    )
    out = capsys.readouterr().out
    assert "-> same (skip)" in out


@pytest.mark.skipif(not REAL.exists(), reason="no UAD firmware installed")
def test_real_apollo_x4_image():
    header = fw.read(REAL)
    assert header.magic == "GBUT"
    assert header.version == "11.15.51635"
    assert header.dev_info == 0x0322007A
    assert header.selector == 0x1F
    assert header.product == "Apollo x4"
    assert header.payload_matches_size
    assert fw.problems(header) == []
    assert Path(REAL).stat().st_size == header.size
