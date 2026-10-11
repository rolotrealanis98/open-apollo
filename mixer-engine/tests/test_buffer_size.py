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
