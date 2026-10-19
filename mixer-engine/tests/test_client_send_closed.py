"""A client whose connection broke is written to no more (one dropped Console connection logged
asyncio's "socket.send() raised exception" 498 times)."""
from ua_mixer_daemon import MixerClient


class Writer:
    def __init__(self):
        self.closing, self.writes = False, []

    def is_closing(self):
        return self.closing

    def write(self, data):
        self.writes.append(data)

    def get_extra_info(self, key):
        return None


def test_no_writes_after_the_connection_broke():
    w = Writer()
    c = MixerClient(None, w)
    c.send(b'{"a":1}\0')
    w.closing = True                 # asyncio force-closed the transport after a send error
    for _ in range(500):
        c.send(b'{"a":2}\0')
    assert len(w.writes) == 1 and c.closed
