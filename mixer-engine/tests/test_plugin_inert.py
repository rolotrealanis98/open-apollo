"""Without an injected plugin backend the plugin hooks are inert: effect-slot
SETs and GETs behave exactly as before the plugin port."""

import json

from protocol import Command
from ua_mixer_daemon import MixerClient, MixerDaemon

SLOT = "/devices/0/inputs/0/effects/0"


class MemoryWriter:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4710)

    def write(self, data):
        self.frames.append(data)


def replies(writer):
    return [json.loads(f.rstrip(b"\0")) for f in writer.frames]


def test_effect_set_reaches_the_tree_without_a_backend(tree):
    daemon = MixerDaemon(tree, None)
    assert not daemon.plugins_enabled and not daemon.plugin_remote_enabled
    daemon._session_ready = True
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    power = tree.get_value(f"{SLOT}/Power/value")
    for field, value in (("Power", not power), ("EffectName", "x")):
        path = f"{SLOT}/{field}/value"
        daemon._handle_set(client, Command("set", path, value,
                                           raw_params={"func_id": "5"}))
        assert tree.get_value(path) == value
    # the echo, as on driver-v2 before the port
    assert replies(writer)[0] == {"path": f"{SLOT}/Power/value",
                                  "data": not power,
                                  "parameters": {"func_id": 5}}
    assert all("error" not in r for r in replies(writer))


def test_vendor_specific_get_falls_through_without_a_backend(tree):
    daemon = MixerDaemon(tree, None)
    writer = MemoryWriter()
    client = MixerClient(None, writer)
    path = f"{SLOT}/VendorSpecific"
    daemon._handle_get(client, Command("get", path, None,
                                       raw_params={"opcode": "1"}))
    out = replies(writer)
    assert out and all("openapollo" not in json.dumps(r) for r in out)
