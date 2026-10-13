#!/usr/bin/env python3
"""Extract UAD Console's engine traffic from open-apollo daemon traces.

The traces are `ua_mixer_daemon.py --trace` logs of UAD Console 1.2.x (Wine) talking to the
open-apollo daemon on 4710 (`RECV ← client-N: …` lines). Console is the client that sends
`subscribe mode=multi`. Writes, into OUT (default: engine-model's test fixtures):

  subscriptions.txt  every property path Console subscribed (the daemon's `state_tree
                     SUBSCRIBE` lines), `/value` and queries stripped, sorted, union over runs
  gets.txt           every path Console `get`s (queries stripped), sorted
  ui-writes.txt      Console's set/post traffic outside the connect handshake, grouped into
                     UI actions (an /UndoRecording bracket, or one unbracketed write), one action
                     per block: `# run time` then one `set|post PATH VALUE` line per command, with
                     `func_id` removed and other query parameters kept

Usage: extract_traces.py [RUNS_DIR] [OUT_DIR]
"""
import json
import os
import re
import sys

RUNS = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/wine-ua-work/runs")
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(
    os.path.dirname(__file__), "../../crates/engine-model/tests/fixtures/console-trace")

LINE = re.compile(r"^(\d\d:\d\d:\d\d) .*RECV ← (client-\d+): (.*?)\s*$")
# The daemon logs each subscribed path in full (RECV lines are cut at ~200 characters).
SUB = re.compile(r"^\d\d:\d\d:\d\d state_tree DEBUG SUBSCRIBE (client-\d+) → (\S+)")
HANDSHAKE = re.compile(r"^(set /(?:sleep|Sleep) |set /status/timeout_ms|post /request_challenge|post command_format|post /response)")


def strip_query(path, keep=()):
    if "?" not in path:
        return path
    base, q = path.split("?", 1)
    kept = [kv for kv in q.split("&") if kv.split("=", 1)[0] in keep]
    return base + ("?" + "&".join(kept) if kept else "")


def prop(path):
    path = strip_query(path)
    return path[: -len("/value")] if path.endswith("/value") else path


subs, gets, actions = set(), set(), []
for run in sorted(os.listdir(RUNS)):
    log = os.path.join(RUNS, run, "daemon.log")
    if not os.path.isfile(log):
        continue
    console = None
    open_bracket = None
    with open(log, errors="replace") as f:
        for line in f:
            sm = SUB.match(line)
            if sm:
                if sm.group(1) == console:
                    subs.add(prop(sm.group(2)))
                continue
            m = LINE.match(line)
            if not m:
                continue
            t, client, cmd = m.groups()
            if console is None and cmd == "subscribe mode=multi":
                console = client
            if client != console:
                continue
            verb, _, rest = cmd.partition(" ")
            if verb == "get":
                gets.add(strip_query(rest.strip()))
            elif verb in ("set", "post") and not HANDSHAKE.match(cmd):
                path, _, value = rest.partition(" ")
                text = f"{verb} {strip_query(path, keep=('context_type',))} {value}".rstrip()
                if path.startswith("/UndoRecording/value"):
                    if value == "true":
                        open_bracket = {"run": run, "time": t, "cmds": [text]}
                    elif open_bracket is not None:
                        open_bracket["cmds"].append(text)
                        actions.append(open_bracket)
                        open_bracket = None
                elif open_bracket is not None:
                    open_bracket["cmds"].append(text)
                else:
                    actions.append({"run": run, "time": t, "cmds": [text]})

os.makedirs(OUT, exist_ok=True)
with open(os.path.join(OUT, "subscriptions.txt"), "w") as f:
    f.write("\n".join(sorted(subs)) + "\n")
with open(os.path.join(OUT, "gets.txt"), "w") as f:
    f.write("\n".join(sorted(gets)) + "\n")
with open(os.path.join(OUT, "ui-writes.txt"), "w") as f:
    for a in actions:
        f.write(f"# {a['run']} {a['time']}\n")
        f.write("\n".join(a["cmds"]) + "\n\n")
print(f"{len(subs)} subscribed paths, {len(gets)} gets, {len(actions)} UI actions → {OUT}")
