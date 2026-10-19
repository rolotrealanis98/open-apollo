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
    assert sorted((p.name, type(p).__name__, p.input_index) for p in offered) == [
        ("UAD Both", "StereoStub", 0), ("UAD Both", "StereoStub", 2),
        *[("UAD Both", "Stub", i) for i in range(4)], *[("UAD Mono Only", "Stub", i) for i in range(4)]]
    assert callable(offered.composer)
    everything = registry.create_profiled_providers(tmp_path, advertise_all=True)
    assert {(p.name, type(p).__name__) for p in everything} >= {("UAD Mono Only", "StereoStub"), ("UAD Not Yet Live", "Stub")}


def test_a_native_transaction_runs_off_the_event_loop(monkeypatch):
    """The load runs on the daemon's worker thread: gets are served meanwhile, the slot reads
    Loading, and a set waits (in order) until the load has committed."""
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


def test_known_good_list_holds_no_dsp_data():
    for entry in registry.known_good().values():
        assert set(entry) <= {"name", "stage", "family", "layouts", "latency_samples", "dynamic_refs"}
        assert set(entry["layouts"]) <= {"mono", "stereo"}
        assert all(len(ref) == 2 for ref in entry.get("dynamic_refs", ()))
