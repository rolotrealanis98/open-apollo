"""plugin_report: what the opt-in install report says about plug-ins.

Only render ids of plug-ins the device says are authorized, and which of
those built here for native hosting: no other licence state, no demo days,
no names, no paths, nothing the caller adds.
"""

import errno
import json
import logging

import plugin_license as pl
import plugin_report as pr


def table(**states):
    """768 words: every plug-in demo-not-started, except render id -> word."""
    words = [0x80000000] * pl.BOARD_WORDS
    for rid, word in states.items():
        words[int(rid[1:]) - pl.BOARD_BASE] = word
    return words


WORDS = table(r2276=0x81000000,      # authorized
              r2100=0x82000000,      # authorized (the other word)
              r2200=(13 << 12) | 5,  # a running demo: 13 days
              r2300=0x00000000,      # demo expired
              r2400=0x83000000,      # unavailable
              r2500=0x81000000)      # authorized, not on this machine's list

MANIFEST = {"format": pl.MANIFEST_FORMAT, "plugins": [
    {"name": "UAD Neve 1084", "plugin_id": "n1084", "render_id": 2776,
     "ua_xml": [{"renderid": 2276}]},                 # licensed under the registry id
    {"name": "UAD Pultec EQP-1A", "plugin_id": "pultec", "render_id": 2100},
    {"name": "UAD Demo Thing", "plugin_id": "demo", "render_id": 2200},
    {"name": "UAD Twice", "plugin_id": "t1", "render_id": 2500},
    {"name": "UAD Twice", "plugin_id": "t2", "render_id": 2501},   # ambiguous name
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


def test_authorized_ids_are_the_authorized_words_only():
    assert pr.authorized_ids(WORDS) == [2100, 2276, 2500]


def test_report_shape_and_native_status():
    doc = pr.build(WORDS, MANIFEST, INDEX)
    assert doc == {
        "schema": pr.SCHEMA,
        "available": True,
        "authorized": [2100, 2276, 2500],
        "native": {"built_mono": [2276], "built_stereo": [2276], "failed": [2100]},
    }


def test_nothing_identifying_is_in_the_report():
    text = json.dumps(pr.build(WORDS, MANIFEST, INDEX))
    for secret in ("yourname", "UAD", "Neve", "timeout", "home", "Demo", "13", "x.json"):
        assert secret not in text


def test_no_manifest_or_index_still_reports_the_authorized_ids():
    assert pr.build(WORDS) == {"schema": pr.SCHEMA, "available": True,
                               "authorized": [2100, 2276, 2500],
                               "native": {"built_mono": [], "built_stereo": [], "failed": []}}


def test_a_wrong_table_is_no_answer():
    assert pr.build([0x81000000] * 10) == {"schema": pr.SCHEMA, "available": False,
                                           "reason": "bad_answer"}


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
    assert pr.sanitize({"available": False, "reason": "plugin_host_off", "x": 1}) == {
        "schema": pr.SCHEMA, "available": False, "reason": "plugin_host_off"}
    assert pr.sanitize({"available": False, "reason": "/home/yourname: EIO"})["reason"] == "query_failed"
    assert pr.sanitize(None) == pr.unavailable("query_failed")
    assert pr.sanitize({"available": "yes", "authorized": [2276]}) == pr.unavailable("query_failed")


def test_native_ids_must_be_authorized():
    doc = pr.sanitize({"available": True, "authorized": [2276],
                       "native": {"built_mono": [2100], "failed": [2276]}})
    assert doc["native"] == {"built_mono": [], "built_stereo": [], "failed": [2276]}


class FakeBackend:
    def __init__(self, opened=True, host=True, errors=()):
        self.opened, self.host, self.errors = opened, host, list(errors)
        self.queries = 0
        self.closed = False

    def open(self):
        return self.opened

    def has_feature(self, bit):
        return self.host

    def features2(self):
        return 1 if self.host else 0

    def board_query(self):
        self.queries += 1
        if self.errors:
            raise OSError(self.errors.pop(0), "x")
        return 0, 1, tuple(WORDS)

    def close(self):
        self.closed = True


def run_query(monkeypatch, be):
    import hardware
    monkeypatch.setattr(hardware, "HardwareBackend", lambda device=None: be)
    monkeypatch.setattr(pr.time, "sleep", lambda s: None)
    return pr.query_device()


def test_query_device_reasons(monkeypatch):
    assert run_query(monkeypatch, FakeBackend(opened=False)) == (None, "no_device")
    be = FakeBackend(host=False)
    assert run_query(monkeypatch, be) == (None, "plugin_host_off")
    assert be.queries == 0 and be.closed
    be = FakeBackend(errors=[errno.EBUSY, errno.ETIMEDOUT])
    words, reason = run_query(monkeypatch, be)
    assert reason is None and list(words) == WORDS and be.queries == 3
    be = FakeBackend(errors=[errno.EIO])
    assert run_query(monkeypatch, be) == (None, "query_failed")
    be = FakeBackend(errors=[errno.EOPNOTSUPP])
    assert run_query(monkeypatch, be) == (None, "plugin_host_off")


def test_main_prints_only_the_report(monkeypatch, tmp_path, capsys):
    (tmp_path / "manifest.json").write_text(json.dumps(MANIFEST))
    (tmp_path / "b").mkdir()
    (tmp_path / "b" / "index.json").write_text(json.dumps(INDEX))
    monkeypatch.setattr(pr, "query_device", lambda device=None: (tuple(WORDS), None))
    assert pr.main(["--manifest", str(tmp_path / "manifest.json"),
                    "--bundles", str(tmp_path / "b")]) == 0
    assert json.loads(capsys.readouterr().out) == pr.build(WORDS, MANIFEST, INDEX)
    logging.disable(logging.NOTSET)        # main() silences logging for its own process
