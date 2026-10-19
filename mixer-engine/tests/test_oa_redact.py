"""scripts/oa_redact.py: what the install logs and the install report hide.

The same cases as oa-setup's runlog.rs (tests/fixtures/redact-cases.tsv), so
the wizard's own log lines and the scripts' lines follow one set of rules.
"""

import importlib.util
import os
import pwd
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "scripts" / "oa_redact.py"
CASES = REPO / "tests" / "fixtures" / "redact-cases.tsv"

spec = importlib.util.spec_from_file_location("oa_redact", SCRIPT)
oa_redact = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oa_redact)

WORDS = oa_redact.words_for("yourname", "/home/yourname", "studio.lan",
                            "0123456789abcdef0123456789abcdef")


def cases():
    with open(CASES, encoding="utf-8", newline="") as f:
        for row in f.read().split("\n"):
            if row and not row.startswith("#"):
                inp, want = row.split("\t")
                yield inp, want


@pytest.mark.parametrize("inp,want", list(cases()))
def test_shared_cases(inp, want):
    assert oa_redact.clean_line(inp, WORDS) == want


def test_enough_shared_cases():
    assert len(list(cases())) >= 10


def test_scrub_walks_nested_reports():
    doc = {"a": ["/home/yourname/x", {"b": "serial: 20000000000123"}], "n": 3, "t": True}
    assert oa_redact.scrub(doc, WORDS) == {
        "a": ["<home>/x", {"b": "serial: <redacted>"}], "n": 3, "t": True}


def test_meaningless_identity_is_ignored():
    words = oa_redact.words_for("root", "/", "localhost", "")
    assert oa_redact.redact("root@localhost:/", words) == "root@localhost:/"


def test_control_characters_and_long_lines():
    assert oa_redact.clean_line("a\x07b\tc\n", []) == "ab\tc"
    assert oa_redact.clean_line("x" * 5000, []).endswith(" …(cut)")


def test_filter_appends_redacted_and_tees_raw(tmp_path):
    log = tmp_path / "install.log"
    me = pwd.getpwuid(os.getuid())
    raw = f"\x1b[0;32m[ OK ]\x1b[0m  serial: 20000000000123 in {me.pw_dir}\n"
    out = subprocess.run([sys.executable, str(SCRIPT), "--append", str(log), "--tee"],
                         input=raw.encode(), capture_output=True, check=True)
    assert out.stdout.decode() == raw          # the window sees the line as it was
    text = log.read_text()
    assert "20000000000123" not in text and "\x1b" not in text
    assert text.endswith("[ OK ]  serial: <redacted> in <home>\n") or me.pw_name == "root"
    assert oa_redact.os.stat(log).st_mode & 0o777 == 0o600
    # A second run appends
    subprocess.run([sys.executable, str(SCRIPT), "--append", str(log)],
                   input=b"second\n", check=True)
    assert log.read_text().count("\n") == 2


def test_filter_never_writes_through_a_link(tmp_path):
    target = tmp_path / "elsewhere"
    target.write_text("")
    link = tmp_path / "install.log"
    link.symlink_to(target)
    out = subprocess.run([sys.executable, str(SCRIPT), "--append", str(link)],
                         input=b"x\n", capture_output=True)
    assert out.returncode != 0
    assert target.read_text() == ""
