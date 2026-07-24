#!/usr/bin/env python3
"""
Dump the FULL x8p control tree from a UA Mixer Engine on TCP 4710
-> device_map_apollo_x8p.json  (for open-apollo #52).

The engine's `get /?recursive` only lists top-level child *names* (empty stubs), so we
walk the tree explicitly: get each node, read its children, recurse. This reproduces the
full `controls` tree (the x4 map is ~3.6 MB, so expect it to take a minute).

Run on the Mac (x8p connected + UAD Console open, so UA Mixer Engine serves 4710):
    python3 dump_x8p_tree.py --host 127.0.0.1
Or from the LAN:
    python3 dump_x8p_tree.py --host <mac-ip>
"""
import socket, json, sys, time, argparse

def recv_msg(sock):
    """Read one null-terminated JSON message."""
    buf = b""
    while b"\x00" not in buf:
        c = sock.recv(65536)
        if not c:
            break
        buf += c
    raw = buf.split(b"\x00", 1)[0]
    return json.loads(raw) if raw.strip() else None

def get(sock, path):
    sock.sendall(("get " + path + "\x00").encode("utf-8"))
    # skip any stray non-matching messages (e.g. meter updates), take the reply for `path`
    for _ in range(20):
        msg = recv_msg(sock)
        if msg is None:
            return None
        if msg.get("path", "").rstrip("/") == path.rstrip("/"):
            return msg
    return msg

count = [0]
def walk(sock, path):
    resp = get(sock, path)
    if not resp:
        return {}
    data = resp.get("data", {})
    count[0] += 1
    if count[0] % 50 == 0:
        print(f"  ...{count[0]} nodes", file=sys.stderr)
    ch = data.get("children", {})
    for name in list(ch.keys()):
        child_path = path.rstrip("/") + "/" + name
        data["children"][name] = walk(sock, child_path)
    return data

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--port", type=int, default=4710)
    ap.add_argument("--out", default="device_map_apollo_x8p.json")
    a = ap.parse_args()

    s = socket.create_connection((a.host, a.port)); s.settimeout(10)
    print("connected — walking the full tree (can take ~1 min)...", file=sys.stderr)
    tree = walk(s, "/")
    out = {
        "timestamp": int(time.time()), "host": a.host, "port": a.port,
        "tree_path": "/", "device_name": "Apollo x8p", "controls": tree,
    }
    json.dump(out, open(a.out, "w"), indent=1)
    kb = len(json.dumps(out)) // 1024
    print(f"wrote {a.out}: {count[0]} nodes, {kb} KB", file=sys.stderr)
    if kb < 100:
        print("!! still small — the tree may need auth/subscribe first; tell Claude the node count.", file=sys.stderr)

if __name__ == "__main__":
    main()
