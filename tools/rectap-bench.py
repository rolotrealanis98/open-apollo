"""REC/MON tap bench: does bit 0x4 of a block's word 4 pick the record tap?

Builds a 1176 Rev A chain on input 1 (DSP input 0) with the daemon's own composer and
driver client, and records input 1 (ALSA ch 0) against MON L/R (ch 14/15) at each step:

  B0  plain strip
  C   chain as composed today (flag on the 1176), Input 0.8
  L   live SERVICE flip: flag on Mic m0 (row 1 word 4), cleared on the 1176
  L2  live flip back (= C)
  B1  chain destroyed (plain strip restored)
  F   fresh chain built with the flag on Mic m0 and cleared on the 1176, Input 0.8
  B2  chain destroyed

Usage: ~/.local/share/open-apollo/plugin-venv/bin/python3 tools/rectap-bench.py mixer-engine OUT_DIR [--offline]
OUT_DIR gets the WAVs and results.json; keep it outside the repository.
The daemon and PipeWire must be stopped for a run (arecord needs hw:x4).
"""
import copy, dataclasses, json, os, subprocess, sys, time
from pathlib import Path

ME, OUT = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
OFFLINE = '--offline' in sys.argv
sys.path.insert(0, str(ME))
os.chdir(ME)
OUT.mkdir(parents=True, exist_ok=True)

from profiled_bundle_registry import BundleWatcher
from native_chain_composer import compose, _write, _shadow
from native_carrier import check

NAME, INPUT = 'UAD UA 1176 Rev A', 0
FIELD, VALUE = 'parameters/0/NormalizedValue', 0.8
log_lines = []


def log(*a):
    line = ' '.join(str(x) for x in a)
    print(line, flush=True)
    log_lines.append(line)


providers = [p for p in BundleWatcher(inputs=(INPUT,)).poll()
             if p.name == NAME and p.input_index == INPUT and type(p).__name__ == 'ProfiledCarrierProvider']
assert len(providers) == 1, providers
template = providers[0]


def build(delays, moved):
    """(composed chain, create arguments, staged chunks); moved=True puts the tap flag on Mic m0."""
    source = copy.deepcopy(template)
    source.reset()
    composed = compose([source], channels=(INPUT,), record_delay_samples=delays[0],
                       monitor_delay_samples=delays[1], active_capacity=delays[2])
    base = composed.effects[0].base_row
    flags = _shadow(composed.arguments['commands'])[base, 4]
    if moved:
        args = copy.deepcopy(composed.arguments)
        args['commands'] = tuple(args['commands']) + tuple(_write(1, {4: 4})) + tuple(_write(base, {4: flags & ~4}))
        check(list(args['private_rows']), list(args['commands']))
        composed = dataclasses.replace(composed, arguments=args)
    if composed.needs_staging:
        stage = composed.stage_bundle()
        arguments = dict(stage['create_arguments'], chain=True)
        chunks = stage['initialize_chunks']
    else:
        arguments = dict(composed.create_arguments(), chain=True)
        chunks = ()
    return composed, arguments, chunks, base, flags


def record(tag, seconds=4):
    if OFFLINE:
        return None
    path = OUT / f'{tag}.wav'
    subprocess.run(['arecord', '-q', '-D', 'hw:CARD=x4,DEV=0', '-c', '22', '-f', 'S32_LE', '-r', '48000',
                    '-d', str(seconds), str(path)], check=True, timeout=seconds + 10)
    import numpy as np
    raw = path.read_bytes()
    i = raw.find(b'data') + 8
    a = np.frombuffer(raw[i:], '<i4').astype(np.float64) / 2.0 ** 31
    a = a[: len(a) // 22 * 22].reshape(-1, 22)[48000:]

    def db(x):
        r = float(np.sqrt(np.mean(x ** 2)))
        return 20 * np.log10(r) if r else float('-inf')

    rec, ml, mr = db(a[:, 0]), db(a[:, 14]), db(a[:, 15])
    result = dict(step=tag, rec_dbfs=round(rec, 2), mon_l_dbfs=round(ml, 2), mon_r_dbfs=round(mr, 2),
                  mon_minus_rec_db=round((ml + mr) / 2 - rec, 2))
    log('MEASURE', json.dumps(result))
    return result


def read_words(driver, native, row, word, count=1):
    return driver.service(native, read=(row, word, count))


def start_chain(driver, arguments, chunks):
    native = driver.create(**arguments)
    for writes in chunks:
        driver.service(native, writes=writes)
    if arguments.get('stage'):
        driver.start(native)
    return native


def set_input(driver, native, composed):
    plan = composed.effects[0].plan_control(FIELD, VALUE)
    driver.service(native, writes=plan.writes)
    log(f'1176 {FIELD} = {VALUE}: {len(plan.writes)} words written')


def main():
    if OFFLINE:
        delays = (200, 0, 256)
    else:
        from plugin_driver import PluginDriver
        fd = os.open('/dev/ua_apollo0', os.O_RDWR)
        driver = PluginDriver(fd)
        strips = driver.strip_delays()
        delays = (strips.record[INPUT], strips.monitor[INPUT], strips.capacity[INPUT])
        log('retained strips:', strips)
    c_comp, c_args, c_chunks, base, flags = build(delays, moved=False)
    f_comp, f_args, f_chunks, _, _ = build(delays, moved=True)
    log(f'delays record/monitor/capacity {delays}; effect base row {base}; composed effect word4 0x{flags:x}; '
        f'staged={c_comp.needs_staging}; commands {len(c_args["commands"])} / {len(f_args["commands"])} words')
    sc, sf = _shadow(c_comp.arguments['commands']), _shadow(f_comp.arguments['commands'])
    log('recipe differences (row, word): control -> moved:',
        {k: (hex(sc.get(k, 0)), hex(sf.get(k, 0))) for k in sorted(set(sc) | set(sf)) if sc.get(k) != sf.get(k)})
    if OFFLINE:
        return
    results = [record('B0_plain')]
    native = None
    try:
        native = start_chain(driver, c_args, c_chunks)
        log('C created', native, 'm0[4], eff[4] =', read_words(driver, native, 1, 4), read_words(driver, native, base, 4))
        set_input(driver, native, c_comp)
        time.sleep(1)
        results.append(record('C_control'))

        driver.service(native, writes=tuple(_write(1, {4: 4})) + tuple(_write(base, {4: flags & ~4})))
        log('L flipped: m0[4], eff[4] =', read_words(driver, native, 1, 4), read_words(driver, native, base, 4))
        time.sleep(1)
        results.append(record('L_live_flip'))

        driver.service(native, writes=tuple(_write(1, {4: 0})) + tuple(_write(base, {4: flags})))
        log('L2 flipped back: m0[4], eff[4] =', read_words(driver, native, 1, 4), read_words(driver, native, base, 4))
        time.sleep(1)
        results.append(record('L2_flip_back'))

        driver.destroy(native)
        native = None
        time.sleep(1)
        results.append(record('B1_plain'))

        native = start_chain(driver, f_args, f_chunks)
        log('F created', native, 'm0[4], eff[4] =', read_words(driver, native, 1, 4), read_words(driver, native, base, 4))
        set_input(driver, native, f_comp)
        time.sleep(1)
        results.append(record('F_fresh_moved'))

        driver.destroy(native)
        native = None
        time.sleep(1)
        results.append(record('B2_plain'))
    finally:
        if native is not None:
            log('cleanup: closing the fd restores the plain strip')
        os.close(fd)
        (OUT / 'results.json').write_text(json.dumps(dict(results=results, log=log_lines), indent=1))


main()
