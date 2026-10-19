#!/usr/bin/env python3
"""Drive uaconsole headless against the recording fake daemon and check what reached the
(fake) Apollo.

    tools/drive/run.py SCRIPT.txt [--uaconsole BIN] [--daemon-arg ARG]... [--keep]

SCRIPT is a `uaconsole --drive` script (crates/uaconsole/src/drive.rs). Lines starting with
`#! ` are assertions on the daemon log, run after the script:

    #! hw set_mixer_param 1 0 2 1          a hardware write (name then args; `*` matches any)
    #! tree /devices/0/inputs/0/Stereo/value true      a value the daemon published
    #! no hw set_driver_param 0 *          the opposite

Exit 0 when the script ran and every assertion held. The daemon log is printed on failure.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONSOLE_RS = HERE.parent.parent
FAKE = CONSOLE_RS.parent / "mixer-engine" / "tests" / "fake_hw_daemon.py"


def start_daemon(log, extra):
    p = subprocess.Popen([sys.executable, str(FAKE), "--log", str(log), *extra],
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                         env={**os.environ, "HOME": str(log.parent / "home")})
    for line in p.stdout:
        if line.startswith("LISTENING "):
            return p, int(line.split()[1])
    raise SystemExit("fake daemon did not start")


def matches(pattern, entry):
    if pattern[0] == "hw":
        if "hw" not in entry or entry["hw"] != pattern[1]:
            return False
        args = [str(a) for a in entry["args"]]
        want = pattern[2:]
        return len(args) >= len(want) and all(w == "*" or w == a for w, a in zip(want, args))
    if pattern[0] == "tree":
        return entry.get("tree") == pattern[1] and (len(pattern) < 3 or str(entry.get("value")) == pattern[2])
    raise SystemExit(f"bad assertion {' '.join(pattern)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("script", type=Path)
    ap.add_argument("--uaconsole", default=os.environ.get("UACONSOLE", "uaconsole"))
    ap.add_argument("--daemon-arg", action="append", default=[])
    ap.add_argument("--keep", action="store_true", help="keep the temp dir")
    a = ap.parse_args()

    tmp = Path(tempfile.mkdtemp(prefix="oa-drive-"))
    (tmp / "home").mkdir()
    log = tmp / "calls.jsonl"
    daemon, port = start_daemon(log, a.daemon_arg)
    try:
        run = subprocess.run([a.uaconsole, "--drive", str(a.script), "--engine", f"127.0.0.1:{port}", "--writes"],
                             cwd=a.script.parent, text=True, capture_output=True)
        sys.stdout.write(run.stdout)
        if run.returncode:
            sys.stderr.write(run.stderr)
            return 1
        time.sleep(0.5)
    finally:
        daemon.terminate()
        daemon.wait()
    entries = [json.loads(l) for l in log.read_text().splitlines() if l.strip()]
    checks = [l[3:].split() for l in a.script.read_text().splitlines() if l.startswith("#! ")]
    failed = []
    for c in checks:
        negate = c[0] == "no"
        pat = c[1:] if negate else c
        hit = any(matches(pat, e) for e in entries)
        if hit == negate:
            failed.append(" ".join(c))
    for f in failed:
        print(f"FAIL {f}")
    if failed:
        print("--- daemon log")
        for e in entries:
            if "hw" in e or any(k in e.get("tree", "") for k in ("Stereo", "Link", "Active", "Name")):
                print(json.dumps(e))
    if not a.keep:
        subprocess.run(["rm", "-rf", str(tmp)])
    print(f"{len(checks) - len(failed)}/{len(checks)} assertions passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
