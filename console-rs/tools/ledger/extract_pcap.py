#!/usr/bin/env python3
"""Extract UAD Console 1.3.1's writes from a loopback capture of its 4710 connection.

Reads a pcap (tshark required), reassembles every client→4710 TCP stream, splits NUL-framed
commands, and picks Console's stream: the one whose writes carry `context_type=main`.
Writes OUT/v131-ui-writes.txt in the same block format as extract_traces.py (one UI action per
block: an /UndoRecording bracket or one unbracketed write; `func_id` removed, other query
parameters kept; consecutive identical-path sets inside a bracket are kept, so drags show their
streaming), and OUT/v131-gets.txt (Console's gets, queries stripped, with counts).

Usage: extract_pcap.py PCAP [OUT_DIR]
"""
import collections
import os
import re
import subprocess
import sys

PCAP = sys.argv[1]
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(
    os.path.dirname(__file__), "../../crates/engine-model/tests/fixtures/console-trace")

rows = subprocess.run(
    ["tshark", "-r", PCAP, "-Y", "tcp.dstport==4710 && tcp.len>0", "-T", "fields",
     "-e", "frame.time_relative", "-e", "tcp.stream", "-e", "tcp.payload"],
    check=False, capture_output=True, text=True).stdout.splitlines()  # exit 14: truncated last packet
buf = collections.defaultdict(bytearray)
cmds = collections.defaultdict(list)
for row in rows:
    t, stream, payload = row.split("\t")
    buf[stream] += bytes.fromhex(payload)
    while b"\0" in buf[stream]:
        msg, _, rest = bytes(buf[stream]).partition(b"\0")
        buf[stream] = bytearray(rest)
        cmds[stream].append((float(t), msg.decode("utf-8", "replace").strip()))

console = next((s for s, c in cmds.items() if any("context_type=main" in m for _, m in c)), None)
if console is None:
    sys.exit("no stream with context_type=main writes")


def drop_func_id(path):
    if "?" not in path:
        return path
    base, q = path.split("?", 1)
    kept = [kv for kv in q.split("&") if not kv.startswith("func_id=")]
    return base + ("?" + "&".join(kept) if kept else "")


actions, open_bracket, gets = [], None, collections.Counter()
for t, m in cmds[console]:
    verb, _, rest = m.partition(" ")
    if verb == "get":
        gets[rest.split("?")[0].strip()] += 1
        continue
    if verb not in ("set", "post"):
        continue
    path, _, value = rest.partition(" ")
    text = f"{verb} {drop_func_id(path)} {value}".rstrip()
    if path.startswith("/UndoRecording/value"):
        if value == "true":
            open_bracket = {"time": t, "cmds": [text]}
        elif open_bracket is not None:
            open_bracket["cmds"].append(text)
            actions.append(open_bracket)
            open_bracket = None
    elif open_bracket is not None:
        open_bracket["cmds"].append(text)
    else:
        actions.append({"time": t, "cmds": [text]})

os.makedirs(OUT, exist_ok=True)
with open(os.path.join(OUT, "v131-ui-writes.txt"), "w") as f:
    for a in actions:
        f.write(f"# v131 t={a['time']:.3f}\n" + "\n".join(a["cmds"]) + "\n\n")
with open(os.path.join(OUT, "v131-gets.txt"), "w") as f:
    for p, n in sorted(gets.items()):
        f.write(f"{n}\t{p}\n")
print(f"Console stream {console}: {len(cmds[console])} commands, {len(actions)} UI actions, {len(gets)} distinct gets → {OUT}")
