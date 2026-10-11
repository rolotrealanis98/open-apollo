"""Converter latency and delay compensation from devices/*.json (A10/D5):
model_features.latency_figures against the Mixer Engine's own arithmetic,
and the daemon publishing it / sending the ADC delay comp (other 0x6a)."""

import json

import pytest

import model_features as mf
from model_features import latency_figures, load_descriptor, table_row

X4 = load_descriptor("apollo-x4")


class TestTables:
    def test_every_model_has_latency(self):
        for path in sorted(mf.DEVICES_DIR.glob("*.json")):
            d = load_descriptor(path.stem)
            for rate in mf.RATES:
                assert latency_figures(d["latency"], rate) is not None, \
                    path.stem

    @pytest.mark.parametrize("board, want", [
        (0, [5, 5, 5, 5, 6, 6]), (None, [5, 5, 5, 5, 6, 6]),
        (-1, [5, 5, 5, 5, 6, 6]), (7, [5, 5, 5, 5, 6, 6]),
        (2, [4.8069, 4.7957, 4.6805, 4.6581, 4.4276, 4.3829])])
    def test_x4_input_by_board(self, board, want):
        # MixerTypeFreud::GetInputDelay: board 2 uses the fractional table
        assert table_row(X4["latency"]["input"], board) == want

    def test_grouped_board_keys(self):
        arrow = load_descriptor("arrow")["latency"]["input"]
        for b in range(1, 8):
            assert table_row(arrow, b) == [12, 12, 9, 9, 5, 5]
        assert table_row(arrow, 0) == [5, 5, 5, 5, 6, 6]

    def test_unknown_rate(self):
        assert latency_figures(X4["latency"], 32000) is None
        assert latency_figures(None, 48000) is None


class TestFigures:
    def test_x4_48k_board0(self):
        f = latency_figures(X4["latency"], 48000, 0)
        assert f["adc"] == 5 and f["dac"] == pytest.approx(43.3)
        assert f["adc_comp"] == 5
        # the line outs are 4 frames faster than the monitor outs; the cue
        # outs are slower and never delayed
        assert f["dac_comp"] == {"line": 4, "cue": 0, "monitor": 0}
        # SetMaximumDelayCompInDevice: 5 + 64 + 8 in, 43.3 + 4 out
        assert (f["additional_input"], f["additional_output"]) == (77, 47)

    def test_max_delay_comp_goes_to_the_input_side(self):
        f = latency_figures(X4["latency"], 48000, 0,
                            mf.MAX_DELAY_COMP_SAMPLES["Medium"])
        assert (f["additional_input"], f["additional_output"]) == (277, 47)

    def test_hi_band_dsp_block(self):
        f = latency_figures(X4["latency"], 192000, 0)
        assert f["adc_comp"] == 6
        assert f["additional_input"] == int(6 + 128 + 8 + 0.5)

    def test_board2_rounds_like_the_engine(self):
        # (int)(4.3829 + 0.5) = 4
        assert latency_figures(X4["latency"], 192000, 2)["adc_comp"] == 4
        assert latency_figures(X4["latency"], 48000, 2)["adc_comp"] == 5

    def test_twin_cue_outputs_are_not_delayed(self):
        twin = load_descriptor("apollo-twin-mkii")["latency"]
        assert latency_figures(twin, 48000, 0)["dac_comp"]["cue"] == 1
        assert latency_figures(twin, 48000, 0, lines34_hp=True)[
            "dac_comp"]["cue"] == 0

    def test_delay_comp_names(self):
        assert mf.MAX_DELAY_COMP_SAMPLES == {
            "Off": 0, "Short": 100, "Medium": 200, "Medium-Long": 300,
            "Long": 1000}


@pytest.fixture
def daemon(router, tree):
    from ua_mixer_daemon import MixerDaemon, apply_runtime_properties
    apply_runtime_properties(tree)
    d = MixerDaemon(tree, router)
    d.model_data = X4
    d._hw_init_done = True
    return d


def other(backend, param):
    return [c[3] for c in backend.mixer_params if c[0] == 2 and c[2] == param]


class TestDaemon:
    def test_publishes_and_sends_adc_comp_once(self, daemon, backend, tree):
        tree.set("/SampleRate/value", 48000)
        assert daemon._refresh_latency()
        assert tree.get_value("/openapollo/latency/input_frames/value") == 5
        assert tree.get_value(
            "/openapollo/latency/output_frames/value") == pytest.approx(43.3)
        assert tree.get_value(
            "/openapollo/latency/additional_input/value") == 277  # Medium
        assert json.loads(tree.get_value(
            "/openapollo/latency/output_delay_comp/value"))["line"] == 4
        assert other(backend, 0x6a) == [5]
        assert not daemon._refresh_latency()      # nothing changed
        assert other(backend, 0x6a) == [5]

    def test_rate_change_resends(self, daemon, backend, tree):
        tree.set("/SampleRate/value", 48000)
        daemon._refresh_latency()
        tree.set("/SampleRate/value", 192000)
        assert daemon._refresh_latency()
        assert other(backend, 0x6a) == [5, 6]

    def test_board_and_delay_comp_follow(self, daemon, backend, tree):
        tree.set("/SampleRate/value", 192000)
        daemon._refresh_latency()
        tree.set("/devices/0/AnalogBoardID/value", 2)
        assert daemon._refresh_latency()
        assert other(backend, 0x6a) == [6, 4]
        tree.set("/MaxDelayComp/value", "Off")
        assert daemon._refresh_latency()
        assert tree.get_value(
            "/openapollo/latency/additional_input/value") == int(
                4.3829 + 128 + 8 + 0.5)
        assert other(backend, 0x6a) == [6, 4]     # unchanged: not resent

    def test_reinit_resends(self, daemon, backend, tree):
        # the init sequence writes its own 0x6a; a re-init (hot-plug,
        # resume) gets the model's value again
        tree.set("/SampleRate/value", 192000)
        daemon._refresh_latency()
        daemon._readback_start_time += 10.0
        assert not daemon._refresh_latency()      # figures unchanged
        assert other(backend, 0x6a) == [6, 6]

    def test_nothing_before_init(self, daemon, backend, tree):
        daemon._hw_init_done = False
        tree.set("/SampleRate/value", 48000)
        assert daemon._refresh_latency()
        assert other(backend, 0x6a) == []

    def test_no_model_no_work(self, daemon, backend):
        daemon.model_data = {}
        assert not daemon._refresh_latency()
        assert backend.mixer_params == []
