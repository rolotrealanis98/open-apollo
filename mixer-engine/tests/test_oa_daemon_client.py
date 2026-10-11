"""tools/oa-daemon-client.py against a scripted NUL-framed server."""

import importlib.util
import json
import socket
import threading

import pytest

from conftest import ENGINE_DIR

TOOL = ENGINE_DIR.parent / "tools" / "oa-daemon-client.py"


@pytest.fixture(scope="module")
def client():
    spec = importlib.util.spec_from_file_location("oa_daemon_client", TOOL)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Server:
    """One-connection server: records the NUL-framed commands it gets and
    answers each with the messages `reply(cmd)` returns."""

    def __init__(self, reply):
        self.reply = reply
        self.commands = []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        conn, _ = self.sock.accept()
        buf = b""
        with conn:
            while True:
                data = conn.recv(4096)
                if not data:
                    break
                buf += data
                while b"\0" in buf:
                    raw, buf = buf.split(b"\0", 1)
                    cmd = raw.decode()
                    self.commands.append(cmd)
                    for msg in self.reply(cmd):
                        conn.sendall(json.dumps(msg).encode() + b"\0")
        self.sock.close()


def run(client, server, *argv):
    return client.main(["--port", str(server.port), "--timeout", "0.3",
                        *argv])


def test_get_prints_the_value(client, capsys):
    srv = Server(lambda c: [{"path": "/a/b", "data": -6.5}])
    assert run(client, srv, "get", "/a/b") == 0
    assert capsys.readouterr().out.strip() == "-6.5"
    assert srv.commands == ["get /a/b"]


def test_get_error_exits_1(client, capsys):
    srv = Server(lambda c: [{"path": "/a/b", "error": "Unable to resolve"}])
    assert run(client, srv, "get", "/a/b") == 1
    assert "Unable to resolve" in capsys.readouterr().err


def test_get_without_reply_exits_1(client):
    srv = Server(lambda c: [])
    assert run(client, srv, "get", "/a/b") == 1


def test_set_asks_for_an_echo(client, capsys):
    srv = Server(lambda c: [{"path": "/a/b", "data": 1}])
    assert run(client, srv, "set", "/a/b", "true") == 0
    assert srv.commands == ["set /a/b?func_id=1 true"]
    assert '"data":1' in capsys.readouterr().out


def test_set_error_exits_1(client):
    srv = Server(lambda c: [{"path": "/a/b", "error": "nope"}])
    assert run(client, srv, "set", "/a/b", "3") == 1


def test_watch_subscribes_to_every_path_and_prints_pushes(client, capsys):
    srv = Server(lambda c: [{"path": c.split()[1], "data": 0}])
    assert run(client, srv, "watch", "--for", "0.4", "/x", "/y") == 0
    assert srv.commands == ["subscribe /x", "subscribe /y"]
    out = capsys.readouterr().out
    assert '"path":"/x"' in out and '"path":"/y"' in out


def test_connection_refused_exits_2(client, capsys):
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    assert client.main(["--port", str(port), "get", "/a"]) == 2
    assert "cannot connect" in capsys.readouterr().err
