"""Post-fader and send meters the daemon derives from the input meters."""

import pytest

from metering import SILENCE_DB, ReadbackMeter
from ua_mixer_daemon import MixerDaemon

IN0 = "/devices/0/inputs/0"
LEVEL = IN0 + "/meters/0/MeterLevel/value"


@pytest.fixture
def meter():
    m = ReadbackMeter()
    words = [0] * 40
    words[8] = 0x8000 << 16          # analog 1 at -6.0 dB
    m.feed(words)
    return m


@pytest.fixture
def daemon(router, tree, meter):
    return MixerDaemon(tree, router, metering=meter)


def read(daemon, path):
    return daemon._read_meter(daemon._parse_meter_path(path))


def send_level(send):
    return f"{IN0}/sends/{send}/meters/0/MeterLevel/value"


def test_input_meter_is_pre_fader_by_default(daemon, tree):
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, LEVEL) == pytest.approx(-6.0, abs=0.05)


def test_post_fader_metering_adds_the_fader(daemon, tree):
    tree.set_value("/PostFaderMetering/value", True)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, LEVEL) == pytest.approx(-16.0, abs=0.05)


def test_post_fader_metering_mute_is_silent(daemon, tree):
    tree.set_value("/PostFaderMetering/value", True)
    tree.set_value(IN0 + "/Mute/value", True)
    assert read(daemon, LEVEL) == SILENCE_DB


def test_post_fader_tops_out_at_zero_and_clips(daemon, tree):
    tree.set_value("/PostFaderMetering/value", True)
    tree.set_value(IN0 + "/FaderLevel/value", 12.0)
    assert read(daemon, LEVEL) == 0.0
    assert read(daemon, IN0 + "/meters/0/MeterClip/value") is True


def test_pre_fader_aux_send_is_input_plus_send_gain(daemon, tree):
    tree.set_value("/devices/0/auxs/0/SendPostFader/value", False)
    tree.set_value(IN0 + "/sends/0/Gain/value", -6.0)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, send_level(0)) == pytest.approx(-12.0, abs=0.05)


def test_post_fader_aux_send_adds_the_fader(daemon, tree):
    tree.set_value("/devices/0/auxs/0/SendPostFader/value", True)
    tree.set_value(IN0 + "/sends/0/Gain/value", -6.0)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, send_level(0)) == pytest.approx(-22.0, abs=0.05)


def test_send_override_beats_the_aux_setting(daemon, tree):
    tree.set_value("/devices/0/auxs/0/SendPostFader/value", True)
    tree.set_value(IN0 + "/sends/0/PostFaderOverride/value", 0)
    tree.set_value(IN0 + "/sends/0/Gain/value", -6.0)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, send_level(0)) == pytest.approx(-12.0, abs=0.05)


def test_aux_sends_are_post_fader_by_default(daemon, tree):
    # The x4 device map's AUX buses default to SendPostFader
    tree.set_value(IN0 + "/sends/0/Gain/value", -6.0)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, send_level(0)) == pytest.approx(-22.0, abs=0.05)


def test_post_fader_cue_send(daemon, tree):
    tree.set_value("/devices/0/HP1SendPostFader/value", True)
    tree.set_value(IN0 + "/sends/2/Gain/value", -3.0)
    tree.set_value(IN0 + "/FaderLevel/value", -10.0)
    assert read(daemon, send_level(2)) == pytest.approx(-19.0, abs=0.05)


def test_cue_send_ignores_fader_and_mute(daemon, tree):
    tree.set_value(IN0 + "/sends/2/Gain/value", -3.0)
    tree.set_value(IN0 + "/FaderLevel/value", -144.0)
    tree.set_value(IN0 + "/Mute/value", True)
    assert read(daemon, send_level(2)) == pytest.approx(-9.0, abs=0.05)


@pytest.mark.parametrize("path, value", [
    ("/sends/1/Bypass/value", True),
    ("/sends/1/Gain/value", -144.0),
])
def test_bypassed_or_off_send_is_silent(daemon, tree, path, value):
    tree.set_value(IN0 + "/sends/1/Gain/value", 0.0)
    tree.set_value(IN0 + path, value)
    assert read(daemon, send_level(1)) == SILENCE_DB


def test_silent_input_stays_silent(daemon, tree, meter):
    meter.feed([0] * 40)
    tree.set_value(IN0 + "/sends/0/Gain/value", 12.0)
    assert read(daemon, send_level(0)) == SILENCE_DB


def test_parse_keeps_the_send_index(daemon):
    entry = daemon._parse_meter_path(send_level(3))
    assert (entry["kind"], entry["channel"], entry["send"],
            entry["meter_idx"], entry["prop"]) == ("inputs", 0, 3, 0, "level")
    assert daemon._parse_meter_path(LEVEL)["send"] is None
