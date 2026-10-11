"""Engine-side details from the Mixer Helper / Engine comparison (N32,
N34)."""

import json

from commands import refused_set
from protocol import Command
from ws_protocol import WsCommand
from ws_server import WsServer


class Client:
    def __init__(self):
        self.ubjson = False
        self.helper = False
        self.sent = []
        self.id = "c1"

    def send(self, data):
        self.sent.append(data)


def daemon(tree, router):
    from ua_mixer_daemon import MixerDaemon, apply_runtime_properties
    apply_runtime_properties(tree)
    return MixerDaemon(tree, router)


def test_dante_statuses_are_ints(tree, router):
    daemon(tree, router)
    for path in ("/DanteDevicesStatus", "/DanteErrorStatus"):
        assert tree.get_value(f"{path}/value") == 0
        assert tree.get_prop_dict(f"{path}/value")["type"] == "int"


def test_only_an_empty_device_name_is_refused():
    assert refused_set("/devices/0/DeviceName/value", "")
    assert refused_set("/devices/1/DeviceName", None)
    assert not refused_set("/devices/0/DeviceName/value", "Studio x4")
    assert not refused_set("/devices/0/inputs/0/Name/value", "")


def test_empty_device_name_is_refused(tree, router):
    d = daemon(tree, router)
    before = tree.get_value("/devices/0/DeviceName/value")
    c = Client()
    d._handle_set(c, Command(verb="set", path="/devices/0/DeviceName/value",
                             value=""))
    assert tree.get_value("/devices/0/DeviceName/value") == before
    assert len(c.sent) == 1 and b"DeviceName" in c.sent[0]   # an error


def test_device_name_is_kept(tree, router):
    d = daemon(tree, router)
    c = Client()
    d._handle_set(c, Command(verb="set", path="/devices/0/DeviceName/value",
                             value="Studio x4"))
    assert tree.get_value("/devices/0/DeviceName/value") == "Studio x4"
    # persisted with the rest of the client-set state
    assert "/devices/0/DeviceName/value" in tree._dirty


def test_empty_device_name_is_refused_on_websocket(tree, router):
    before = tree.get_value("/devices/0/DeviceName/value")
    ws = WsServer(tree, router)
    out = ws._handle_set(object(), WsCommand(
        verb="set", path="/devices/0/DeviceName/value", value="",
        message_id="a:1"))
    assert tree.get_value("/devices/0/DeviceName/value") == before
    msg = json.loads(out[0])
    assert msg["path"] == "/devices/0/DeviceName/value" and "error" in msg
    assert msg["parameters"] == {"message_id": "a:1"}
