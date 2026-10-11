#!/usr/bin/env python3
"""Synthetic-blob tests for extract-ua-dsp-modules.py (no UA files needed):
python3 tools/test-extract-ua-dsp-modules.py"""
import importlib.util
import struct
import sys
import tempfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "extract", Path(__file__).with_name("extract-ua-dsp-modules.py"))
ex = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ex)


def bill(mid, body=b"\x11" * 40, arch=0):
    return (b"Bill" + struct.pack("<IIII", mid | arch << 24, 0x02000000,
                                  len(body), len(body) // 2) + body)


def main():
    with tempfile.TemporaryDirectory() as t:
        t = Path(t)
        pad = b"\0" * 12
        blobs = b"".join(bill(m, bytes([m & 0xff]) * 40) + pad
                         for m in ex.MIC_STRIP)
        # noise: a wrong word2, a non-strip id, an arch-patched copy
        noise = (b"Bill" + struct.pack("<IIII", 0xEB, 1, 8, 4) + b"\0" * 8
                 + pad + bill(0x123) + pad + bill(0xEB, arch=2))
        (t / "UAD Mic Input Strip Header.dll").write_bytes(
            b"MZ" + b"\0" * 2 + blobs + noise)
        out = t / "out"
        assert ex.main([str(t), "--out", str(out)]) == 0
        files = sorted(p.name for p in out.iterdir())
        assert files == sorted("%03x.bill" % m for m in ex.MIC_STRIP), files
        assert (out / "0eb.bill").read_bytes() == bill(0xEB, b"\xeb" * 40)
        assert not (out / "123.bill").exists()
        # two different copies of one id: not written, exit 2
        (t / "other.dll").write_bytes(bill(0xEB, b"\x99" * 40))
        out2 = t / "out2"
        assert ex.main([str(t), "--out", str(out2)]) == 2
        assert not (out2 / "0eb.bill").exists()
        # dry run writes nothing; a missing Mic module gives exit 1
        empty = t / "empty"
        empty.mkdir()
        assert ex.main([str(empty), "--out", str(t / "o3"), "--dry-run"]) == 1
        assert not (t / "o3").exists()
    print("ok")


if __name__ == "__main__":
    sys.exit(main())
