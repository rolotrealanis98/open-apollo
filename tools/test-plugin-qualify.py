#!/usr/bin/env python3
"""Synthetic tests for plugin-qualify.py and the PLUGINS.md it feeds (no
Apollo, no UA data): python3 tools/test-plugin-qualify.py"""
import importlib.util
import json
import socket
import tempfile
import threading
from pathlib import Path
from types import SimpleNamespace


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


q = load("q", "plugin-qualify.py")
gen = load("gen", "gen-status-graph.py")

NAME = "UAD Test Comp"
SLOT = "/devices/0/inputs/0/effects/0"


def sweep(maximum, texts):
    return {"index": 0, "maximum": maximum, "samples": [
        {"integer": i, "formatted": {"console_trimmed": t}}
        for i, t in zip((0, maximum // 2, maximum), texts)]}


BUNDLE = {"control_sweep": [
    dict(sweep(2, ["Off", "Mid", "On"]), index=0),
    dict(sweep(32768, ["0.0", "5.0", "10.0"]), index=1),
    dict(sweep(32768, ["-inf", "0", "+6"]), index=2),
]}
SPEC = {"effectName": NAME, "parameters": [{}, {}, {}],
        "elements": [{"param": 0}, {"frameSource": {"params": [1]}}, {"param": 2}],
        "meterSources": {"m": {"vendorSpecificFloat": 4}}}


class FakeDaemon(threading.Thread):
    """Answers the commands plugin-qualify sends.  Parameter 2 refuses its
    maximum; parameter 1 formats its middle as "5.1"; meters are refused,
    as the daemon does today."""

    def __init__(self):
        super().__init__(daemon=True)
        self.srv = socket.socket()
        self.srv.bind(("127.0.0.1", 0))
        self.srv.listen(1)
        self.port = self.srv.getsockname()[1]
        self.tree = {"/NativePluginHosting/value": True, "/SampleRate/value": 48000,
                     "/plugins/0/Name/value": NAME,
                     "/plugins/0/NativeLoadSupported/value": True,
                     "/plugins/0/NativeMonoSlots/value": SLOT,
                     "/plugins/0/NativeStereoSlots/value": "",
                     SLOT + "/EffectName/value": ""}
        for k in range(3):
            self.tree["%s/parameters/%d/NormalizedValue/value" % (SLOT, k)] = 0.0
            self.tree["%s/parameters/%d/StringValue/value" % (SLOT, k)] = ""

    def text(self, k, v):
        b = BUNDLE["control_sweep"][k]
        for s in b["samples"]:
            if abs(s["integer"] / b["maximum"] - v) < 1e-9:
                t = s["formatted"]["console_trimmed"]
                return "5.1" if (k, t) == (1, "5.0") else t
        return "?"

    def run(self):
        conn, _ = self.srv.accept()
        buf = b""
        send = lambda m: conn.sendall(json.dumps(m).encode() + b"\0")
        send({"path": "/initialized/value", "data": True})     # unrequested push
        while True:
            data = conn.recv(65536)
            if not data:
                return
            buf += data
            while b"\0" in buf:
                raw, buf = buf.split(b"\0", 1)
                verb, rest = raw.decode().split(" ", 1)
                if verb == "get":
                    path, _, query = rest.partition("?")
                    if path.endswith("/VendorSpecific/value"):
                        send({"path": path, "error": "openapollo.plugin_remote_readback_unavailable",
                              "detail": "remote variable has no authorized native mapping"})
                    elif path == SLOT + "/parameters":
                        send({"path": path, "data": {"children": {str(k): {} for k in range(3)}}})
                    elif path in self.tree:
                        send({"path": path, "data": self.tree[path]})
                    else:
                        send({"path": path, "error": "Unable to resolve path for get."})
                    continue
                target, value = rest.split(" ", 1)
                path, func = target.split("?func_id=")
                value, params = json.loads(value), {"func_id": int(func)}
                if path.endswith("NormalizedValue/value"):
                    k = int(path.split("/")[-3])
                    if k == 2 and value == 1.0:
                        send({"path": path, "error": "openapollo.plugin_transaction_failed",
                              "detail": "Control apply raised ValueError", "parameters": params})
                        continue
                    self.tree[path.replace("NormalizedValue", "StringValue")] = self.text(k, value)
                self.tree[path] = value
                send({"path": path, "data": value, "parameters": params})


def main():
    # panel: hidden parameter and unbound element
    assert q.panel_check(SPEC, 3) == ("pass", "3/3")
    assert q.panel_check(dict(SPEC, elements=[{"param": 0}, {"param": 1}]), 3)[0] == "fail"
    assert "unbound ordinals 3" in q.panel_check(dict(SPEC, elements=SPEC["elements"] + [{"param": 3}]), 3)[1]
    assert q.scrub("/home/you/x 123456789012345") == "~/x …"

    with tempfile.TemporaryDirectory() as t:
        t = Path(t)
        maps, specs, out = t / "maps", t / "specs", t / "out"
        maps.mkdir(), specs.mkdir()
        (maps / "test-comp-mono-runtime-bundle.json").write_text(json.dumps(BUNDLE))
        (maps / "index.json").write_text(json.dumps({
            "bundles": [{"name": NAME, "layout": "mono", "file": "test-comp-mono-runtime-bundle.json"}],
            "failures": {"UAD Broken": {"reasons": {"stereo": "execution blocked"}}}}))
        (specs / "PluginSpec_test_comp.json").write_text(json.dumps(SPEC))

        fake = FakeDaemon()
        fake.start()
        common = SimpleNamespace(out=str(out), maps=str(maps), specs=str(specs),
                                 uad=None, by="tester", how=q.HOW)
        q.offline(SimpleNamespace(**vars(common), only=None), q.Recorder(common))
        args = SimpleNamespace(**vars(common), device="Apollo x4", only=None, layout=None,
                               signal=False, host="127.0.0.1", port=fake.port)
        q.live(args, q.Recorder(args))
        q.mark(SimpleNamespace(plugin="Test Comp", layout="mono", check="audio",
                               result="pass", detail=""),
               q.Recorder(SimpleNamespace(**vars(common), device="Apollo x4", rate=48000)))

        d = json.loads((out / "test-comp.json").read_text())
        mono = {k: (v["result"], v.get("detail")) for k, v in d["layouts"]["mono"].items()}
        assert mono == {
            "map": ("pass", "3 parameters"), "panel": ("pass", "3/3"),
            "load": ("pass", "loads, reads back, unloads"),
            "controls": ("fail", "2/3; refused ordinals 2"),
            "text": ("fail", "2/3; differs at ordinals 1"),
            "meters": ("fail", "meter read refused: plugin_remote_readback_unavailable "
                               "remote variable has no authorized native mapping"),
            "audio": ("pass", None)}, mono
        assert list(d["layouts"]["mono"]) == ["map", "panel", "load", "controls", "text",
                                              "meters", "audio"]       # check order
        assert d["layouts"]["mono"]["load"]["rate"] == 48000
        assert d["layouts"]["mono"]["audio"]["how"] == "by hand"
        assert fake.tree[SLOT + "/EffectName/value"] == ""             # unloaded
        assert fake.tree[SLOT + "/parameters/1/NormalizedValue/value"] == 0.0   # restored
        broken = json.loads((out / "broken.json").read_text())["layouts"]
        assert broken["stereo"]["map"]["result"] == "fail" and "mono" not in broken

        checks = gen.load_checks(str(out))
        md, working, total = gen.plugins_md(
            [["Test Comp", "live"], ["Other", "ready"], ["x", "unlicensed"]], checks)
        assert (working, total) == (0, 4), (working, total)   # Test Comp, Other, Broken + 1 hidden
        assert "### meters: 1 fail" in md and "| Other | – |" in md, md
        assert "| Test Comp | mono | ✅ | ✅ 3/3 | ✅ | ❌ 2/3; refused ordinals 2 |" in md, md
        good = {c: {"result": "pass"} for c in gen.CHECK_NAMES}
        assert gen.plugins_md([["Test Comp", "live"]], {"Test Comp": {"mono": good}})[1] == 1
    print("ok")


if __name__ == "__main__":
    main()
