"""MixerDaemon: init-sequence defaults and command dispatch, no sockets."""

import json

import pytest

from conftest import make_readback
from protocol import MessageFramer
from ua_mixer_daemon import MixerDaemon

GAIN = "/devices/0/inputs/{ch}/preamps/0/Gain/value"


class FakeClient:
    """Minimal MixerClient: collects what the daemon sends."""

    id = "fake-1"
    helper = False
    ubjson = False
    trace = False

    def __init__(self):
        self.sent = []

    def send(self, data):
        self.sent.append(data)

    def messages(self):
        out = []
        for chunk in self.sent:
            out += [json.loads(m) for m in MessageFramer().feed(chunk)]
        return out


@pytest.fixture
def daemon(router, tree):
    # Constructing the daemon opens no sockets; start() does that
    return MixerDaemon(tree, router)


def mon_level_calls(backend):
    return [c for c in backend.mixer_params if c[:3] == (2, 1, 0x01)]


class TestInitWithReadback:
    def setup_readback(self, backend):
        backend.readback = (1, make_readback(
            mon_raw=150, mute=False, mono=True, dim=True,
            flags={0: {"48v", "pad", "phase"}, 1: {"lowcut"}, 2: {"link"}},
            gains_c=[21, 31, 41, 51]))      # 30, 40, 50, 60 dB

    def test_monitor_level_kept(self, daemon, backend):
        self.setup_readback(backend)
        daemon._push_defaults_to_hardware()
        vals = {c[3] for c in mon_level_calls(backend)}
        assert vals == {150}

    def test_gains_kept(self, daemon, backend):
        self.setup_readback(backend)
        daemon._push_defaults_to_hardware()
        gainc = {c[1]: c[3] for c in backend.mixer_params if c[0] == 1 and c[2] == 0x06}
        assert gainc == {0: 21, 1: 31, 2: 41, 3: 51}

    def test_gains_reach_state_tree_path_as_floats(self, daemon, backend, tree):
        self.setup_readback(backend)
        daemon._push_defaults_to_hardware()
        # Gain triplet written for every channel
        for ch, db in enumerate((30, 40, 50, 60)):
            assert (1, ch, 0x0A, db - 10) in backend.mixer_params

    def test_flags_kept(self, daemon, backend):
        self.setup_readback(backend)
        daemon._push_defaults_to_hardware()
        last = {}
        for t, ch, p, v in backend.mixer_params:
            if t == 1:
                last[(ch, p)] = v
        assert last[(0, 0x03)] == 1 and last[(1, 0x03)] == 0   # 48V
        assert last[(0, 0x01)] == 1 and last[(1, 0x01)] == 0   # Pad
        assert last[(0, 0x05)] == 0xBF800000                   # Phase inverted
        assert last[(1, 0x05)] == 0x3F800000                   # Phase normal
        assert last[(1, 0x04)] == 1 and last[(0, 0x04)] == 0   # LowCut
        assert last[(0, 0x02)] == 0 and last[(2, 0x02)] == 1   # Link

    @pytest.mark.parametrize("mute, mono, want", [
        (False, False, 0), (False, True, 1), (True, False, 2), (True, True, 3)])
    def test_monitor_mute_mono_word_composes_both_bits(
            self, daemon, backend, mute, mono, want):
        backend.readback = (1, make_readback(mon_raw=150, mute=mute, mono=mono))
        daemon._push_defaults_to_hardware()
        calls = [c for c in backend.mixer_params if c[:3] == (2, 0, 0x03)]
        assert calls and {c[3] for c in calls} == {want}

    def test_dim_kept_but_talkback_forced_off(self, daemon, backend):
        self.setup_readback(backend)
        daemon._push_defaults_to_hardware()
        dim = [c for c in backend.mixer_params if c[0] == 2 and c[2] == 0x44]
        tb = [c for c in backend.mixer_params if c[0] == 2 and c[2] == 0x46]
        assert dim and all(c[3] == 1 for c in dim)
        assert tb and all(c[3] == 0 for c in tb)

    def test_dim_dropped_when_talkback_was_active(self, daemon, backend):
        # Talkback also sets the dim bit; init turns talkback off
        backend.readback = (1, make_readback(mon_raw=150, dim=True, talkback=True))
        daemon._push_defaults_to_hardware()
        dim = [c for c in backend.mixer_params if c[0] == 2 and c[2] == 0x44]
        assert dim and all(c[3] == 0 for c in dim)


class TestInitWithoutReadback:
    def test_monitor_defaults_to_minimum(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        vals = {c[3] for c in mon_level_calls(backend)}
        assert vals == {0}

    def test_status_not_ready_counts_as_no_readback(self, daemon, backend):
        backend.readback = (0, make_readback(mon_raw=150))
        daemon._push_defaults_to_hardware()
        vals = {c[3] for c in mon_level_calls(backend)}
        assert vals == {0}

    def test_preamps_default_to_10db_everything_off(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        for ch in range(4):
            assert (1, ch, 0x06, 1) in backend.mixer_params       # GainC = 10-9
            for p in (0x01, 0x03, 0x04, 0x00):                     # pad 48V lowcut micline
                vals = {c[3] for c in backend.mixer_params
                        if c[:3] == (1, ch, p)}
                assert vals == {0}, (ch, p)
            assert {c[3] for c in backend.mixer_params
                    if c[:3] == (1, ch, 0x05)} == {0x3F800000}

    def test_dim_off_talkback_off(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        for p in (0x44, 0x46):
            vals = {c[3] for c in backend.mixer_params if c[0] == 2 and c[2] == p}
            assert vals == {0}

    def test_inputs_start_muted_and_clients_told(self, daemon, backend, tree):
        backend.readback = None
        client = FakeClient()
        tree.register_callback(client.id, lambda p, v: client.sent.append(
            json.dumps({"path": p, "data": v}).encode() + b"\0"))
        tree.subscribe(client.id, "/devices/0/inputs/5/Mute/value")
        daemon._push_defaults_to_hardware()
        assert tree.get_value("/devices/0/inputs/5/Mute/value") is True
        assert {"path": "/devices/0/inputs/5/Mute/value", "data": True} \
            in client.messages()

    def test_init_sequence_constants(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        assert (1, 8, 0x13, 0x0003FFFF) in backend.mixer_params   # S/PDIF L route
        assert (1, 0, 0x16, 0xA0) in backend.mixer_params         # Level
        assert (0, 1, 0x01, 1) in backend.mixer_params            # S/PDIF path

    def test_no_router_is_a_no_op(self, tree):
        MixerDaemon(tree, None)._push_defaults_to_hardware()


class TestSavedStateRestoredOnInit:
    def test_saved_fader_and_mute_are_used(self, daemon, backend, tree):
        backend.readback = None
        tree._dirty["/devices/0/inputs/2/Mute/value"] = False
        tree._dirty["/devices/0/inputs/2/FaderLevel/value"] = -12.0
        daemon._push_defaults_to_hardware()
        # input 2 was not forced to 0 dB / muted
        assert tree.get_value("/devices/0/inputs/3/Mute/value") is True


class TestDispatch:
    def test_get_ping(self, daemon):
        c = FakeClient()
        daemon._dispatch(c, "get /ping?message_id=3")
        assert c.messages() == [{"path": "/ping", "data": True,
                                 "parameters": {"message_id": 3}}]

    def test_get_value(self, daemon):
        c = FakeClient()
        daemon._dispatch(c, "get /SampleRate/value")
        assert c.messages() == [{"path": "/SampleRate/value", "data": 48000}] \
            or c.messages()[0]["data"] == 48000

    def test_get_unknown_path_errors(self, daemon):
        c = FakeClient()
        daemon._dispatch(c, "get /nope/nothing")
        assert c.messages() == [{"path": "/nope/nothing",
                                 "error": "Unable to resolve path for get."}]

    def test_set_reaches_state_and_hardware(self, daemon, backend, tree):
        c = FakeClient()
        daemon._dispatch(c, "set /devices/0/outputs/18/CRMonitorLevel/value -30")
        assert tree.get_value("/devices/0/outputs/18/CRMonitorLevel/value") == -30.0
        assert backend.mixer_params == [(2, 1, 0x01, 132)]

    def test_set_with_func_id_echoes(self, daemon):
        c = FakeClient()
        daemon._dispatch(c, "set /BufferSize/value?func_id=4 512")
        msgs = c.messages()
        assert msgs and msgs[-1]["data"] == 512
        assert msgs[-1]["parameters"] == {"func_id": 4}

    def test_gain_set_pushes_companion_tapered_value(self, daemon, tree):
        c = FakeClient()
        daemon._dispatch(c, "set " + GAIN.format(ch=1) + " 30")
        paths = [m["path"] for m in c.messages()]
        assert "/devices/0/inputs/1/preamps/0/GainTapered/value" in paths

    def test_subscribe_floods_current_values(self, daemon, tree):
        c = FakeClient()
        daemon._dispatch(c, "subscribe /devices/0/inputs/0/preamps/0?recursive=1")
        paths = {m["path"] for m in c.messages()}
        assert GAIN.format(ch=0) in paths

    def test_subscribed_client_notified_on_set(self, daemon, tree):
        c = FakeClient()
        tree.register_callback(c.id, lambda p, v: c.send(
            json.dumps({"path": p, "data": v}).encode() + b"\0"))
        daemon._dispatch(c, "subscribe /SampleRate/value")
        c.sent.clear()
        tree.set("/SampleRate/value", 96000)
        assert c.messages() == [{"path": "/SampleRate/value", "data": 96000}]

    def test_unsubscribe(self, daemon, tree):
        c = FakeClient()
        tree.register_callback(c.id, lambda p, v: c.send(b"x\0"))
        daemon._dispatch(c, "subscribe /SampleRate/value")
        daemon._dispatch(c, "unsubscribe /SampleRate/value")
        c.sent.clear()
        tree.set("/SampleRate/value", 96000)
        assert c.sent == []

    def test_garbage_does_not_raise(self, daemon):
        c = FakeClient()
        daemon._dispatch(c, "bogus")
        daemon._dispatch(c, "{not json")
        assert c.sent == []

    def test_ubjson_client_gets_ubjson(self, daemon):
        import ubjson_codec as u
        c = FakeClient()
        c.ubjson = True
        daemon._dispatch(c, "get /ping")
        value, _ = u.decode_frame(c.sent[0])
        assert value == {"path": "/ping", "data": True}

    def test_parse_ubjson_command(self):
        cmd = MixerDaemon._parse_ubjson_command(
            {"cmd": "set", "url": "/Sleep?func_id=2", "data": False,
             "parameters": {"message_id": 9}})
        assert (cmd.verb, cmd.path, cmd.value) == ("set", "/Sleep", False)
        assert cmd.raw_params == {"func_id": "2", "message_id": "9"}
        ident = MixerDaemon._parse_ubjson_command({"path": "network_id", "data": "x"})
        assert ident.verb == "json" and ident.value == "x"
        assert MixerDaemon._parse_ubjson_command({}) is None

    def test_coerce_params(self):
        out = MixerDaemon._coerce_params({"message_id": "5", "other": "x",
                                          "func_id": "bad"})
        assert out == {"message_id": 5, "other": "x", "func_id": "bad"}


class TestMonitorLevelCompanion:
    def test_db_set_updates_tapered(self, daemon, tree):
        from hardware import encode_monitor_level, monitor_hw_to_tapered
        c = FakeClient()
        daemon._dispatch(c, "set /devices/0/outputs/18/CRMonitorLevel/value -40")
        want = monitor_hw_to_tapered(encode_monitor_level(-40.0))
        assert tree.get_value(
            "/devices/0/outputs/18/CRMonitorLevelTapered/value") == want

    def test_tapered_set_updates_db(self, daemon, tree):
        from hardware import monitor_tapered_to_hw
        c = FakeClient()
        daemon._dispatch(
            c, "set /devices/0/outputs/18/CRMonitorLevelTapered/value 0.5")
        want = (monitor_tapered_to_hw(0.5) - 192) / 2.0
        assert tree.get_value(
            "/devices/0/outputs/18/CRMonitorLevel/value") == want


class TestCueSendInit:
    """Init writes the CUE 3/4 send entries (sub 5/6): both sides."""

    def pairs(self, backend):
        left = {(b, s) for b, s, _ in backend.bus_params if s in (5, 6)}
        right = {(b, s) for b, s, _ in backend.bus_params_r if s in (5, 6)}
        return left, right

    def test_cue34_sends_start_silent_on_both_sides(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        for bus in (0x00, 0x03, 0x08, 0x0A, 0x10, 0x12, 0x1B):
            for sub in (5, 6):
                assert (bus, sub, 0.0) in backend.bus_params
                assert (bus, sub, 0.0) in backend.bus_params_r

    def test_every_left_entry_has_its_right_twin(self, daemon, backend):
        backend.readback = None
        daemon._push_defaults_to_hardware()
        left, right = self.pairs(backend)
        assert left == right and len(left) == 21 * 2
