#!/usr/bin/env python3
"""virtual-insert-roundtrip.py's analysis on a synthetic recording (no hardware, no PipeWire).

Run:  python3 tools/test-virtual-insert-roundtrip.py
"""
import array, importlib.util, os, tempfile, wave

spec = importlib.util.spec_from_file_location(
    'roundtrip', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'virtual-insert-roundtrip.py'))
rt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rt)

with tempfile.TemporaryDirectory() as tmp:
    rt.write_signal(os.path.join(tmp, 'played.wav'))
    with wave.open(os.path.join(tmp, 'played.wav')) as w:
        played = array.array('h', w.readframes(w.getnframes()))[0::2]
    # What the stream records: the monitor (played, 512 samples of graph delay) on L/R, then
    # Virtual 1 back 131 samples later at half level and Virtual 2 back 100 samples later.
    mon = [v << 16 for v in [0] * 512 + list(played)]
    v1 = [0] * 131 + [v // 2 for v in mon]
    v2 = [0] * 100 + mon
    n = len(mon)
    frames = array.array('i', (s for k in range(n) for s in (mon[k], mon[k], v1[k], v2[k])))
    path = os.path.join(tmp, 'recorded.wav')
    with wave.open(path, 'wb') as w:
        w.setnchannels(4), w.setsampwidth(4), w.setframerate(rt.RATE)
        w.writeframes(frames.tobytes())
    ch = rt.channels(path)
    a, b = rt.analyse(ch[0], ch[2]), rt.analyse(ch[1], ch[3])
    assert a['offset_samples'] == 131 and a['spread'] == 0, a
    assert b['offset_samples'] == 100 and abs(b['tone_change_db']) < 0.05, b
    assert abs(a['tone_change_db'] + 6.02) < 0.05, a
    assert rt.analyse(ch[0], [0] * n) is None                    # silence back: nothing came back
print('ok')
