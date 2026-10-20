#!/usr/bin/env python3
"""Round trip through the Virtual strips: play clicks and a tone to VIRTUAL 1/2, record them back.

    python3 tools/virtual-insert-roundtrip.py OUT_DIR [--pair 12|34]

Plays OUT_DIR/played.wav (clicks, then a 1 kHz tone at -20 dBFS, the same on L and R) into
"Apollo Virtual Out 1+2" and records, in one pw-record stream, that sink's monitor (what was
played) and "Apollo Virtual In 1+2" (the VIRTUAL 1/2 record channels), so the four channels share
one graph clock. Put the insert on Virtual 1 and leave Virtual 2 plain: Virtual 2 is the reference.
Prints, per Virtual input, the click offset (samples from played to recorded: the round trip a
PipeWire client sees, without its own quantum) and the tone's level change (dB), and the insert's
extra latency (Virtual 1 - Virtual 2); OUT_DIR/results.json keeps them. stdlib only. Needs the
Virtual nodes (configs/pipewire/setup-apollo-io.sh) and a running PipeWire; keep OUT_DIR outside
the repository.
"""
import argparse, array, json, math, signal, statistics, subprocess, sys, time, wave
from pathlib import Path

RATE, FULL = 48000, 2 ** 31
CLICKS = [int(RATE * (0.5 + 0.25 * k)) for k in range(8)]
TONE = (int(RATE * 3.0), int(RATE * 5.0))
REC = 'oa-virtual-roundtrip'


def write_signal(path):
    frames = array.array('h', bytes(4 * int(RATE * 5.5)))
    for at in CLICKS:
        frames[2 * at] = frames[2 * at + 1] = 16384                   # -6 dBFS, one sample
    amplitude = 32767 * 10 ** (-20 / 20)
    for n in range(*TONE):
        frames[2 * n] = frames[2 * n + 1] = round(amplitude * math.sin(2 * math.pi * 1000 * n / RATE))
    with wave.open(str(path), 'wb') as w:
        w.setnchannels(2), w.setsampwidth(2), w.setframerate(RATE)
        w.writeframes(frames.tobytes())


def ports(direction, node):
    out = subprocess.run(['pw-link', direction], capture_output=True, text=True, check=True).stdout
    return [line.strip() for line in out.splitlines() if line.strip().startswith(node + ':')]


def wait_ports(direction, node, count, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        found = ports(direction, node)
        if len(found) >= count:
            return found
        time.sleep(0.1)
    sys.exit(f'{node}: no ports ({direction}); is PipeWire running with the Virtual nodes?')


def channels(path):
    with wave.open(str(path), 'rb') as w:
        if w.getsampwidth() != 4 or w.getnchannels() != 4:
            sys.exit(f'{path}: expected 4 channels of 32-bit PCM')
        data = array.array('i', w.readframes(w.getnframes()))
    return [data[c::4] for c in range(4)]


def rms_db(x, start, end):
    window = x[start:end]
    return 10 * math.log10(max(sum(v * v for v in window) / len(window), 1) / FULL ** 2)


def analyse(played, back):
    """Offsets of each click (played -> back, samples) and the tone level change (dB)."""
    offsets = []
    for at in CLICKS:
        window = range(max(at - RATE // 10, 0), at + RATE // 10)
        p = max(window, key=lambda n: abs(played[n]))
        if abs(played[p]) < FULL // 8:
            continue                                        # the click was not played (yet)
        reply = range(p, min(p + RATE // 10, len(back)))
        q = max(reply, key=lambda n: abs(back[n]))
        if abs(back[q]) > FULL // 1000:                     # above -60 dBFS: it came back
            offsets.append(q - p)
    if not offsets:
        return None
    lag = round(statistics.median(offsets))
    start = next((n for n in range(TONE[0] - RATE, len(played)) if abs(played[n]) > FULL // 100), TONE[0])
    middle = (start + RATE // 4, start + RATE * 7 // 4)
    gain = rms_db(back, middle[0] + lag, middle[1] + lag) - rms_db(played, *middle)
    return dict(offset_samples=lag, offsets=offsets, spread=max(offsets) - min(offsets),
                tone_change_db=round(gain, 2), back_peak_dbfs=round(20 * math.log10(max(max(map(abs, back)), 1) / FULL), 1))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('out', type=Path)
    ap.add_argument('--pair', default='12', choices=('12', '34'))
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    sink, source = f'apollo_virtual_out_{args.pair}', f'apollo_virtual_in_{args.pair}'
    first = 1 if args.pair == '12' else 3
    write_signal(args.out / 'played.wav')
    rec = subprocess.Popen(['pw-record', '--target', '0', '-P', f'{{ node.name = "{REC}" node.autoconnect = false }}',
                            '--rate', str(RATE), '--channels', '4', '--channel-map', 'FL,FR,RL,RR',
                            '--format', 's32', str(args.out / 'recorded.wav')])
    try:
        inputs = wait_ports('-i', REC, 4)[:4]
        monitor = [p for p in wait_ports('-o', sink, 2) if 'monitor' in p][:2]
        capture = [p for p in wait_ports('-o', source, 2) if 'monitor' not in p][:2]
        if len(monitor) != 2 or len(capture) != 2:
            sys.exit(f'ports not found: monitor {monitor}, capture {capture}')
        for out, inp in zip(monitor + capture, inputs):
            subprocess.run(['pw-link', out, inp], check=True)
        time.sleep(0.5)
        subprocess.run(['pw-play', '--target', sink, str(args.out / 'played.wav')], check=True)
        time.sleep(0.5)
    finally:
        rec.send_signal(signal.SIGINT)
        rec.wait(timeout=10)
    ch = channels(args.out / 'recorded.wav')
    results = {f'Virtual {first + k}': analyse(ch[k], ch[2 + k]) for k in (0, 1)}
    a, b = results.values()
    if a and b:
        results['insert_extra_samples'] = a['offset_samples'] - b['offset_samples']
    (args.out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    for name, r in results.items():
        if isinstance(r, dict):
            print(f"{name}: round trip {r['offset_samples']} samples ({r['offset_samples'] / RATE * 1000:.2f} ms, "
                  f"spread {r['spread']}), tone {r['tone_change_db']:+.2f} dB, peak {r['back_peak_dbfs']} dBFS")
        elif r is None:
            print(f'{name}: no click came back (is the strip unmuted on the record path? are the nodes linked?)')
    if 'insert_extra_samples' in results:
        print(f"insert on Virtual {first}: {results['insert_extra_samples']:+d} samples against plain Virtual {first + 1}")


if __name__ == '__main__':
    main()
