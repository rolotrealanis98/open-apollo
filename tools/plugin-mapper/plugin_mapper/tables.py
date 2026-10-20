"""Learn a plug-in's control tables from its emulator, so the daemon can host it without one.

Every control change of a mapped plug-in runs the plug-in's own setter in the emulator. For a
plug-in whose words depend on few controls at a time, the same answers fit in tables. This learns
them from the emulator on this machine and writes `<bundle>.tables` (table_capture.py) only when
a differential gate passes: random control sequences, each from a freshly loaded plug-in, through
the emulator and the tables give the same events, values and texts.

1. Probe. Each change's events are a fixed list per control (transaction brackets and writes of
   word runs: blocks). Changing one control at a time from several bases shows which controls
   each block's words and each formatter text depend on.
2. Contexts. A block's stepped dependencies are its key; per key, at most one continuous control
   may matter (its axis, a row per value). Two continuous controls refuse.
3. Load-time state. Per control, a freshly loaded plug-in shows whether a block's words change
   when that control is set to the value it already has. Such a control reads as 'L' in that
   block's key until first set (the plug-in's copy of it is out of sync at load).
4. Record every key (each axis swept 0..maximum), every text, every control's read-back value.
5. Gate.

Refusals are ValueError (Refused) with the reason the mapper records in index.json.
"""
import gc
import itertools
import random
import time
from math import prod

from table_capture import TableCapture, key as table_key

STEPPED_MAX = 255           # a control with at most this maximum is enumerated, step by step
CONTEXTS_MAX = 4096         # keys per block or text
WORDS_MAX = 16 << 20        # table words (64 MB)


class Refused(ValueError):
    pass


def stepped(maximum):
    return maximum <= STEPPED_MAX


class Session:
    """One plug-in from load: its emulator and what the learner knows of its state."""

    def __init__(self, learner):
        self.learner = learner
        self.cap = learner.new()
        self.values = list(self.cap.read_values())
        self.touched = set()
        self.tx = []        # (start, begin count, end count), the load's first
        self._open = None
        self._transactions(self.cap.events)
        self.startup_tx = len(self.tx)

    def _transactions(self, events):
        for e in events:
            if e[0] == 'begin':
                self._open = (e[1], e[3])
            elif e[0] == 'end':
                if self._open is None:
                    raise Refused('transaction end without a begin')
                self.tx.append((self._open[0], self._open[1], e[2]))
                self._open = None

    def set(self, index, value):
        """{block: words} this change wrote."""
        cap = self.cap
        first = len(cap.events)
        cap.set_control(index, value)
        after = tuple(cap.read_values())
        moved = [i for i, (a, b) in enumerate(zip(self.values, after)) if a != b and i != index]
        if moved:
            raise Refused(f'setting control {index} moves control {moved[0]} (coupled parameters)')
        self.values = list(after)
        self.touched.add(index)
        self.learner.note_actual(index, value, after[index])
        events = cap.events[first:]
        self._transactions(events)
        return self.learner.parse(index, events)

    def text(self, index):
        return self.cap.format_parameter(index)['raw']

    def move(self, values):
        for i in self.learner.rng.sample(range(len(values)), len(values)):
            if self.values[i] != values[i]:
                self.set(i, values[i])


class Learner:
    def __init__(self, new_capture, *, seed=0, bases=4, log=None):
        self.new = new_capture
        self.rng = random.Random(seed)
        self.bases = bases
        self.log = log or (lambda text: None)
        self.sessions = []
        self.templates = {}     # control -> its events, writes as (ref, len)
        self.actual = {}        # control -> {requested: read back}
        first = self.session()
        self.main = first
        self.maxima = tuple(first.cap.maxima)
        self.defaults = tuple(first.cap.defaults_values)
        self.n = len(self.maxima)

    def session(self):
        s = Session(self)
        self.sessions.append(s)
        return s

    def note_actual(self, index, value, actual):
        if self.actual.setdefault(index, {}).setdefault(value, actual) != actual:
            raise Refused(f'control {index} reads back differently for the same value {value}')

    def parse(self, index, events):
        template, contents = [], {}
        for e in events:
            if e[0] in ('begin', 'end'):
                template.append((e[0], *e[1:3 if e[0] == 'begin' else 2]))
            elif e[0] == 'write':
                block = (e[1], len(e[2]))
                words = tuple(e[2])
                if contents.setdefault(block, words) != words:
                    raise Refused(f'control {index} writes {e[1]:#x} twice with different words')
                template.append(('write', *block))
            else:
                raise Refused(f'control {index} emits {e[0]!r} events')
        template = tuple(template)
        if self.templates.setdefault(index, template) != template:
            raise Refused(f'control {index} writes different words from one change to another')
        return contents

    # ── helpers ──

    def random_values(self):
        return [self.rng.randint(0, m) for m in self.maxima]

    def alternatives(self, c, current):
        m = self.maxima[c]
        if m <= 3:
            return [v for v in range(m + 1) if v != current]
        return [v for v in dict.fromkeys((0, m, self.rng.randint(1, m - 1))) if v != current]

    def writers(self, block):
        return [i for i in range(self.n) if ('write', *block) in self.templates[i]]

    def emit(self, session, block, avoid=()):
        """The block's words now, written by setting one of its writers to its current value."""
        for w in self.writers(block):
            if w not in avoid:
                return session.set(w, session.values[w])
        return None

    # ── 1. probe ──

    def probe(self):
        s = self.main
        for i in range(self.n):  # every control once: templates, and no control left at its load-time copy
            s.set(i, s.values[i])
        self.blocks = sorted({(t[1], t[2]) for tpl in self.templates.values() for t in tpl if t[0] == 'write'})
        self.deps = {b: set() for b in self.blocks}
        self.text_deps = [{j} for j in range(self.n)]
        # The load state, both ends of every control (a knob may be flat over most of its range), random states.
        for base in [list(s.values), [0] * self.n, list(self.maxima)] + [self.random_values() for _ in range(self.bases)]:
            s.move(base)
            ref = {}
            for i in range(self.n):
                ref.update(s.set(i, base[i]))
            ref_texts = [s.text(j) for j in range(self.n)]
            for c in range(self.n):
                for a in self.alternatives(c, base[c]):
                    seen = s.set(c, a)
                    for b in self.blocks:
                        if b not in seen:
                            got = self.emit(s, b, avoid=(c,))
                            if got:
                                seen = {**got, **seen}
                    for b, words in seen.items():
                        if words != ref[b]:
                            self.deps[b].add(c)
                    for j in range(self.n):
                        if s.text(j) != ref_texts[j]:
                            self.text_deps[j].add(c)
                    back = s.set(c, base[c])
                    if any(back[b] != ref[b] for b in back):
                        raise Refused(f'control {c}: words differ after setting it back (history-dependent)')
        self.log(f'probe: {len(self.blocks)} blocks, dependencies '
                 + '; '.join(f'{b[0]:#x}:{sorted(d)}' for b, d in zip(self.blocks, map(self.deps.get, self.blocks))))

    # ── 2. contexts and axes ──

    def contexts(self):
        self.context, self.axes = {}, {}
        for b in self.blocks:
            ctx = sorted(c for c in self.deps[b] if stepped(self.maxima[c]))
            continuous = sorted(c for c in self.deps[b] if not stepped(self.maxima[c]))
            self.context[b] = ctx
            keys = list(itertools.product(*(range(self.maxima[c] + 1) for c in ctx)))
            if len(keys) > CONTEXTS_MAX:
                raise Refused(f'block {b[0]:#x} depends on {len(keys)} stepped combinations')
            axes = self.axes[b] = {}
            for k in keys:
                if not continuous:
                    axes[k] = None
                    continue
                s = self.main
                base = self.random_values()
                for c, v in zip(ctx, k):
                    base[c] = v
                s.move(base)
                ref = self.emit(s, b)[b]
                active = set()
                for c in continuous:
                    for a in self.alternatives(c, base[c]):
                        got = s.set(c, a)
                        words = got[b] if b in got else self.emit(s, b, avoid=(c,))[b]
                        if words != ref:
                            active.add(c)
                    s.set(c, base[c])
                if len(active) > 1:
                    raise Refused(f'block {b[0]:#x} depends on two continuous controls {sorted(active)}')
                axis = active.pop() if active else None
                if axis is not None and axis not in self.writers(b):
                    raise Refused(f'block {b[0]:#x} depends on control {axis}, which does not write it')
                axes[k] = axis
        for j, deps in enumerate(self.text_deps):
            if prod(self.maxima[c] + 1 for c in deps - {j} if stepped(self.maxima[c])) > CONTEXTS_MAX:
                raise Refused(f'text of parameter {j} depends on too many stepped combinations')
            others = [c for c in deps if c != j and not stepped(self.maxima[c])]
            if others:
                raise Refused(f'text of parameter {j} depends on continuous control {others[0]}')

    # ── 3. load-time state ──

    def load_state(self):
        self.lazy = {b: set() for b in self.blocks}
        for c in range(self.n):
            blocks = [b for b in self.blocks if any(w != c for w in self.writers(b))]
            if not blocks:
                continue
            s = self.session()
            before = {}
            for _ in range(2):  # the first pass sets the other writers: only c differs afterwards
                for b in blocks:
                    before[b] = self.emit(s, b, avoid=(c,))[b]
            s.set(c, s.values[c])
            for b in blocks:
                if self.emit(s, b, avoid=(c,))[b] != before[b]:
                    if not stepped(self.maxima[c]):
                        raise Refused(f'block {b[0]:#x} keeps load-time state of continuous control {c}')
                    self.lazy[b].add(c)
                    if c not in self.context[b]:  # its value does not matter, only whether it was set
                        self.context[b] = sorted(self.context[b] + [c])
                        self.axes[b] = {k2: self.axes[b][tuple(v for cc, v in zip(self.context[b], k2) if cc != c)]
                                        for k2 in itertools.product(*(range(self.maxima[cc] + 1)
                                                                      for cc in self.context[b]))}
            self.sessions.remove(s)
            del s
            gc.collect()  # an emulator is a reference cycle holding its memory
        found = {f'{b[0]:#x}': sorted(v) for b, v in self.lazy.items() if v}
        if found:
            self.log(f'load-time state: {found}')

    # ── 4. record ──

    def key_of(self, block, s):
        lazy = self.lazy[block]
        return tuple('L' if c in lazy and c not in s.touched else s.values[c] for c in self.context[block])

    def record(self, s, contents):
        for b, words in contents.items():
            k = self.key_of(b, s)
            axis = self.axis_at(b, k)
            row = s.values[axis] if axis is not None else 0
            entry = self.tables[b].setdefault(k, {})
            if entry.setdefault(row, words) != words:
                raise Refused(f'block {b[0]:#x} words differ at the same key {k}')

    def reachable(self, control):
        """The values a control reads back (all of them until every request was seen)."""
        seen = self.actual.get(control, {})
        return set(seen.values()) if len(seen) == self.maxima[control] + 1 else set(range(self.maxima[control] + 1))

    def axis_at(self, b, k):
        return self.axes[b][tuple(self.defaults[c] if v == 'L' else v for c, v in zip(self.context[b], k))]

    def reach(self, assignment):
        """A session whose state has these {control: value or 'L'}; 'L' needs a control not set
        since load."""
        s = self.main
        if any(v == 'L' and c in s.touched for c, v in assignment.items()):
            s = next((x for x in self.sessions[1:]
                      if not any(v == 'L' and c in x.touched for c, v in assignment.items())), None) or self.session()
        for c, v in assignment.items():
            if v != 'L' and (s.values[c] != v or c not in s.touched):
                self.record(s, s.set(c, v))
        return s

    def sweep(self, s, axis, texts=True):
        started = time.monotonic()
        for x in range(self.maxima[axis] + 1):
            self.record(s, s.set(axis, x))
            if texts:
                self.record_text(s, axis)
            if x and x % 8192 == 0:
                self.log(f'  control {axis}: {x}/{self.maxima[axis]} ({time.monotonic() - started:.0f} s)')

    def text_key(self, j, s):
        return tuple(s.values[c] for c in sorted(self.text_deps[j] - {j}))

    def record_text(self, s, j):
        entry = self.text_tables[j].setdefault(self.text_key(j, s), {})
        text = s.text(j)
        if entry.setdefault(s.values[j], text) != text:
            raise Refused(f'text of parameter {j} differs at the same value')

    def record_all(self):
        self.tables = {b: {} for b in self.blocks}
        self.text_tables = [{} for _ in range(self.n)]
        for b in self.blocks:
            domains = [list(range(self.maxima[c] + 1)) + (['L'] if c in self.lazy[b] else []) for c in self.context[b]]
            for k in itertools.product(*domains):
                held = {c for c, v in zip(self.context[b], k) if v == 'L'}
                if all(w in held for w in self.writers(b)):
                    continue  # never written while those controls keep their load-time state
                axis = self.axis_at(b, k)
                if set(self.tables[b].get(k, {})) >= (self.reachable(axis) if axis is not None else {0}):
                    continue
                s = self.reach(dict(zip(self.context[b], k)))
                if axis is None:
                    self.record(s, self.emit(s, b, avoid=held))
                else:
                    self.log(f'block {b[0]:#x} at {table_key(k) or "any"}: sweeping control {axis}')
                    self.sweep(s, axis)
        for j in range(self.n):
            ctx = sorted(self.text_deps[j] - {j})
            for k in itertools.product(*(range(self.maxima[c] + 1) for c in ctx)):
                if set(self.text_tables[j].get(k, {})) >= self.reachable(j):
                    continue
                s = self.reach(dict(zip(ctx, k)))
                if not stepped(self.maxima[j]):
                    self.log(f'text of parameter {j} at {table_key(k) or "any"}: sweeping')
                self.sweep(s, j)

    # ── output ──

    def counter(self):
        """How the begin/end count moves, checked against every session's transactions."""
        if not any(len(s.tx) > s.startup_tx for s in self.sessions):
            return dict(mode='none')
        if any(begin != end for s in self.sessions for _, begin, end in s.tx):
            raise Refused('transaction end differs from its begin count')
        for mode in ('global', 'per_start'):
            seed, fits = {}, True
            for s in self.sessions:
                last = {}
                for start, begin, _ in s.tx[:s.startup_tx]:
                    last[start if mode == 'per_start' else None] = begin
                for group, value in last.items():
                    fits &= seed.setdefault(group, value) == value
                for start, begin, _ in s.tx[s.startup_tx:]:
                    group = start if mode == 'per_start' else None
                    if group not in last:  # first transaction of its kind after load
                        last[group] = (begin - 1) & 0xffffffff
                        fits &= seed.setdefault(group, last[group]) == last[group]
                    fits &= begin == (last[group] + 1) & 0xffffffff
                    last[group] = begin
            if fits:
                if mode == 'global':
                    return dict(mode='global', last=seed[None])
                return dict(mode='per_start', last={str(k): v for k, v in seed.items()})
        raise Refused('transaction count follows neither one global count nor one per start word')

    def build(self):
        words, offsets = [], {}

        def place(seq):
            seq = tuple(seq)
            if seq not in offsets:
                offsets[seq] = len(words)
                words.extend(seq)
                if len(words) > WORDS_MAX:
                    raise Refused(f'tables over {WORDS_MAX * 4 >> 20} MB')
            return offsets[seq]

        index = {b: i for i, b in enumerate(self.blocks)}
        blocks = []
        for b in self.blocks:
            entries = {}
            for k, rows in self.tables[b].items():
                axis = self.axis_at(b, k)
                if axis is None or len(set(rows.values())) == 1:
                    entries[table_key(k)] = [-1, place(next(iter(rows.values())))]
                else:
                    if not set(rows) >= self.reachable(axis):
                        raise Refused(f'block {b[0]:#x} at {k}: sweep incomplete')
                    unused = (0,) * b[1]  # a value the control never reads back (its setter clamps)
                    entries[table_key(k)] = [axis, place(w for x in range(self.maxima[axis] + 1)
                                                         for w in rows.get(x, unused))]
            blocks.append(dict(ref=b[0], len=b[1], context=self.context[b], lazy=sorted(self.lazy[b]), entries=entries))
        controls = []
        for i in range(self.n):
            seen = self.actual.get(i, {})
            if len(seen) != self.maxima[i] + 1:
                raise Refused(f'control {i}: read-back not seen for every value')
            actual = None if all(seen[x] == x for x in seen) else place(seen[x] for x in range(self.maxima[i] + 1))
            events = [list(t) if t[0] != 'write' else ['write', t[1], t[2], index[(t[1], t[2])]]
                      for t in self.templates[i]]
            controls.append(dict(events=events, actual=actual))
        texts = []
        for j in range(self.n):
            entries = {}
            for k, values in self.text_tables[j].items():
                runs = []
                if not set(values) >= self.reachable(j):
                    raise Refused(f'text of parameter {j} at {k}: sweep incomplete')
                for x in sorted(values):  # a value never read back takes the run before it
                    if not runs or runs[-1][1] != values[x]:
                        runs.append([x, values[x]])
                entries[table_key(k)] = runs
            texts.append(dict(context=sorted(self.text_deps[j] - {j}), entries=entries))
        header = dict(maxima=list(self.maxima), defaults=list(self.defaults), counter=self.counter(),
                      controls=controls, blocks=blocks, texts=texts)
        return header, words

    def learn(self):
        started = time.monotonic()
        self.probe()
        self.contexts()
        self.load_state()
        self.record_all()
        tables = self.build()
        self.log(f'recorded {len(tables[1]) * 4 / 1e6:.1f} MB of tables in {time.monotonic() - started:.0f} s')
        return tables


# ── 5. the gate ──

def gate(new_capture, tables, *, changes=3000, seed=1, log=None):
    """Random control sequences, one per control (each from a freshly loaded plug-in, starting
    with that control), through the emulator and the tables: events, values and texts must be
    identical. Returns the gate record; raises Refused on the first difference."""
    log = log or (lambda text: None)
    rng = random.Random(seed)
    header = tables[0]
    maxima = tuple(header['maxima'])
    n = len(maxima)
    per = max(50, changes // n)
    done = 0
    for first in range(n):
        cap, tab = new_capture(), TableCapture(tables)
        if tuple(cap.defaults_values) != tab.defaults_values or tuple(cap.read_values()) != tab.read_values():
            raise Refused('gate: load-time values differ')
        for j in range(n):
            if cap.format_parameter(j) != tab.format_parameter(j):
                raise Refused(f'gate: load-time text of parameter {j} differs')
        for step in range(per):
            i = first if step == 0 else rng.randrange(n)
            current = cap.read_values()[i]
            v = rng.choice((0, maxima[i], current, header['defaults'][i], rng.randint(0, maxima[i]),
                            rng.randint(0, maxima[i])))
            a, b = len(cap.events), len(tab.events)
            cap.set_control(i, v)
            tab.set_control(i, v)
            where = f'sequence {first}, change {step} (control {i} = {v})'
            if cap.events[a:] != tab.events[b:]:
                raise Refused(f'gate: events differ at {where}')
            if tuple(cap.read_values()) != tab.read_values():
                raise Refused(f'gate: values differ at {where}')
            for j in {i, rng.randrange(n)}:
                if cap.format_parameter(j) != tab.format_parameter(j):
                    raise Refused(f'gate: text of parameter {j} differs at {where}')
            done += 1
        for j in range(n):
            if cap.format_parameter(j) != tab.format_parameter(j):
                raise Refused(f'gate: text of parameter {j} differs after sequence {first}')
        log(f'gate: sequence {first + 1}/{n} identical ({done} changes)')
        del cap
        gc.collect()
    return dict(status='pass', sequences=n, changes=done, seed=seed)


def learn_and_gate(new_capture, *, seed=0, changes=3000, log=None):
    """(header, words) with a passing gate record, or Refused."""
    tables = Learner(new_capture, seed=seed, log=log).learn()
    tables[0]['gate'] = gate(new_capture, tables, changes=changes, seed=seed + 1, log=log)
    return tables


def learn_bundle(bundle_path, *, changes=3000, seed=0, log=None, extra=None):
    """Learn, gate and write `<bundle>.tables` for one runtime bundle with the emulator (venv,
    the bundle's DLL, OA_UAD_SDK). Returns the index record: status 'pass' or 'refused' + reason."""
    import hashlib
    import json
    from pathlib import Path
    from generic_capture import GenericCapture
    from table_capture import sidecar_path, write
    raw = Path(bundle_path).read_bytes()
    data = json.loads(raw)
    sha = hashlib.sha256(raw).hexdigest()

    def new():
        cap = GenericCapture(data['profile'], auto_start=False)
        cap.start()
        return cap
    started = time.monotonic()
    record = dict(bundle_sha256=sha)
    try:
        header, words = learn_and_gate(new, changes=changes, seed=seed, log=log)
    except Exception as error:  # Refused, or the emulator failing a probe: either way no tables
        reason = str(error) if isinstance(error, Refused) else f'{type(error).__name__}: {error}'
        return dict(record, status='refused', reason=reason, seconds=round(time.monotonic() - started))
    header.update(extra or {}, bundle_sha256=sha, name=data['name'], layout=data.get('layout'))
    write(sidecar_path(bundle_path), header, words)
    return dict(record, status='pass', seconds=round(time.monotonic() - started),
                bytes=sidecar_path(bundle_path).stat().st_size, changes=header['gate']['changes'])
