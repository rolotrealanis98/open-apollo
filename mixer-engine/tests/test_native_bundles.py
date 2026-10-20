"""Native plug-in bundles: the carrier recipe and which providers the registry offers.

Synthetic data only (a made-up descriptor and module); the emulator is never imported.
"""
import hashlib
import json
import struct

import native_carrier
import profiled_bundle_registry as registry


def fake_module(tmp_path):
    """A made-up kind-1 (data) module for DSP arch 2, stored by its hash."""
    payload = bytes(range(64))
    record = b"Bill" + struct.pack("<IIII", 0x123, 1 | 2 << 24, len(payload), 4) + payload
    digest = hashlib.sha256(record).hexdigest()
    (tmp_path / f"{digest}.bill").write_bytes(record)
    return dict(id=0x123, architecture=2, kind=1, sha256=digest)


def descriptor(tmp_path, **extra):
    return dict(plugin="Test", render_id=0x456, rows=[[1, 8, 0xffffffff, 0]], resources=[fake_module(tmp_path)],
                scratch=[0, 0, 0], io=[1, 1], latency_samples=0, sample_rate=48000, **extra)


def test_carrier_recipe(tmp_path, monkeypatch):
    strips = tmp_path / "dsp"
    strips.mkdir()
    for name in native_carrier.STRIP_MODULES:
        (strips / f"{name}.bill").write_bytes(name.encode())
    monkeypatch.setattr(native_carrier, "DSPDIR", str(strips))
    carrier = native_carrier.GeneratedOrdinaryCarrier(
        descriptor(tmp_path), {"shadow": {"0x5": 7, "0x6": 9}}, input_index=1, module_store=tmp_path)
    args = carrier.create_arguments()
    assert args["input_pair"] == (1, 32) and args["render_id"] == 0x456
    assert len(args["private_rows"]) == 11  # chain head + Mic Input Strip (10) + the plug-in's root row
    assert args["private_rows"][10][2] == native_carrier.L(0, 8)
    assert len(args["resources"]) == 6       # five strip modules + the plug-in's
    assert native_carrier.check(args["private_rows"], list(args["commands"])) > 0
    # The startup words, after the header words the carrier owns (0-2, and 4 with its tap bit).
    assert carrier.startup_observations() == ((10, 0, (0x40000000, 1, 1)), (10, 4, (4, 7, 9)))


def test_write_masks_gaps():
    assert native_carrier.write(3, {1: 10, 3: 30}) == [0x00140005, 3 << 24 | 1, 0b1, 10, 30]


class Stub:
    sample_rate = 48000

    def __init__(self, path, input_index, *, data, module_store):
        self.name, self.input_index = data["name"], input_index


class StereoStub(Stub):
    pass


def bundles(tmp_path, *entries):
    index = {"uad_sdk": str(tmp_path / "UAD2SDK.dll"), "module_store": str(tmp_path), "bundles": []}
    for name, layout in entries:
        file = f"{name.replace(' ', '-')}-{layout}-runtime-bundle.json"
        (tmp_path / file).write_text(json.dumps({"status": "pass", "name": name, "layout": layout,
                                                 "carrier_channels": 2 if layout == "stereo" else 1}))
        index["bundles"].append({"name": name, "layout": layout, "file": file})
    (tmp_path / "index.json").write_text(json.dumps(index))


def test_registry_offers_each_verified_layout_from_its_own_bundle(tmp_path, monkeypatch):
    monkeypatch.setattr(registry, "ProfiledCarrierProvider", Stub)
    monkeypatch.setattr(registry, "StereoProfiledCarrierProvider", StereoStub)
    monkeypatch.delenv("OA_UAD_SDK", raising=False)
    monkeypatch.delenv("OA_MODULE_STORE", raising=False)
    monkeypatch.setattr(registry, "known_good", lambda: {
        "UAD Both": {"layouts": ["mono", "stereo"]}, "UAD Mono Only": {"layouts": ["mono"]}})
    bundles(tmp_path, ("UAD Both", "mono"), ("UAD Both", "stereo"), ("UAD Mono Only", "mono"),
            ("UAD Mono Only", "stereo"), ("UAD Not Yet Live", "mono"))
    offered = registry.create_profiled_providers(tmp_path)
    inputs = (0, 1, 2, 3, 14, 15, 16, 17)       # analog 1-4, Virtual 1-4
    assert sorted((p.name, p.kind.__name__, p.input_index) for p in offered) == [
        *[("UAD Both", "StereoStub", i) for i in (0, 2, 14, 16)],
        *[("UAD Both", "Stub", i) for i in inputs], *[("UAD Mono Only", "Stub", i) for i in inputs]]
    assert callable(offered.composer)
    everything = registry.create_profiled_providers(tmp_path, advertise_all=True)
    assert {(p.name, p.kind.__name__) for p in everything} >= {("UAD Mono Only", "StereoStub"), ("UAD Not Yet Live", "Stub")}


def test_offering_reads_no_bundle_and_the_first_use_builds_once(tmp_path, monkeypatch):
    """The daemon offers every map at startup from index.json alone; a bundle is parsed (once for
    all its inputs) and its provider built when a load first needs it."""
    import copy
    built = []

    class Counting(Stub):
        def __init__(self, path, input_index, *, data, module_store):
            super().__init__(path, input_index, data=data, module_store=module_store)
            built.append(input_index)
            self.recipe = "built"
    monkeypatch.setattr(registry, "ProfiledCarrierProvider", Counting)
    monkeypatch.setattr(registry, "StereoProfiledCarrierProvider", type("StereoCounting", (Counting,), {}))
    monkeypatch.setattr(registry, "known_good", lambda: {"UAD A": {"layouts": ["mono", "stereo"]}})
    reads = []
    original = registry.Path.read_text
    monkeypatch.setattr(registry.Path, "read_text", lambda self, *a, **k: reads.append(self.name) or original(self, *a, **k))
    bundles(tmp_path, ("UAD A", "mono"), ("UAD A", "stereo"))
    offered = registry.create_profiled_providers(tmp_path)
    assert reads == ["index.json"] and built == []
    mono = sorted((p for p in offered if p.kind is Counting), key=lambda p: p.input_index)
    stereo = [p for p in offered if p.kind is not Counting]
    assert [p.slot for p in mono] == [f"/devices/0/inputs/{i}/effects/0" for i in (0, 1, 2, 3, 14, 15, 16, 17)]
    assert not hasattr(mono[1], "channels") and [p.channels for p in stereo] == [(0, 1), (2, 3), (14, 15), (16, 17)]
    assert all(p.sample_rate == 48000 for p in offered) and built == []   # all the backend asks to offer
    assert mono[1].recipe == "built" and mono[1].recipe == "built" and built == [1]
    assert mono[2].built().input_index == 2 and built == [1, 2]
    assert reads.count("UAD-A-mono-runtime-bundle.json") == 1             # one parse for every input
    clone = copy.deepcopy(mono[3])
    assert type(clone) is Counting and clone is not mono[3].built() and clone.input_index == 3


def test_a_bundle_found_unusable_fails_its_load_not_the_others(tmp_path, monkeypatch, caplog):
    monkeypatch.setattr(registry, "ProfiledCarrierProvider", Stub)
    monkeypatch.setattr(registry, "known_good", lambda: {"UAD A": {"layouts": ["mono"]}, "UAD B": {"layouts": ["mono"]}})
    bundles(tmp_path, ("UAD A", "mono"), ("UAD B", "mono"))
    (tmp_path / "UAD-B-mono-runtime-bundle.json").write_text(json.dumps({"status": "fail", "name": "UAD B"}))
    offered = {p.name: p for p in registry.create_profiled_providers(tmp_path, inputs=(0,))}
    assert set(offered) == {"UAD A", "UAD B"}
    assert offered["UAD A"].built().name == "UAD A"
    import pytest
    with pytest.raises(ValueError, match="passing runtime bundle required"):
        offered["UAD B"].built()
    assert "UAD-B-mono-runtime-bundle.json unusable" in caplog.text


def test_a_native_transaction_runs_off_the_event_loop(monkeypatch):
    """The load runs on the daemon's worker thread: gets are served meanwhile, the slot reads
    Loading, a set waits (in order) until the load has committed, and state the loop changes
    meanwhile (a hardware readback) survives the publication, which is staged on the loop."""
    import asyncio, threading
    from test_plugin_native_layouts import provider, rig
    from test_plugin_native import FIELD
    from protocol import Command
    from plugin_test_support import MemoryWriter
    from ua_mixer_daemon import MixerClient, MixerDaemon
    p = provider(1)
    gate, threads = threading.Event(), []
    original_reset = type(p).reset

    def reset(self):
        threads.append(threading.current_thread().name)
        assert gate.wait(5)
        original_reset(self)
    monkeypatch.setattr(type(p), "reset", reset, raising=False)
    state, driver, backend, tx = rig((p,), max_instances=4)
    daemon = MixerDaemon(state, None, helper_tree=backend.publisher.helper,
                         plugin_transactions=tx, plugin_hosting=True)
    writer = MemoryWriter()
    client = MixerClient(None, writer)

    async def run():
        daemon._handle_set(client, Command("set", p.slot + "/EffectName/value", p.name, raw_params={"func_id": "7"}))
        assert state.get_value(p.slot + "/Status") == "Loading" and not writer.frames
        daemon._handle_get(client, Command("get", p.slot + "/Status/value", raw_params={"func_id": "8"}))
        assert json.loads(writer.frames[-1].rstrip(b"\0"))["data"] == "Loading"  # served while loading
        daemon._handle_set(client, Command("set", f"{p.slot}/{FIELD}/value", .75, raw_params={"func_id": "9"}))
        assert len(writer.frames) == 1  # waits for the load
        state.add_runtime_property("/devices/0/Readback", {"type": "int", "value": 3})
        gate.set()
        for _ in range(500):
            if len(writer.frames) == 3:
                break
            await asyncio.sleep(0.01)
    asyncio.run(run())
    assert threads and all(name.startswith("native-plugin") for name in threads)
    replies = [json.loads(frame.rstrip(b"\0")) for frame in writer.frames[1:]]
    assert [r["parameters"]["func_id"] for r in replies] == [7, 9] and not any("error" in r for r in replies)
    assert state.get_value(p.slot + "/EffectName") == p.name and state.get_value(p.slot + "/Status") == ""
    assert list(driver.instances.values()) == [75]
    assert state.get_value("/devices/0/Readback/value") == 3


def test_known_good_list_holds_no_dsp_data():
    for entry in registry.known_good().values():
        assert set(entry) <= {"name", "stage", "family", "layouts", "latency_samples", "dynamic_refs"}
        assert set(entry["layouts"]) <= {"mono", "stereo"}
        assert all(len(ref) == 2 for ref in entry.get("dynamic_refs", ()))


def test_maps_landing_while_the_daemon_runs_are_offered_once(tmp_path, monkeypatch):
    monkeypatch.setattr(registry, "ProfiledCarrierProvider", Stub)
    monkeypatch.setattr(registry, "StereoProfiledCarrierProvider", StereoStub)
    monkeypatch.setattr(registry, "known_good", lambda: {
        "UAD A": {"layouts": ["mono", "stereo"]}, "UAD B": {"layouts": ["mono", "stereo"]}})
    watcher = registry.BundleWatcher(tmp_path)
    first = registry.create_profiled_providers(watcher=watcher)
    assert list(first) == [] and first.growable          # no maps yet: hosting starts empty
    bundles(tmp_path, ("UAD A", "mono"), ("UAD A", "stereo"))
    assert {(p.name, p.kind.__name__) for p in watcher.poll()} == {("UAD A", "Stub"), ("UAD A", "StereoStub")}
    assert watcher.poll() == []                          # unchanged index: nothing new
    bundles(tmp_path, ("UAD A", "mono"), ("UAD A", "stereo"), ("UAD B", "mono"))
    import os
    os.utime(tmp_path / "index.json", ns=(1, 10**18))    # a new index even within one clock tick
    assert {(p.name, p.kind.__name__) for p in watcher.poll()} == {("UAD B", "Stub")}
    assert "2 plug-ins (1 in both layouts)" in watcher.summary()
    (tmp_path / "progress.json").write_text(json.dumps({"state": "running", "done": 3, "total": 9, "eta_seconds": 300}))
    assert watcher.progress()["done"] == 3


def test_daemon_offers_landed_maps_and_publishes_progress(tmp_path, monkeypatch):
    from ua_mixer_daemon import MixerDaemon
    from test_plugin_native_layouts import provider, rig
    p = provider(1)
    state, driver, backend, tx = rig((p,), max_instances=4)
    daemon = MixerDaemon(state, None, helper_tree=backend.publisher.helper,
                         plugin_transactions=tx, plugin_hosting=True)

    class Watcher:
        def __init__(self):
            self.landed = []

        def progress(self):
            return {"state": "running", "done": 2, "total": 5, "eta_seconds": 240}

        def poll(self):
            landed, self.landed = self.landed, []
            return landed

        def summary(self):
            return "test"
    daemon.plugin_maps = Watcher()
    published = []
    monkeypatch.setattr(daemon, "_publish_native_capabilities", lambda: published.append(True))
    daemon._plugin_maps_poll()
    assert state.get_value("/NativePluginMapsStatus") == "Mapping plug-ins 2/5 · ~4 min left"
    assert state.get_value("/NativePluginMapsTotal") == 5 and not published
    newcomer = provider(2)
    newcomer.name = "UAD Newcomer"
    daemon.plugin_maps.landed = [newcomer]
    daemon._native_busy = True                           # a load is running: wait for it
    daemon._plugin_maps_poll()
    assert not published
    daemon._native_busy = False
    daemon._plugin_maps_poll()
    assert published and (newcomer.slot, "UAD Newcomer", (2,)) in backend.providers


def test_licence_answer_and_rescan_start_the_mapper_once_per_change(tmp_path, monkeypatch):
    from ua_mixer_daemon import MixerDaemon
    from state_tree import StateTree
    import subprocess
    daemon = MixerDaemon(StateTree(), None)
    daemon.plugin_maps = object()
    unit = tmp_path / "systemd/user/open-apollo-plugin-maps.service"
    unit.parent.mkdir(parents=True)
    unit.write_text("[Service]\n")
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    started = []
    monkeypatch.setattr(subprocess, "Popen", lambda argv, **kw: started.append(argv))
    monkeypatch.setattr("shutil.which", lambda name: "/usr/bin/" + name)
    for i, (name, auth) in enumerate((("UAD A", True), ("UAD B", False))):
        daemon.state.add_runtime_property(f"/plugins/{i}/Name", {"type": "string", "value": name})
        daemon.state.add_runtime_property(f"/plugins/{i}/Authorized", {"type": "bool", "value": auth})
    daemon._plugin_maps_on_licence()
    daemon._plugin_maps_on_licence()                     # same answer: no second build
    assert started == [["systemctl", "--user", "start", "--no-block", "open-apollo-plugin-maps.service"]]
    daemon.state.set_value("/plugins/1/Authorized", True)   # a licence appeared
    daemon._plugin_maps_on_licence()
    assert len(started) == 2
