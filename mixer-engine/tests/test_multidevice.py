"""Two Apollos: per-device ports, state file and ALSA card lookup."""
import os
from pathlib import Path

import metering
import ua_mixer_daemon as d


def test_device_index():
    assert d.device_index("/dev/ua_apollo0") == 0
    assert d.device_index("/dev/ua_apollo3") == 3
    assert d.device_index(None) == 0
    assert d.device_index("/dev/something") == 0


def test_state_file_names():
    m = Path("device_map_apollo_x4.json")
    assert d.state_file_for(m, 0, 0x1234).name == "device_map_apollo_x4.state.json"
    a = d.state_file_for(m, 1, 0xAB)
    b = d.state_file_for(m, 2, 0xAB)
    assert a.name == "device_map_apollo_x4.dev1-000000ab.state.json"
    assert a != b
    assert d.state_file_for(m, 1, None).name == "device_map_apollo_x4.dev1.state.json"


def _fake_sys(tmp_path, monkeypatch, pcis):
    """pcis: PCI names; card (5+i) and /sys/class/ua_apollo/ua_apollo<i> share pcis[i]."""
    sysroot = tmp_path / "sys"
    drv = sysroot / "drv" / "ua_apollo"
    drv.mkdir(parents=True)
    for i, pci in enumerate(pcis):
        pdev = sysroot / "pci" / pci
        pdev.mkdir(parents=True)
        (pdev / "driver").symlink_to(drv)
        c = sysroot / "class_sound" / f"card{5 + i}"
        c.mkdir(parents=True)
        (c / "device").symlink_to(pdev)
        cd = sysroot / "class_ua" / f"ua_apollo{i}"
        cd.mkdir(parents=True)
        (cd / "device").symlink_to(pdev)
    real_glob, real_realpath = metering.glob.glob, os.path.realpath

    def fake_glob(pat):
        return real_glob(pat.replace("/sys/class/sound",
                                     str(sysroot / "class_sound")))

    def fake_realpath(p, *a, **k):
        return real_realpath(str(p).replace("/sys/class/ua_apollo",
                                            str(sysroot / "class_ua")), *a, **k)

    monkeypatch.setattr(metering.glob, "glob", fake_glob)
    monkeypatch.setattr(metering.os.path, "realpath", fake_realpath)


def test_card_follows_device(tmp_path, monkeypatch):
    _fake_sys(tmp_path, monkeypatch, ["0000:03:00.0", "0000:04:00.0"])
    assert metering.find_apollo_card() == 5
    assert metering.find_apollo_card("/dev/ua_apollo0") == 5
    assert metering.find_apollo_card("/dev/ua_apollo1") == 6
    assert metering.find_apollo_card("/dev/ua_apollo7") is None
