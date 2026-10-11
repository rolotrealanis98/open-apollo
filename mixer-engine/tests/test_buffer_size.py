"""/BufferSize follows the period size of the Apollo's running stream."""
import ua_mixer_daemon as d


def test_period_from_hw_params(tmp_path, monkeypatch):
    base = tmp_path / "card3" / "pcm0p" / "sub0"
    base.mkdir(parents=True)
    (base / "hw_params").write_text(
        "access: MMAP_INTERLEAVED\nformat: S32_LE\nchannels: 24\n"
        "rate: 48000 (48000/1)\nperiod_size: 512\nbuffer_size: 1024\n")
    real = d.Path

    def fake(p):
        p = str(p)
        if p.startswith("/proc/asound/"):
            return real(str(tmp_path) + p[len("/proc/asound"):])
        return real(p)
    monkeypatch.setattr(d, "Path", fake)
    assert d.stream_period_frames(3) == 512


def test_nothing_running(tmp_path, monkeypatch):
    real = d.Path
    monkeypatch.setattr(d, "Path", lambda p: real(str(tmp_path) + "/x" + str(p)))
    assert d.stream_period_frames(3) is None
    assert d.stream_period_frames(None) is None


def test_device_status_is_polled_without_clients(tree, monkeypatch):
    """The 1 Hz polls run with no client connected, so the first client
    gets the current /BufferSize instead of the start-up default."""
    import asyncio

    import pytest

    daemon = d.MixerDaemon(tree, None)
    assert not daemon.state._callbacks
    polls = []
    monkeypatch.setattr(daemon, "_poll_device_status",
                        lambda: polls.append(1))
    ticks = [0]

    async def fake_sleep(_):
        ticks[0] += 1
        if ticks[0] > 120:
            raise asyncio.CancelledError

    monkeypatch.setattr(d.asyncio, "sleep", fake_sleep)
    with pytest.raises(asyncio.CancelledError):
        asyncio.run(daemon._meter_pump())
    assert len(polls) == 2      # ticks 50 and 100
