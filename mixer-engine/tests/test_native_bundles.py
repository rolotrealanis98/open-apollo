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
    for name, channels in entries:
        file = name.replace(" ", "-") + "-runtime-bundle.json"
        (tmp_path / file).write_text(json.dumps({"status": "pass", "name": name, "carrier_channels": channels}))
        index["bundles"].append({"name": name, "file": file})
    (tmp_path / "index.json").write_text(json.dumps(index))


def test_registry_offers_known_good_on_every_input(tmp_path, monkeypatch):
    monkeypatch.setattr(registry, "ProfiledCarrierProvider", Stub)
    monkeypatch.setattr(registry, "StereoProfiledCarrierProvider", StereoStub)
    monkeypatch.delenv("OA_UAD_SDK", raising=False)
    monkeypatch.delenv("OA_MODULE_STORE", raising=False)
    known = next(iter(registry.known_good()))
    bundles(tmp_path, (known, 2), ("UAD Not Yet Live", 1))
    offered = registry.create_profiled_providers(tmp_path)
    assert sorted((type(p).__name__, p.input_index) for p in offered) == [
        ("StereoStub", 0), ("StereoStub", 2), ("Stub", 0), ("Stub", 1), ("Stub", 2), ("Stub", 3)]
    assert {p.name for p in offered} == {known}
    assert callable(offered.composer)
    everything = registry.create_profiled_providers(tmp_path, advertise_all=True)
    assert {p.name for p in everything} == {known, "UAD Not Yet Live"}


def test_known_good_list_holds_no_dsp_data():
    for entry in registry.known_good().values():
        assert set(entry) <= {"name", "stage", "family", "channels", "latency_samples", "dynamic_refs"}
        assert all(len(ref) == 2 for ref in entry.get("dynamic_refs", ()))
