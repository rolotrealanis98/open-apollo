"""HardwareRouter.poll_hw_readback decoding and the readback-driven meters."""

import pytest

import metering
from conftest import make_readback
from metering import SILENCE_DB, ReadbackMeter, meter_channel


def poll(router, backend, words, status=1):
    backend.readback = (status, words)
    return router.poll_hw_readback()


class TestAvailability:
    def test_none_without_readback(self, router, backend):
        backend.readback = None
        assert router.poll_hw_readback() is None

    def test_none_when_status_not_ready(self, router, backend):
        assert poll(router, backend, make_readback(), status=0) is None


class TestMonitorSection:
    def test_level(self, router, backend):
        r = poll(router, backend, make_readback(mon_raw=172))
        assert r["monitor_level_raw"] == 172
        assert r["monitor_level_db"] == -10.0

    def test_level_extremes(self, router, backend):
        assert poll(router, backend, make_readback(mon_raw=0))["monitor_level_db"] == -96.0
        assert poll(router, backend, make_readback(mon_raw=192))["monitor_level_db"] == 0.0

    def test_tapered_is_monotonic(self, router, backend):
        lo = poll(router, backend, make_readback(mon_raw=100))["monitor_level_tapered"]
        hi = poll(router, backend, make_readback(mon_raw=180))["monitor_level_tapered"]
        assert 0.0 <= lo < hi <= 1.0

    def test_flags_off(self, router, backend):
        r = poll(router, backend, make_readback(mon_raw=100))
        assert (r["monitor_mute"], r["monitor_mono"], r["monitor_dim"]) == (False,) * 3

    @pytest.mark.parametrize("kw, key", [
        ({"mute": True}, "monitor_mute"),
        ({"mono": True}, "monitor_mono"),
        ({"dim": True}, "monitor_dim"),
    ])
    def test_each_flag_alone(self, router, backend, kw, key):
        r = poll(router, backend, make_readback(mon_raw=100, **kw))
        flags = {k: r[k] for k in ("monitor_mute", "monitor_mono", "monitor_dim")}
        assert flags == {k: k == key for k in flags}

    def test_flags_do_not_disturb_level(self, router, backend):
        r = poll(router, backend, make_readback(
            mon_raw=150, hp1_raw=120, mute=True, mono=True, dim=True))
        assert r["monitor_level_raw"] == 150
        assert r["hp1_level_raw"] == 120

    def test_hp2(self, router, backend):
        r = poll(router, backend, make_readback(hp2_raw=140))
        assert r["hp2_level_raw"] == 140       # the 0xa0 top byte is masked off


class TestTalkbackAndClock:
    def test_talkback(self, router, backend):
        assert poll(router, backend, make_readback(talkback=True))["talkback_active"]
        assert not poll(router, backend, make_readback())["talkback_active"]

    def test_clock_lock_absent_when_not_valid(self, router, backend):
        assert "clock_locked" not in poll(router, backend, make_readback())

    @pytest.mark.parametrize("lock", [True, False])
    def test_clock_lock_when_valid(self, router, backend, lock):
        r = poll(router, backend, make_readback(clock_lock=lock))
        assert r["clock_locked"] is lock

    def test_lock_bit_without_valid_bit_is_ignored(self, router, backend):
        w = make_readback()
        w[6] |= 1 << 31
        assert "clock_locked" not in poll(router, backend, w)


class TestPreampFlags:
    def test_all_clear(self, router, backend):
        r = poll(router, backend, make_readback())
        for ch in range(4):
            for k in ("micline", "pad", "link", "48v", "lowcut", "phase"):
                assert r[f"preamp_{ch}_{k}"] is False

    def test_six_bit_stride(self, router, backend):
        flags = {0: {"48v"}, 1: {"pad", "phase"}, 2: {"micline", "lowcut", "link"},
                 3: {"48v", "pad", "lowcut", "phase", "micline", "link"}}
        r = poll(router, backend, make_readback(flags=flags))
        allk = ("micline", "pad", "link", "48v", "lowcut", "phase")
        for ch in range(4):
            for k in allk:
                assert r[f"preamp_{ch}_{k}"] is (k in flags[ch]), (ch, k)

    def test_hiz_bits(self, router, backend):
        r = poll(router, backend, make_readback(hiz=(True, False)))
        assert (r["preamp_0_hiz"], r["preamp_1_hiz"]) == (True, False)
        r = poll(router, backend, make_readback(hiz=(False, True)))
        assert (r["preamp_0_hiz"], r["preamp_1_hiz"]) == (False, True)

    def test_hiz_bits_do_not_leak_into_ch3_flags(self, router, backend):
        # bits 24/25 are the first two bits above ch3's 6-bit field (18-23)
        r = poll(router, backend, make_readback(hiz=(True, True)))
        assert not any(r[f"preamp_3_{k}"] for k in
                       ("micline", "pad", "link", "48v", "lowcut", "phase"))


class TestPreampGain:
    def test_gains(self, router, backend):
        r = poll(router, backend, make_readback(gains_c=[1, 21, 41, 56]))
        assert [r[f"preamp_{ch}_gain"] for ch in range(4)] == [10, 30, 50, 65]
        assert [r[f"preamp_{ch}_gain_raw"] for ch in range(4)] == [1, 21, 41, 56]

    def test_fields_are_independent(self, router, backend):
        # 6-bit fields at shifts 0/6/16/22: one at a time
        for hot in range(4):
            g = [0, 0, 0, 0]
            g[hot] = 0x2A
            r = poll(router, backend, make_readback(gains_c=g))
            assert [r[f"preamp_{c}_gain_raw"] for c in range(4)] == g

    def test_mixer_engine_range_rule(self, router, backend):
        # applyInputGainSettings: GainC 0 reads as 0 dB, 0x39 and up as 65
        r = poll(router, backend, make_readback(gains_c=[0, 63, 0x38, 0x39]))
        assert [r[f"preamp_{c}_gain"] for c in range(4)] == [0, 65, 65, 65]

    def test_ch1_field_is_six_bits(self, router, backend):
        # bit 12 lies above ch1's field [11:6]
        w = make_readback(gains_c=[0, 21, 0, 0])
        w[3] |= 1 << 12
        assert poll(router, backend, w)["preamp_1_gain_raw"] == 21

    def test_selection_indicator_bits_do_not_change_gain(self, router, backend):
        w = make_readback(gains_c=[21, 21, 21, 21])
        w[3] |= 0xF0 << 24 | 0xE0 << 8     # b3[7:4] indicator, b1[7:5] constant
        r = poll(router, backend, w)
        assert [r[f"preamp_{c}_gain"] for c in range(4)] == [30] * 4

    def test_gain_round_trips_the_write_path(self, router, backend):
        # What the router writes as GainC (dB-9) reads back as the same dB
        for db in (10, 25, 40, 55, 64):
            router.on_set("/devices/0/inputs/0/preamps/0/Gain/value", float(db))
            gain_c = backend.mixer_params[-1][3]
            backend.clear()
            r = poll(router, backend, make_readback(gains_c=[gain_c, 0, 0, 0]))
            assert r["preamp_0_gain"] == db


class TestCheckWords:
    @pytest.mark.parametrize("idx", [38, 39])
    def test_snapshot_with_allones_check_word_is_dropped(self, router, backend, idx):
        w = make_readback(mon_raw=100)
        w[idx] = 0xFFFFFFFF
        assert poll(router, backend, w) is None
        assert router.readback_skipped is True

    def test_valid_snapshot_clears_the_flag(self, router, backend):
        w = make_readback()
        w[38] = 0xFFFFFFFF
        poll(router, backend, w)
        assert poll(router, backend, make_readback()) is not None
        assert router.readback_skipped is False

    def test_not_ready_is_not_a_skip(self, router, backend):
        assert poll(router, backend, make_readback(), status=0) is None
        assert router.readback_skipped is False


class TestFrontPanelAndStatus:
    def test_defaults(self, router, backend):
        r = poll(router, backend, make_readback())
        assert (r["hp_focus_valid"], r["hp_focus_headphone"],
                r["front_panel_input"], r["altmon_selection"],
                r["low_noise_preamps"], r["temperature_raw"],
                r["analog_board_id"]) == (False, False, 0, 0, False, 0, 0)

    @pytest.mark.parametrize("focus, valid, hp", [
        ("monitor", True, False), ("headphone", True, True)])
    def test_focus(self, router, backend, focus, valid, hp):
        r = poll(router, backend, make_readback(focus=focus))
        assert (r["hp_focus_valid"], r["hp_focus_headphone"]) == (valid, hp)

    def test_focus_bits_do_not_touch_hiz_or_flags(self, router, backend):
        r = poll(router, backend, make_readback(focus="headphone"))
        assert not r["preamp_0_hiz"] and not r["preamp_1_hiz"]
        assert not any(r[f"preamp_3_{k}"] for k in
                       ("micline", "pad", "link", "48v", "lowcut", "phase"))

    @pytest.mark.parametrize("sel", range(8))
    def test_selected_input_does_not_change_gains(self, router, backend, sel):
        r = poll(router, backend, make_readback(gains_c=[56, 56, 56, 56],
                                                sel_input=sel))
        assert r["front_panel_input"] == sel
        assert [r[f"preamp_{c}_gain"] for c in range(4)] == [65] * 4

    def test_altmon_temperature_board_low_noise(self, router, backend):
        r = poll(router, backend, make_readback(
            altmon=2, temperature=0x2D, board_id=5, low_noise=True,
            talkback=True, clock_lock=True))
        assert (r["altmon_selection"], r["temperature_raw"],
                r["analog_board_id"], r["low_noise_preamps"]) == (2, 0x2D, 5, True)
        assert r["talkback_active"] and r["clock_locked"]

    def test_low_noise_does_not_touch_dim(self, router, backend):
        r = poll(router, backend, make_readback(low_noise=True))
        assert r["monitor_dim"] is False


class TestEightPreamps:
    @pytest.fixture
    def x8p(self):
        from conftest import ENGINE_DIR, FakeBackend
        from hardware import HardwareRouter
        from state_tree import StateTree
        t = StateTree()
        t.load_device_map(ENGINE_DIR / "device_maps" / "device_map_apollo_x8p.json")
        be = FakeBackend()
        return HardwareRouter(be, t), be

    def test_x4_decodes_four(self, router, backend):
        assert "preamp_4_gain" not in poll(router, backend, make_readback())

    def test_preamps_5_to_8_from_words_5_and_7(self, x8p):
        router, backend = x8p
        w = make_readback(flags={4: {"48v"}, 7: {"pad", "phase"}})
        w[7] = 1 | 21 << 6 | 41 << 16 | 56 << 22
        r = poll(router, backend, w)
        assert [r[f"preamp_{c}_gain"] for c in range(4, 8)] == [10, 30, 50, 65]
        assert r["preamp_4_48v"] and not r["preamp_4_pad"]
        assert r["preamp_7_pad"] and r["preamp_7_phase"]
        assert not r["preamp_0_48v"]          # word 0 untouched


class TestRawWords:
    def test_words_returned_for_meters(self, router, backend):
        w = make_readback()
        w[8] = 0xDEADBEEF
        r = poll(router, backend, w)
        assert r["words"] == w and r["words"] is not w


class TestReadbackMeter:
    @staticmethod
    def feed_one(word, shift, value, chan_id):
        m = ReadbackMeter()
        w = make_readback()
        w[word] = value << shift
        m.feed(w)
        return m.get_channel_meter(m.channel_ids.index(chan_id))

    def test_16bit_half_scale(self):
        level, _peak, clip = self.feed_one(8, 16, 0x8000, 0x0001)
        assert level == pytest.approx(-6.0, abs=0.05)
        assert clip is False

    def test_16bit_full_scale_clips(self):
        level, _peak, clip = self.feed_one(8, 16, 0xFFFF, 0x0001)
        assert level == pytest.approx(0.0, abs=0.01)
        assert clip is True

    def test_16bit_low_half_of_word(self):
        level, _p, _c = self.feed_one(8, 0, 0x8000, 0x0002)
        assert level == pytest.approx(-6.0, abs=0.05)

    def test_12bit_bus_meter(self):
        level, _p, clip = self.feed_one(18, 16, 0x028, 0x0901)    # monitor L
        assert level == pytest.approx(-40.2, abs=0.05)
        assert clip is False

    def test_12bit_full_scale_clips(self):
        _l, _p, clip = self.feed_one(18, 0, 0xFFF, 0x0902)
        assert clip is True

    def test_below_floor_is_silence(self):
        level, _p, clip = self.feed_one(8, 16, 1, 0x0001)
        assert level == SILENCE_DB and clip is False

    def test_untouched_channels_stay_silent(self):
        m = ReadbackMeter()
        w = make_readback()
        w[8] = 0x8000 << 16
        m.feed(w)
        others = [m.get_channel_meter(i)[0] for i in range(1, len(m.channel_ids))]
        assert all(v == SILENCE_DB for v in others)

    def test_snapshot_with_allones_check_word_is_skipped(self):
        for idx in (38, 39):
            m = ReadbackMeter()
            w = make_readback()
            w[8] = 0xFFFF << 16
            w[idx] = 0xFFFFFFFF
            m.feed(w)
            assert m.get_channel_meter(0)[0] == SILENCE_DB

    def test_short_snapshot_is_skipped(self):
        m = ReadbackMeter()
        m.feed([0xFFFFFFFF] * 10)
        assert m.get_channel_meter(0)[0] == SILENCE_DB

    def test_out_of_range_channel(self):
        assert ReadbackMeter().get_channel_meter(999) == (SILENCE_DB, SILENCE_DB, False)

    def test_peak_holds_after_level_drops(self):
        m = ReadbackMeter()
        hi = make_readback()
        hi[8] = 0x8000 << 16
        m.feed(hi)
        m.feed(make_readback())
        level, peak, _ = m.get_channel_meter(0)
        assert level == SILENCE_DB
        assert peak == pytest.approx(-6.0, abs=0.05)

    def test_table_channel_ids_unique(self):
        ids = ReadbackMeter().channel_ids
        assert len(ids) == len(set(ids))


class TestMeterChannel:
    IDS = [0x0001, 0x0002, 0x0003, 0x0004, 0x0401, 0x0402, 0x0501, 0x0502,
           0x0701, 0x0702, 0x0b01, 0x0b02, 0x0901, 0x0902, 0x0201, 0x0202]

    def mc(self, kind, index, meter, name, stereo=False):
        return meter_channel(kind, index, meter, name, stereo, self.IDS)

    def test_analog(self):
        assert self.mc("inputs", 0, 0, "ANALOG 1") == 0
        assert self.mc("inputs", 2, 0, "ANALOG 3") == 2

    def test_stereo_right_meter(self):
        assert self.mc("inputs", 0, 1, "ANALOG 1", stereo=True) == 1
        assert self.mc("inputs", 0, 1, "ANALOG 1", stereo=False) == 0

    def test_adat_virtual_spdif_talkback(self):
        assert self.mc("inputs", 4, 0, "ADAT 1") == self.IDS.index(0x0401)
        assert self.mc("inputs", 8, 0, "VIRTUAL 2") == self.IDS.index(0x0702)
        assert self.mc("inputs", 12, 0, "S/PDIF L") == self.IDS.index(0x0501)
        assert self.mc("inputs", 13, 0, "S/PDIF R") == self.IDS.index(0x0502)
        assert self.mc("inputs", 20, 1, "TALKBACK") == self.IDS.index(0x0b02)

    def test_monitor_and_aux(self):
        assert self.mc("outputs", 18, 0, "MONITOR") == self.IDS.index(0x0901)
        assert self.mc("outputs", 18, 1, "MONITOR") == self.IDS.index(0x0902)
        assert self.mc("outputs", 18, 2, "MONITOR") is None
        assert self.mc("auxs", 0, 0, "AUX 1") == self.IDS.index(0x0201)
        assert self.mc("auxs", 0, 1, "AUX 1") == self.IDS.index(0x0202)

    def test_unrecorded_things_have_no_meter(self):
        assert self.mc("inputs", 5, 0, "ADAT 99") is None
        assert self.mc("outputs", 0, 0, "LINE 1") is None
        assert self.mc("inputs", 0, 0, None) is None

    def test_name_is_case_insensitive(self):
        assert self.mc("inputs", 0, 0, "analog 1") == 0
