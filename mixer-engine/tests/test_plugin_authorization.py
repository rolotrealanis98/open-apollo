"""plugin_authorization: the daemon's /plugins answer, read like a client would."""
import json
import socket
import threading

import plugin_authorization as auth


def serve(children, notify_first=True):
    """A one-shot daemon: optional unsolicited notification, then the reply to the get."""
    server = socket.socket()
    server.bind(('127.0.0.1', 0))
    server.listen(1)

    def run():
        conn, _ = server.accept()
        with conn:
            request = b''
            while b'\0' not in request:
                request += conn.recv(4096)
            func_id = request.split(b'func_id=')[1].split(b'&')[0].decode()
            if notify_first:
                conn.sendall(json.dumps({'path': '/Meters', 'data': 1}).encode() + b'\0')
            reply = {'path': '/plugins', 'data': {'children': children}, 'parameters': {'func_id': func_id}}
            conn.sendall(json.dumps(reply).encode() + b'\0')
        server.close()
    threading.Thread(target=run, daemon=True).start()
    return server.getsockname()[1]


def plugin(name, authorized=False, demoing=False, status=''):
    return {'properties': {'Name': {'value': name}, 'Authorized': {'value': authorized},
                           'Demoing': {'value': demoing}, 'Status': {'value': status}}}


def test_daemon_answer_states():
    port = serve({'0': plugin('UAD A', authorized=True), '1': plugin('UAD B', demoing=True, status='Demo'),
                  '2': plugin('UAD C', status='Not authorized'), '3': plugin('UAD D')})
    answer = auth.authorized_plugins(port=port, device='/nonexistent')
    assert answer['source'] == 'daemon'
    assert answer['plugins'] == {'UAD A': 'authorized', 'UAD B': 'demo', 'UAD C': 'unauthorized', 'UAD D': 'unknown'}
    assert auth.authorized_names(answer) == {'UAD A', 'UAD B'}


def test_engine_without_a_device_is_no_answer():
    # UA's own engine with no Apollo: every plug-in "No UAD-2 devices found", none authorized.
    port = serve({str(i): plugin(f'UAD {i}', status='No UAD-2 devices found') for i in range(3)})
    answer = auth.authorized_plugins(port=port, device='/nonexistent')
    assert answer == {'source': None, 'plugins': {}}
    assert auth.authorized_names(answer) == set()


def test_no_daemon_and_no_device():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
    assert auth.authorized_plugins(port=port, device='/nonexistent', timeout=1)['source'] is None
