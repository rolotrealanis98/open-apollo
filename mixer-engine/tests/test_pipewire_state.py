"""Settings › PIPEWIRE: /openapollo/pipewire/* from a synthetic pw-dump
(tests/fixtures/pipewire_dump.json) and pw-metadata writes."""

import copy
import json
from pathlib import Path
from unittest.mock import Mock

import pipewire_state as pw
from fake_hw_daemon import FixturePipeWire
from protocol import Command
from state_tree import StateTree
from ua_mixer_daemon import RUNTIME_PROPERTIES, MixerClient, MixerDaemon

DUMP = json.loads((Path(__file__).parent / "fixtures" /
                   "pipewire_dump.json").read_text())

TOP = """\
S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME
C  101      0      0    ---     ---   ---   ---     0                  alsa_input.pci-test.pro-input-0
S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME
R  101    512  48000  21.3us   9.6us  0.02  0.01    3   S32LE 22 48000 alsa_input.pci-test.pro-input-0
R  100      0      0   0.0us   0.0us  ???   ???     1   S32LE 24 48000  + alsa_output.pci-test.pro-output-0
R  140      0      0   0.0us   0.0us  ???   ???     7   S16LE 2 44100 alsa_output.pci-other.analog-stereo
"""


def state(objs=None, top=""):
    fake = FixturePipeWire(copy.deepcopy(DUMP if objs is None else objs))
    calls = []
    fake.log = calls.append
    run = (lambda cmd, env: top if cmd[0] == "pw-top" else fake.run(cmd, env))
    s = pw.PipeWireState(run=run, locate=lambda card: ({}, "pipewire (4242)"))
    return s, calls


def test_fixture_lists_the_apollo_nodes_and_their_streams():
    s, _ = state()
    v = s.poll()
    nodes = json.loads(v["Nodes"])
    # the ALSA nodes first; the other card is not the Apollo's
    assert [n["name"] for n in nodes] == [
        "alsa_output.pci-test.pro-output-0", "alsa_input.pci-test.pro-input-0",
        "apollo_mic_1", "apollo_monitor"]
    assert nodes[0] == {"id": 100, "name": "alsa_output.pci-test.pro-output-0",
                        "description": "Apollo Test Pro", "class": "Audio/Sink",
                        "state": "running", "channels": 24, "rate": 0,
                        "quantum": 0}
    assert (nodes[2]["channels"], nodes[3]["quantum"]) == (1, 256)
    streams = json.loads(v["Streams"])
    # two links of one loopback make one row; the other app is left out
    assert [(x["app"], x["direction"], x["target"], x["pid"]) for x in streams] == [
        ("Test Mic 1", "capture", "Apollo Test Pro", 0),
        ("Test Monitor L/R", "playback", "Apollo Test Pro", 0),
        ("Test Player", "playback", "Test Monitor L/R", 4242)]
    assert (v["Available"], v["Rate"], v["Quantum"], v["MinQuantum"],
            v["MaxQuantum"], v["ForceQuantum"], v["Xruns"], v["Owner"]) == (
        True, 48000, 1024, 32, 2048, 0, 0, "pipewire (4242)")
    assert json.loads(v["AllowedRates"]) == [44100, 48000, 96000]


def test_pw_top_gives_the_running_quantum_and_xruns():
    rows = pw.parse_top(TOP)
    assert rows[101] == {"quantum": 512, "rate": 48000, "err": 3}
    assert rows[100]["rate"] == 48000       # from FORMAT while RATE is 0
    s, _ = state(top=TOP)
    v = s.poll()
    # the output node follows its driver (the input); the other card's
    # xruns are not counted
    assert (v["Quantum"], v["Xruns"]) == (512, 4)
    assert json.loads(v["Nodes"])[1]["quantum"] == 512


def test_without_pipewire_nothing_is_available():
    s = pw.PipeWireState(run=lambda cmd, env: None,
                         locate=lambda card: ({}, ""))
    v = s.poll()
    assert (v["Available"], v["Nodes"], v["Streams"], v["Quantum"]) == (
        False, "[]", "[]", 0)
    assert s.force("ForceQuantum", 256) is None
    # no socket, no owner: not even pw-dump runs
    s = pw.PipeWireState(run=Mock(), locate=lambda card: (None, ""))
    assert not s.poll()["Available"] and not s.run.called


def test_force_quantum_and_rate_go_through_pw_metadata():
    s, calls = state()
    s.poll()
    assert s.force("ForceQuantum", 256) == {"ForceQuantum": 256, "Quantum": 256}
    assert calls == [{"hw": "pw-metadata", "args": ["clock.force-quantum", "256"]}]
    assert s.poll()["ForceQuantum"] == 256      # the fixture took it
    for bad in (300, 16, 4096, True, "x", 256.5):
        assert s.force("ForceQuantum", bad) is None, bad
    assert s.force("ForceQuantum", "0") == {"ForceQuantum": 0}
    assert s.force("ForceRate", 96000.0) == {"ForceRate": 96000, "Rate": 96000}
    assert s.force("ForceRate", 32000) is None
    assert s.force("Rate", 48000) is None       # read-only
    assert len(calls) == 3


def test_get_setting_parses_pw_metadata():
    out = ("Found \"settings\" metadata 44\n"
           "update: id:0 key:'clock.rate' value:'48000' type:''\n")
    assert pw.get_setting("clock.rate", {}, lambda cmd, env: out) == "48000"
    assert pw.get_setting("clock.rate", {}, lambda cmd, env: None) is None


class Writer:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


def daemon(available=True):
    tree = StateTree()
    for path in [*pw.PROPERTIES, "/BufferSize"]:
        tree.add_runtime_property(path, copy.deepcopy(RUNTIME_PROPERTIES[path]))
    tree._schedule_save = Mock()
    d = MixerDaemon(tree, None)
    s, calls = state() if available else (
        pw.PipeWireState(run=lambda c, e: None, locate=lambda c: ({}, "")), [])
    d.pipewire = s
    d._publish_pipewire(s.poll())
    client = MixerClient(None, Writer())
    return d, client, calls


def test_daemon_publishes_the_poll_and_buffer_size():
    d, _, _ = daemon()
    v = d.state.get_value
    assert v("/openapollo/pipewire/Available/value") is True
    assert v("/BufferSize/value") == 1024
    assert "/openapollo/pipewire/Nodes/value" not in d.state._dirty
    assert d.state.is_readonly("/openapollo/pipewire/Nodes/value")


def test_daemon_set_force_quantum_and_buffer_size():
    d, client, calls = daemon()
    d._handle_set(client, Command("set", "/openapollo/pipewire/ForceQuantum/value", 256,
                                  raw_params={"func_id": "3"}))
    assert d.state.get_value("/openapollo/pipewire/ForceQuantum/value") == 256
    assert d.state.get_value("/BufferSize/value") == 256
    # Settings › HARDWARE › BUFFER SIZE forces the quantum
    d._handle_set(client, Command("set", "/BufferSize/value", 512))
    assert calls[-1]["args"] == ["clock.force-quantum", "512"]
    assert d.state.get_value("/openapollo/pipewire/ForceQuantum/value") == 512
    # refused: the value stays, the client gets the engine's error
    d._handle_set(client, Command("set", "/openapollo/pipewire/ForceQuantum/value", 300,
                                  raw_params={"func_id": "4"}))
    assert d.state.get_value("/openapollo/pipewire/ForceQuantum/value") == 512
    assert b"Unable to set value." in client.writer.frames[-1]
    assert len(calls) == 2


def test_daemon_buffer_size_without_pipewire_is_only_stored():
    d, client, _ = daemon(available=False)
    assert d.state.get_value("/openapollo/pipewire/Available/value") is False
    d._handle_set(client, Command("set", "/BufferSize/value", 512))
    assert d.state.get_value("/BufferSize/value") == 512
    d._handle_set(client, Command("set", "/openapollo/pipewire/ForceQuantum/value", 256,
                                  raw_params={"func_id": "5"}))
    assert d.state.get_value("/openapollo/pipewire/ForceQuantum/value") == 0


def test_daemon_polls_only_while_subscribed():
    import asyncio
    d, _, _ = daemon()
    d.pipewire.poll = Mock(return_value={"Xruns": 9})

    async def tick():
        d._poll_pipewire()
        if d._pw_task:
            await d._pw_task
    asyncio.run(tick())
    assert not d.pipewire.poll.called
    d.state.subscribe("c1", "/openapollo/pipewire/Xruns/value")
    asyncio.run(tick())
    assert d.state.get_value("/openapollo/pipewire/Xruns/value") == 9
    asyncio.run(tick())                 # not again within 2 s
    assert d.pipewire.poll.call_count == 1
