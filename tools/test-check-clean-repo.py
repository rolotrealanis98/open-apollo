#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Tests for check-clean-repo.py on synthetic data: python3 tools/test-check-clean-repo.py

Nothing here is UA's: the Bill records are made-up headers around made-up
payloads, the "pinned" hashes are hashes of those, and every address, path,
serial and name is invented. Values the guard would flag are assembled at
run time, so this file itself passes the guard.
"""
import base64
import hashlib
import importlib.util
import io
import json
import struct
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

spec = importlib.util.spec_from_file_location("guard", Path(__file__).with_name("check-clean-repo.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

BILL_W2 = 0x02000000


def bill(ident, payload_len=40, w4=None, arch=0, w2=BILL_W2, seed=7):
    """A fake on-disk Bill record: 'Bill', id, word 2, size, w4, payload."""
    payload = bytes((seed + i * 13) & 0xFF for i in range(payload_len))
    w4 = payload_len // 4 if w4 is None else w4
    return b"Bill" + struct.pack("<IIII", ident | arch << 24, w2, payload_len, w4) + payload


def sha(b):
    return hashlib.sha256(b).hexdigest()


REC = bill(0x123)             # the "pinned" module of these tests
FX3 = b"CY" + bytes(range(256)) * 4   # a fake "pinned" USB boot image
PINS = guard.Pins("test", json.dumps({
    "dsp_modules": [{"id": "123", "known": [{"length": len(REC), "w4": 10, "sha256": sha(REC)}]}],
    "usb_fx3": [{"name": "Fake.bin", "known": [{"length": len(FX3), "sha256": sha(FX3)}]}],
}))

LAN = ".".join(["192", "168", "7", "23"])          # a private address
TEN = ".".join(["10", "1", "2", "3"])
MID = ".".join(["172", "20", "0", "9"])
NAME = "j" + "doe"                                  # a person's login
MAIL = "jane.doe" + "@" + "corp-mail.net"
CONTACT = "contact" + "@" + "project.dev"           # an allowlisted project address
SERIAL = "2" + "1234567890123"                      # 14 digits
HOST = "Alice" + "'s MacBook Pro"
PNG = b"\x89PNG\r\n\x1a\n" + b"\0" * 40

ALLOW = guard.Allow(f"""
[ipv4]
{".".join(["192", "168", "1", "0"])}   # documented example
[email]
{CONTACT}
@lists.project.dev
[home]
you
[serial]
2{"0" * 12}1
[image]
docs/ok.png {sha(PNG)}
docs/stale.png {"0" * 64}
[large]
big/*.json
[skip]
email third_party/*
""")


def findings(path, data, allow=ALLOW, max_binary=1 << 20, max_text=1 << 20):
    s = guard.Scanner(allow, PINS, max_binary, max_text)
    s.scan(path, data)
    return s.findings


def rules(path, data, **kw):
    return sorted({f[2] for f in findings(path, data, **kw)})


class BillRecords(unittest.TestCase):
    def test_raw_disk_record_is_found_and_pinned(self):
        blob = b"\x7fELF" + b"\0" * 9 + REC + b"\0" * 8
        self.assertEqual(rules("x.bin", blob), ["ua-bill", "ua-pinned"])
        f = [x for x in findings("x.bin", blob) if x[2] == "ua-bill"][0]
        self.assertEqual(f[1], "0xd")          # binary findings carry the offset
        self.assertIn("id 0x123", f[3])

    def test_pcie_and_usb_forms_hash_as_the_disk_form(self):
        pcie = bytearray(REC)
        pcie[7] = 2
        usb = bytearray(REC)
        usb[8:12] = b"\0\0\0\0"
        for form in (pcie, usb):
            self.assertEqual(rules("y.bin", b"\0\0" + bytes(form)), ["ua-bill", "ua-pinned"])

    def test_unknown_module_is_still_a_bill(self):
        self.assertEqual(rules("z.bin", b"\0" + bill(0x0ab, 64, seed=99)), ["ua-bill"])

    def test_split_payload_is_found_by_its_header(self):
        cut = REC[:30]   # header and part of the payload, like a framed USB stream
        self.assertEqual(rules("frames.bin", b"\0" * 4 + cut), ["ua-bill"])

    def test_header_shape_is_required(self):
        bad = [
            bill(0x123, arch=5),                 # arch byte neither 0 nor 2
            bill(0x123, w2=0x1234),              # word 2
            bill(0x123, payload_len=42),         # size not a multiple of 4
            bill(0x123, w4=0),                   # w4 out of range
            bill(0x123, w4=1000),
            bill(0),                             # no id
        ]
        for b in bad:
            self.assertEqual(rules("bad.bin", b"\0" + b), [], b[:20].hex())

    def test_written_out_as_text(self):
        c_bytes = ", ".join(f"0x{b:02x}" for b in REC)
        words = struct.unpack(f"<{len(REC) // 4}I", REC)
        le_words = ", ".join(f"0x{w:08x}" for w in words)
        be_words = ", ".join(f"0x{w:08X}" for w in struct.unpack(f">{len(REC) // 4}I", REC))
        encodings = {
            "c-bytes": f"static const u8 m[] = {{\n\t{c_bytes}\n}};\n",
            "le-words": f"u32 m[] = {{ {le_words} }};",
            "be-words": f"u32 m[] = {{ {be_words} }};",
            "decimal": json.dumps({"m": list(REC)}),
            "decimal-words": json.dumps({"m": list(words)}),
            "hex": json.dumps({"m": REC.hex()}),
            "spaced-hex": " ".join(f"{b:02x}" for b in REC),
            "base64": base64.b64encode(REC).decode(),
        }
        for name, text in encodings.items():
            got = rules("m.txt", ("line one\n" + text).encode())
            self.assertIn("ua-bill", got, name)
        f = findings("m.h", ("a\nb\n" + encodings["c-bytes"]).encode())
        self.assertEqual({x[1] for x in f}, {"4"})   # the line the array starts on
        self.assertIn("ua-pinned", {x[2] for x in f})

    def test_mentioning_bill_is_fine(self):
        text = (
            "A DSP module is a Bill record: 'Bill', id, 0x02000000, payload size, w4.\n"
            'pub const BILL_MAGIC: &[u8; 4] = b"Bill";\n'
            'r = b"Bill" + struct.pack("<IIII", ident, 0x02000000, len(body), w4)\n'
            "Bill 0x0a5 0x02000000 1920 456\n"
        )
        self.assertEqual(rules("docs/notes.md", text.encode()), [])


class PinnedFiles(unittest.TestCase):
    def test_whole_file(self):
        self.assertEqual(rules("fw/Fake.bin", FX3), ["ua-pinned"])

    def test_embedded_image(self):
        self.assertEqual(rules("pack.bin", b"\0" * 10 + FX3 + b"\0" * 10), ["ua-pinned"])

    def test_a_changed_byte_is_not_pinned(self):
        self.assertEqual(rules("other.bin", FX3[:-1] + b"\x01"), [])


class Images(unittest.TestCase):
    def test_raster_files(self):
        self.assertEqual(rules("shots/console.png", PNG), ["raster-image"])
        self.assertEqual(rules("shots/console.dat", PNG), ["raster-image"])      # by magic
        self.assertEqual(rules("art/knob.jpg", b"\xff\xd8\xff\xe0" + b"\0" * 20), ["raster-image"])
        self.assertEqual(rules("art/a.webp", b"RIFF\0\0\0\0WEBPVP8 " + b"\0" * 8), ["raster-image"])

    def test_reviewed_image_is_pinned_by_hash(self):
        self.assertEqual(rules("docs/ok.png", PNG), [])
        self.assertEqual(rules("docs/stale.png", PNG), ["raster-image"])
        self.assertIn("changed since", findings("docs/stale.png", PNG)[0][3])

    def test_data_uri(self):
        text = "<img src='data:image/png;base64," + "iVBORw0KGgo" * 30 + "'>"
        self.assertEqual(rules("page.svg", text.encode()), ["raster-image"])

    def test_vector_is_fine(self):
        self.assertEqual(rules("docs/status/badge.svg", b"<svg><rect/></svg>"), [])


class PersonalData(unittest.TestCase):
    def test_private_ipv4(self):
        for ip in (LAN, TEN, MID):
            f = findings("README.md", f"x\nconnect to {ip}:4710\n".encode())
            self.assertEqual([(x[1], x[2]) for x in f], [("2", "private-ipv4")], ip)

    def test_public_example_and_allowlisted_addresses(self):
        text = ("192.0.2.4 198.51.100.7 203.0.113.9 127.0.0.1 172.32.0.1 8.8.8.8 "
                f"{'.'.join(['192', '168', '1', '0'])}/24 version 1.10.0.0.5 v2.10.3.4.1 999.168.1.1")
        self.assertEqual(rules("doc.md", text.encode()), [])

    def test_allowlisted_address_does_not_cover_its_subnet(self):
        self.assertEqual(rules("doc.md", ".".join(["192", "168", "1", "77"]).encode()), ["private-ipv4"])

    def test_home_paths(self):
        for p in (f"/home/{NAME}/x", f"/Users/{NAME}/Documents", f"C:\\\\Users\\\\{NAME}\\\\AppData",
                  f"C:\\Users\\{NAME}\\AppData", f"drive_c/users/{NAME}/AppData"):
            self.assertEqual(rules("a.md", p.encode()), ["home-path"], p)

    def test_placeholder_homes(self):
        text = ("/home/you/x /Users/you/y /home/$USER /home/${USER}/z /Users/<name>/a "
                "/home/{user} drive_c/users/Public C:\\Users\\%USERNAME% /home/*/x /home/runner/work")
        self.assertEqual(rules("a.md", text.encode()), [])

    def test_email(self):
        self.assertEqual(rules("a.md", f"mail {MAIL} now".encode()), ["email"])
        on_list = "x" + "@" + "lists.project.dev"     # under an allowlisted @domain
        ok = (f"12345+someone@users.noreply.github.com {CONTACT} {on_list} "
              "user@example.com knob@2x.png ua-mixer-daemon@1.service ua-usb-init@Solo.bin.service")
        self.assertEqual(rules("a.md", ok.encode()), [])
        self.assertEqual(rules("a.md", CONTACT.replace("contact", "other").encode()), ["email"])

    def test_skip_rule_by_glob(self):
        self.assertEqual(rules("third_party/AUTHORS", MAIL.encode()), [])
        self.assertEqual(rules("first_party/AUTHORS", MAIL.encode()), ["email"])

    def test_serial(self):
        self.assertEqual(rules("t.py", f'serial = "{SERIAL}"'.encode()), ["serial"])
        self.assertEqual(rules("t.py", ("2" + "0" * 12 + "1").encode()), [])     # allowlisted
        not_serials = ("https://help.example.com/hc/sections/12262368930324-Docs ts 1771026094859 "
                       "ptr 500608159232 hash a2" + "1" * 14 + "f id-" + "2" * 14 + " v1." + "2" * 14)
        self.assertEqual(rules("t.py", not_serials.encode()), [])

    def test_host_name(self):
        self.assertEqual(rules("t.rs", f'"{HOST}"'.encode()), ["host-name"])
        self.assertEqual(rules("t.rs", b'"Studio Mac" "Network RTP MIDI Session"'), [])

    def test_binary_files_are_not_read_as_text(self):
        self.assertEqual(rules("x.bin", b"\0" + LAN.encode()), [])


class Sizes(unittest.TestCase):
    def test_limits(self):
        self.assertEqual(rules("blob.bin", b"\0" * 2000, max_binary=1000), ["large-file"])
        self.assertEqual(rules("dump.json", b"{}" * 1000, max_text=1000), ["large-file"])
        self.assertEqual(rules("big/dump.json", b"{}" * 1000, max_text=1000), [])


class EndToEnd(unittest.TestCase):
    def repo(self, files):
        d = tempfile.TemporaryDirectory()
        root = Path(d.name)
        for p, data in files.items():
            (root / p).parent.mkdir(parents=True, exist_ok=True)
            (root / p).write_bytes(data)
        git = ["git", "-C", d.name, "-c", "user.name=t", "-c", "user.email=t@example.com",
               "-c", "commit.gpgsign=false"]
        subprocess.run(git[:3] + ["init", "-q"], check=True)
        subprocess.run(git + ["add", "-A"], check=True)
        subprocess.run(git + ["commit", "-qm", "t"], check=True)
        return d

    def run_guard(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = guard.main(list(args))
        return rc, out.getvalue(), err.getvalue()

    def test_dirty_and_clean_trees(self):
        allow = "[home]\nyou\n"
        with self.repo({
            "tools/check-clean-repo.allow": allow.encode(),
            "README.md": f"# t\n\nssh {LAN}\n".encode(),
            "fw/m.bin": b"\0\0\0\0" + REC,
            "src/ok.rs": b"fn main() {}\n",
        }) as d:
            pins = Path(d) / "pins.json"
            pins.write_text(json.dumps({"dsp_modules": [{"id": "123", "known": [{"sha256": sha(REC)}]}]}))
            rc, out, err = self.run_guard("--root", d, "--pins", str(pins))
            self.assertEqual(rc, 1)
            self.assertIn("README.md:3: [private-ipv4]", out)
            self.assertIn("fw/m.bin:0x4: [ua-bill]", out)
            self.assertIn("fw/m.bin:0x4: [ua-pinned]", out)
            self.assertIn("3 finding(s) in 2 of 4 files", err)
            # --rev reads the commit, not the working tree
            (Path(d) / "README.md").write_text("# t\n")
            rc2, out2, _ = self.run_guard("--root", d, "--rev", "HEAD", "--pins", str(pins))
            self.assertEqual((rc2, out2), (rc, out))
            rc3, out3, _ = self.run_guard("--root", d, "--pins", str(pins), "README.md", "src/ok.rs")
            self.assertEqual((rc3, out3), (0, ""))

    def test_allowlist_syntax_errors_stop_the_run(self):
        with self.assertRaises(SystemExit):
            guard.Allow("[ipv4]\nnot an address at all\n")
        with self.assertRaises(SystemExit):
            guard.Allow("[skip]\nno-such-rule *\n")


if __name__ == "__main__":
    unittest.main(verbosity=1)
