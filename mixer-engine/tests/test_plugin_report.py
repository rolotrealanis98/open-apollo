"""plugin_report: what the opt-in install report says about plug-ins.

Only render ids of plug-ins the device says are authorized, and which of
those built here for native hosting: no other licence state, no demo days,
no names, no paths, nothing the caller adds.
"""

import json
import logging

import plugin_authorization as pa
import plugin_license as pl
import plugin_report as pr

# plugin_authorization's answer: {name: state}
STATES = {"UAD Neve 1084": pa.AUTHORIZED,       # licensed under the registry id 2276
          "UAD Pultec EQP-1A": pa.AUTHORIZED,
          "UAD Demo Thing": pa.DEMO,             # a demo: not reported
          "UAD Expired": pa.UNAUTHORIZED,
          "UAD Twice": pa.AUTHORIZED,            # ambiguous render id: left out
          "UAD Not In Manifest": pa.AUTHORIZED,  # no render id: left out
          "UAD Unknown": pa.UNKNOWN}

MANIFEST = {"format": pl.MANIFEST_FORMAT, "plugins": [
    {"name": "UAD Neve 1084", "plugin_id": "n1084", "render_id": 2776,
     "ua_xml": [{"renderid": 2276}]},
    {"name": "UAD Pultec EQP-1A", "plugin_id": "pultec", "render_id": 2100},
    {"name": "UAD Demo Thing", "plugin_id": "demo", "render_id": 2200},
    {"name": "UAD Expired", "plugin_id": "exp", "render_id": 2300},
    {"name": "UAD Twice", "plugin_id": "t1", "render_id": 2500},
    {"name": "UAD Twice", "plugin_id": "t2", "render_id": 2501},
]}
INDEX = {"schema": "open-apollo-plugin-bundles/1",
         "uad_sdk": "/home/yourname/win/Windows/System32/UAD2SDK.dll",
         "bundles": [
             {"name": "UAD Neve 1084", "layout": "mono", "file": "x.json"},
             {"name": "UAD Neve 1084", "layout": "stereo", "file": "y.json"},
             {"name": "UAD Demo Thing", "layout": "mono", "file": "z.json"},   # not authorized
         ],
         "failures": {"UAD Pultec EQP-1A (stereo)": "execution timeout: /home/yourname/x",
                      "UAD Neve 1084 (stereo)": "execution crash",            # built anyway
                      "UAD Demo Thing": "not installed"}}


def test_only_authorized_names_with_a_render_id():
    assert pr.authorized_ids(STATES, pr.licence_ids_by_name(MANIFEST)) == [2100, 2276]


def test_report_shape_and_native_status():
    doc = pr.build(STATES, MANIFEST, INDEX)
    assert doc == {
        "schema": pr.SCHEMA,
        "available": True,
        "authorized": [2100, 2276],
        "native": {"built_mono": [2276], "built_stereo": [2276], "failed": [2100]},
    }


def test_nothing_identifying_is_in_the_report():
    text = json.dumps(pr.build(STATES, MANIFEST, INDEX))
    for secret in ("yourname", "UAD", "Neve", "timeout", "home", "Demo", "2200", "x.json"):
        assert secret not in text


def test_without_a_manifest_no_id_is_known():
    assert pr.build(STATES) == {"schema": pr.SCHEMA, "available": True, "authorized": [],
                                "native": {"built_mono": [], "built_stereo": [], "failed": []}}


def test_no_answer():
    assert pr.build({}) == {"schema": pr.SCHEMA, "available": False, "reason": "no_answer"}


def test_sanitize_drops_everything_else():
    dirty = {"schema": "x", "available": True, "serial": "2000000123",
             "authorized": [2276, "2277", 99, 0xB00, True, 2276, 2100.0, 2400],
             "native": {"built_mono": [2276, 2400, 3000], "built_stereo": "all",
                        "failed": [2400], "names": ["Neve"]},
             "demo_days": {"2200": 13}, "account": "a@b.c"}
    assert pr.sanitize(dirty) == {
        "schema": pr.SCHEMA, "available": True, "authorized": [2276, 2400],
        "native": {"built_mono": [2276, 2400], "built_stereo": [], "failed": [2400]},
    }


def test_sanitize_unavailable_keeps_only_a_known_reason():
    assert pr.sanitize({"available": False, "reason": "no_device", "x": 1}) == {
        "schema": pr.SCHEMA, "available": False, "reason": "no_device"}
    assert pr.sanitize({"available": False, "reason": "/home/yourname: EIO"})["reason"] == "no_answer"
    assert pr.sanitize(None) == pr.unavailable("no_answer")
    assert pr.sanitize({"available": "yes", "authorized": [2276]}) == pr.unavailable("no_answer")


def test_native_ids_must_be_authorized():
    doc = pr.sanitize({"available": True, "authorized": [2276],
                       "native": {"built_mono": [2100], "failed": [2276]}})
    assert doc["native"] == {"built_mono": [], "built_stereo": [], "failed": [2276]}


def test_main_prints_only_the_report(monkeypatch, tmp_path, capsys):
    (tmp_path / "manifest.json").write_text(json.dumps(MANIFEST))
    (tmp_path / "b").mkdir()
    (tmp_path / "b" / "index.json").write_text(json.dumps(INDEX))
    asked = {}

    def answer(**kw):
        asked.update(kw)
        return {"source": "daemon", "plugins": STATES}
    monkeypatch.setattr(pa, "authorized_plugins", answer)
    assert pr.main(["--manifest", str(tmp_path / "manifest.json"),
                    "--bundles", str(tmp_path / "b")]) == 0
    assert json.loads(capsys.readouterr().out) == pr.build(STATES, MANIFEST, INDEX)
    assert asked["manifest"] == tmp_path / "manifest.json"
    logging.disable(logging.NOTSET)        # main() silences logging for its own process
