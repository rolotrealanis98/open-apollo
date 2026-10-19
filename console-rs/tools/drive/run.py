#!/usr/bin/env python3
"""Drive uaconsole headless against the recording fake daemon and check what reached the
(fake) Apollo.

    tools/drive/run.py SCRIPT.txt [--uaconsole BIN] [--daemon-arg ARG]... [--catalogue]
                      [--native-slots] [--specs] [--pipewire-fixture] [--keep]

`tools/drive/suite.sh [BIN]` runs every script in scripts/, with the flags of its `#: ` line.

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
        # the value may contain spaces: everything after the path
        return entry.get("tree") == pattern[1] and (len(pattern) < 3 or str(entry.get("value")) == " ".join(pattern[2:]))
    raise SystemExit(f"bad assertion {' '.join(pattern)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("script", type=lambda p: Path(p).resolve())
    ap.add_argument("--uaconsole", default=os.environ.get("UACONSOLE", "uaconsole"))
    ap.add_argument("--daemon-arg", action="append", default=[])
    ap.add_argument("--keep", action="store_true", help="keep the temp dir")
    ap.add_argument("--catalogue", action="store_true",
                    help="give the daemon the synthetic plug-in catalogue (fixtures/catalogue)")
    ap.add_argument("--native-slots", action="store_true",
                    help="the daemon also publishes native slot lists for that catalogue")
    ap.add_argument("--specs", action="store_true",
                    help="give uaconsole the synthetic panel specs (fixtures/specs), drawn as wireframes")
    ap.add_argument("--pipewire-fixture", action="store_true",
                    help="the daemon serves the synthetic pw-dump (mixer-engine/tests/fixtures) as PipeWire")
    a = ap.parse_args()
    if a.catalogue or a.native_slots:
        a.daemon_arg += ["--catalogue", str(HERE / "fixtures" / "catalogue")]
    if a.native_slots:
        a.daemon_arg += ["--native-slots"]
    if a.pipewire_fixture:
        a.daemon_arg += ["--pipewire-fixture", str(FAKE.parent / "fixtures" / "pipewire_dump.json")]

    tmp = Path(tempfile.mkdtemp(prefix="oa-drive-"))
    (tmp / "home").mkdir()
    log = tmp / "calls.jsonl"
    daemon, port = start_daemon(log, a.daemon_arg)
    # Plug-in window scales are saved under the config home: keep the user's out of it.
    env = {**os.environ, "XDG_CONFIG_HOME": str(tmp / "config")}
    if a.specs:
        env.update(OA_PLUGIN_SPECS=str(HERE / "fixtures" / "specs"), OA_PLUGIN_WIREFRAME="1")
    try:
        run = subprocess.run([a.uaconsole, "--drive", str(a.script), "--engine", f"127.0.0.1:{port}", "--writes"],
                             cwd=a.script.parent, text=True, capture_output=True, env=env)
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
