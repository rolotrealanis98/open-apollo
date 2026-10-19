"""Chain carrier for a profiled native plug-in: chain head + Mic Input Strip + the plug-in.

Builds the CREATE recipe (private rows, WRITE commands, resources, scratch) for one plug-in from
its runtime bundle's original descriptor and startup words (tools/plugin-mapper builds the bundle
on the user's machine). No devices, sockets or DLL loading here; the native backend owns I/O.
A successful build is an offline structural gate, not hardware qualification.
"""
from copy import deepcopy
from pathlib import Path
import hashlib
import struct

MODULES = Path('/lib/firmware/universal-audio/plugins/modules')
DSPDIR = '/lib/firmware/universal-audio/dsp'  # Console strip modules (oa-assets firmware)
MAX_COMMAND_WORDS = 1016
NONE = 0xffffffff
SCRATCH = (448, 192, 0)            # pools 1, 2, 3 (minimum the chain head and strip need)
STRIP_MODULES = ('0eb', '0da', '0a5', '0c2', '12b')

# Mic Input Strip sub-plug-in startup WRITEs (row offset, word, skip mask, values), as UA's
# Console sends them for a chain at 48 kHz; the same words driver/ua_strips.c writes for a plain strip.
MIC_STRIP = (
    (0, 0x00, 0x0f1fffec, (0x40000000, 1, 1, 0x800, 0x3a888889, 0xc0000000, 0xc0000000, 0x10, 0x4b,
                           0x41000000, 0x3ada740e, NONE, 1, NONE, 1)),
    (0, 0x26, 0x00000040, (0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f, 0x3f800000, 0xbc2e6fd3, 0xbbc96245,
                           0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f, 0x3f800000, 0xbc2e6fd3, 0xbbc96245, 1)),
    (3, 0x01, 0x00000040, (0x3cae6c74, 0xbf7ff125, 0x3f7ff892, 0x3909422f, 0x3f800000, 0xbc2e6fd3, 0xbbc96245,
                           0x3e3b3f3d, 0xbf7baf04, 0x3f7dd5f1, 0x3c20d8d2, 0x3f7ffcd7, 0xbdbc4dc9, 0xbd565d96)),
    (0, 0x10, 0x00000000, (0x3e8,)),
)


def L(r, w):
    return (r << 24) | w


def head_rows():
    """UA's AllocInfo rows for the chain head (row 0) and the Mic Input Strip (rows 1-9)."""
    return [
        (1, 10, NONE, 0),                                   # 0  chain head
        (1, 56, L(0, 6), 0),                                # 1  Mic m0 -> head[6]
        (2, 16, L(1, 54), 2), (1, 16, L(1, 55), 2),         # 2  m1, 3 m2
        (3, 16, L(1, 27), 0), (1, 4, L(1, 28), 0),          # 4  m3, 5 m4
        (3, 2048, L(1, 14), 2), (3, 2048, L(1, 15), 2),     # 6  c0, 7 c1
        (3, 2048, L(1, 6), 2), (3, 2048, L(1, 7), 2),       # 8  c2, 9 c3
    ]


def write(row, values):
    """One masked WRITE; values is {word_offset: uint32}, span <= 33."""
    positions = sorted(values)
    first, last = positions[0], positions[-1]
    if last - first > 32:
        raise ValueError('WRITE span exceeds mask capacity')
    mask = sum(1 << (p-first-1) for p in range(first+1, last+1) if p not in values)
    return [0x00140000 | (3+len(positions)), row << 24 | first, mask, *(values[p] for p in positions)]


def check(rows, cmd):
    """The driver's ua_inst_check_cmds(), offline."""
    links = {(r[2] >> 24, r[2] & 0xffffff) for r in rows[1:]}
    i = n = 0
    while i < len(cmd):
        ln, row, word, mask = cmd[i] & 0xffff, cmd[i + 1] >> 24, cmd[i + 1] & 0xffffff, cmd[i + 2]
        if cmd[i] >> 16 != 0x14 or not 4 <= ln <= len(cmd) - i:
            raise ValueError('bad WRITE header at %d' % i)
        if row >= len(rows) or word + ln - 3 + bin(mask).count('1') > rows[row][1]:
            raise ValueError('WRITE past row %d' % row)
        pos = word
        for _ in range(ln - 3):
            while pos > word and pos - word - 1 < 32 and (mask >> (pos - word - 1)) & 1:
                pos += 1
            if (row, pos) in links:
                raise ValueError('value on linked word %d of row %d' % (pos, row))
            pos += 1
        i += ln
        n += 1
    return n


def sha_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class CacheWrites:
    """Descriptor-bounded original cache events -> native WRITE words. No I/O. Preserves
    begin/data/end ordering; the native backend verifies receipt."""
    def __init__(self, rows, *, base_row=10, forbidden_refs=()):
        if type(rows) not in (tuple, list) or not 1 <= len(rows) <= 118:
            raise ValueError('effect private-row descriptors required')
        if type(base_row) is not int or not 0 <= base_row <= 127 or base_row+len(rows) > 128:
            raise ValueError('bounded effect row placement required')
        self.sizes = tuple(r[1] for r in rows)
        if any(type(n) is not int or not 0 < n < 1 << 24 for n in self.sizes):
            raise ValueError('positive bounded row sizes required')
        self.base_row = base_row
        self.forbidden = frozenset(forbidden_refs)
        for ref in self.forbidden:
            self.address(ref, allow_forbidden=True)

    def address(self, ref, *, allow_forbidden=False):
        if type(ref) is not int or not 0 <= ref <= 0xffffffff:
            raise ValueError('packed original reference required')
        row, offset = ref >> 24, ref & 0xffffff
        if row >= len(self.sizes) or offset >= self.sizes[row]:
            raise ValueError('original reference leaves owned descriptor')
        if not allow_forbidden and ref in self.forbidden:
            raise ValueError('control write targets DSP-owned readback')
        return self.base_row+row, offset

    def observations(self, shadow, refs):
        result = []
        for ref in sorted(set(refs)):
            row, offset = self.address(ref)
            if ref not in shadow or type(shadow[ref]) is not int or not 0 <= shadow[ref] <= 0xffffffff:
                raise ValueError('authoritative shadow value required')
            if result and result[-1][0] == row and result[-1][1]+len(result[-1][2]) == offset and len(result[-1][2]) < 64:
                r, start, values = result[-1]
                result[-1] = (r, start, values+(shadow[ref],))
            else:
                result.append((row, offset, (shadow[ref],)))
        if not result or len(result) > 32768:  # local safety bound (load audits large plug-ins in full)
            raise ValueError('bounded nonempty observation set required')
        return tuple(result)

    def encode(self, events):
        if type(events) not in (tuple, list) or len(events) > 10000:
            raise ValueError('bounded original event sequence required')
        commands = []
        atomic = None

        def emit(ref, values):
            if type(values) not in (tuple, list) or not 1 <= len(values) <= 4096:
                raise ValueError('bounded raw word array required')
            row, start = self.address(ref)
            lastrow, _ = self.address(ref+len(values)-1)
            if row != lastrow:
                raise ValueError('array crosses owned row')
            for i, value in enumerate(values):
                self.address(ref+i)
                if type(value) is not int or not 0 <= value <= 0xffffffff:
                    raise ValueError('raw uint32 required')
            for first in range(0, len(values), 33):
                commands.extend(write(row, {start+i: values[i] for i in range(first, min(first+33, len(values)))}))
        for event in events:
            if type(event) not in (tuple, list) or not event:
                raise ValueError('typed original event required')
            kind = event[0]
            if kind == 'begin' and len(event) == 4:
                _, start, end, value = event
                r, s = self.address(start)
                er, e = self.address(end)
                if atomic is not None or r != er or s >= e:
                    raise ValueError('invalid atomic begin')
                atomic = (start, end, value)
                emit(start, [value])
            elif kind == 'end' and len(event) == 3:
                _, end, value = event
                if atomic is None or atomic[1:] != (end, value):
                    raise ValueError('invalid atomic end')
                emit(end, [value])
                atomic = None
            elif kind == 'write' and len(event) == 3:
                emit(event[1], event[2])
            else:
                raise ValueError('unsupported runtime cache event')
        if atomic is not None:
            raise ValueError('unterminated atomic sequence')
        return tuple(commands)


def uint(value):
    if isinstance(value, str):
        value = int(value, 0)
    if type(value) is not int or not 0 <= value <= 0xffffffff:
        raise ValueError('uint32 required')
    return value


def shadow(values):
    return {uint(k): uint(v) for k, v in values.items()}


def blocks(commands):
    at = 0
    while at < len(commands):
        n = commands[at] & 0xffff
        if commands[at] >> 16 != 0x14 or n < 4 or at+n > len(commands):
            raise ValueError('complete original WRITE blocks required')
        yield tuple(commands[at:at+n])
        at += n


def chunks(groups):
    """Never divide an atomic group that fits one service call. A larger one (Ocean Way's 48,047-word
    kernel bracket) goes as consecutive calls in original order, end word last: its only atomicity is the
    plug-in's own start/end version words, and UA's host also issues it as hundreds of writes."""
    result, current = [], []
    for group in groups:
        for part in (blocks(group) if len(group) > MAX_COMMAND_WORDS else (group,)):
            if len(part) > MAX_COMMAND_WORDS:
                raise ValueError('one WRITE block exceeds SERVICE ABI')
            if len(current)+len(part) > MAX_COMMAND_WORDS:
                result.append(tuple(current))
                current = []
            current.extend(part)
    if current:
        result.append(tuple(current))
    return tuple(result)


def compact(words, base=10):
    # One sorted sweep per row: each group starts at the lowest remaining word and takes every
    # word up to first+32 (same output as repeatedly taking min(pending); O(n log n)).
    commands = []
    rows = {}
    for ref, value in words.items():
        rows.setdefault(ref >> 24, []).append((ref & 0xffffff, value))
    for row in sorted(rows):
        items = sorted(rows[row])
        i = 0
        while i < len(items):
            first, j = items[i][0], i
            while j < len(items) and items[j][0] <= first+32:
                j += 1
            commands.extend(write(base+row, dict(items[i:j])))
            i = j
    return tuple(commands)


def resource(record, store=MODULES):
    digest = record['sha256']
    if type(digest) is not str or len(digest) != 64:
        raise ValueError('resource content hash required')
    payload = (Path(store)/(digest+'.bill')).read_bytes()
    if hashlib.sha256(payload).hexdigest() != digest:
        raise ValueError('resource store hash mismatch')
    if len(payload) < 56 or len(payload) % 4:
        raise ValueError('bounded complete Bill record required')
    magic, ident, word2, size, words = struct.unpack_from('<5I', payload)
    kind, flags, arch = word2 & 255, (word2 >> 8) & 65535, word2 >> 24
    if (magic != 0x6c6c6942 or ident > 0xffffff or kind > 4 or arch not in (0, 2)
            or size != len(payload)-20 or not 0 < words <= len(payload)//4-13
            or (kind == 0 and flags) or (kind == 3 and flags)):
        raise ValueError('resource is outside qualified original loader contract')
    allocation = words if kind else words*2//3
    if not allocation or (kind == 0 and allocation & 1):
        raise ValueError('invalid original code allocation')
    if uint(record['id']) != ident or uint(record['architecture']) != arch:
        raise ValueError('descriptor resource identity differs from Bill')
    return dict(id=ident, architecture=arch, kind=kind, flags=flags,
                pool=1 if kind == 4 else kind, words=allocation,
                sha256=digest, bytes=len(payload))


class GeneratedOrdinaryCarrier:
    hardware_qualified = False
    hardware_qualified_inputs = ()
    sample_rate = 48000
    remote_reads = {}

    def __init__(self, descriptor, startup, *, input_index=2, record_budget=200,
                 module_store=MODULES, overload_ok=False, shared_cells_ok=False, replay=None):
        if type(input_index) is not int or not 0 <= input_index <= 3:
            raise ValueError('physical mono input0..3 required')
        self.descriptor = deepcopy(descriptor)
        self.name = descriptor['plugin']
        if not isinstance(self.name, str) or not self.name:
            raise ValueError('original plugin name required')
        self.Replay = replay
        # Configured channels only feed the original host-side coefficient code (stereo_channel_request
        # profiles); the CREATE recipe carries no channel field. 2 only for (2,2) IO = stereo carrier.
        channels = descriptor.get('configured_input_channels', 1)
        if descriptor.get('sample_rate', 48000) != 48000 or not (channels == 1 or (channels == 2 and tuple(descriptor.get('io', ())) == (2, 2))):
            raise ValueError('this proven carrier supports mono48k only')
        if tuple(descriptor.get('io', (1, 1))) not in ((1, 1), (2, 2)):
            raise ValueError('descriptor IO capabilities differ from proven mono normalization')
        self.input_index = input_index
        self.slot = f'/devices/0/inputs/{input_index}/effects/0'
        self.render_id = uint(descriptor['render_id'])
        if not 0 < self.render_id <= 0xffffff or uint(descriptor.get('flags', 0)) & 0x40:
            raise ValueError('ordinary licensed descriptor required')
        self.descriptor_latency_samples = uint(descriptor['latency_samples'])
        if type(record_budget) is not int or not 0 <= record_budget < 2048 or not (
                self.descriptor_latency_samples <= record_budget or overload_ok and self.descriptor_latency_samples <= 0x7fffffff):
            raise ValueError('known latency must fit explicit compensation budget')
        # overload_ok: latency above the largest delay-comp budget is reported, not buffered. The host sets no
        # record delay and flags the input overloaded (plugin_delay.plan_compensation), so strips never hold it.
        self.record_delay = max(record_budget-self.descriptor_latency_samples, 0)
        self.local_rows = tuple(tuple(map(uint, r)) for r in descriptor['rows'])
        if not 1 <= len(self.local_rows) <= 118:
            raise ValueError('effect rows must fit CREATE128 with carrier10')
        for i, row in enumerate(self.local_rows):
            if len(row) != 4:
                raise ValueError('four-field private row required')
            pool, size, link, flags = row
            if pool not in (1, 2, 3) or not 0 < size < 1 << 24 or flags not in (0, 2):
                raise ValueError('unsupported private allocation')
            if i == 0:
                if link != 0xffffffff or size < 5:  # root pool 1..3: driver memspec accepts any (TS Overdrive roots in pool 2)
                    raise ValueError('ordinary original root row required')
            elif link == 0xffffffff or link >> 24 >= len(self.local_rows) or (link & 0xffffff) >= self.local_rows[link >> 24][1]:
                raise ValueError('relocation target leaves descriptor')
        self.pointers = frozenset(row[2] for row in self.local_rows[1:])
        # shared_cells_ok: the original descriptor table may list two allocations on one pointer cell (AKG BX 20
        # rows 0x966/0x967/0x968/0x969 twice). ua_memspec_compile stores each row address in row order, so the
        # later one is the one the cell holds, as in UAD2System _transmitMemSpecUpdates; rows stay as the original.
        if len(self.pointers) != len(self.local_rows)-1 and not shared_cells_ok:
            raise ValueError('multiple allocations claim one pointer cell')
        self.codec = CacheWrites(self.local_rows, forbidden_refs=self.pointers)
        self.startup = deepcopy(startup)
        self.words = shadow(startup['shadow'])
        for ref in self.words:
            self.codec.address(ref)
        if startup.get('events') is not None and replay is not None:
            check_replay = replay()
            check_replay.apply(startup['events'])
            if check_replay.words != self.words:
                raise ValueError('original startup event replay differs from supplied shadow')
        self.resources = tuple(resource(r, module_store) for r in descriptor['resources'] if uint(r['architecture']) in (0, 2))
        if len({(r['id'], r['architecture']) for r in self.resources}) != len(self.resources):
            raise ValueError('duplicate original resource identity')
        scratch = tuple(map(uint, descriptor['scratch']))
        if len(scratch) != 3 or scratch[0] > 65535 or scratch[1] > 65535:
            raise ValueError('descriptor scratch exceeds CREATE ABI')
        self.scratch = tuple(max(SCRATCH[i], scratch[i]) for i in range(3))
        self._arguments = self._build()

    def _build(self):
        strip = []
        for row, word, mask, values in MIC_STRIP:
            strip += [0x00140000 | (len(values) + 3), L(1 + row, word), mask, *values]
        head = {0: 0x40000000, 1: 1, 2: 1, 4: 0x22, 5: 2, 7: 0x846, 9: self.render_id}
        commands = write(0, head)+strip+write(1, {16: self.record_delay})
        rows = head_rows()
        for i, (pool, size, link, flags) in enumerate(self.local_rows):
            target = L(0, 8) if i == 0 else L(10+(link >> 24), link & 0xffffff)
            rows.append((pool, size, target, flags))
        words = dict(self.words)
        words.update({0: 0x40000000, 1: 1, 2: 1, 4: words.get(4, 0) | 4})
        self.initialized_words = words
        commands.extend(compact(words))
        check(rows, commands)
        hashes = [sha_file(f'{DSPDIR}/{name}.bill') for name in STRIP_MODULES]
        hashes.extend(r['sha256'] for r in self.resources if r['sha256'] not in hashes)
        if not 1 <= len(hashes) <= 64:
            raise ValueError('combined carrier resources exceed CREATE64')
        self.startup_chunks = chunks(blocks(commands))
        return dict(render_id=self.render_id, resources=tuple(bytes.fromhex(h) for h in hashes),
                    private_rows=tuple(rows), commands=tuple(commands), scratch=self.scratch,
                    dsp=1, input_pair=(self.input_index, 32), chain=True)

    def composition_arguments(self):
        """Return the complete checked recipe for an enclosing staged chain."""
        return deepcopy(self._arguments)

    def create_arguments(self):
        if len(self._arguments['commands']) > MAX_COMMAND_WORDS:
            raise ValueError('startup requires inactive chunk initialization before ordinary activation')
        return self.composition_arguments()

    def stage_bundle(self):
        """Initialize inactive candidate, then caller swaps an existing owned anchor."""
        args = deepcopy(self._arguments)
        args['stage'] = True
        args['commands'] = self.startup_chunks[0]
        return dict(create_arguments=args, initialize_chunks=self.startup_chunks[1:],
                    requires_existing_routed_anchor=True,
                    startup_observations=self.startup_observations())

    def startup_observations(self):
        return self.codec.observations(self.initialized_words, self.initialized_words)

    def event_plan(self, events, *, phase, allowed_pairs, forbidden_refs=()):
        """Preserve original control event order and atomic groups, never infer reads."""
        if type(events) not in (tuple, list) or len(events) > 10000:
            raise ValueError('bounded original event sequence required')
        if phase not in ('startup', 'control'):
            raise ValueError('explicit startup/control ownership phase required')
        authorized = {tuple(map(uint, pair)) for pair in allowed_pairs}
        endpoints = {ref for pair in authorized for ref in pair}
        if any(ref in (0, 1, 2, 4) for ref in endpoints):
            raise ValueError('atomic endpoint collides with host-owned header')
        codec = CacheWrites(self.local_rows, forbidden_refs=self.pointers | frozenset(map(uint, forbidden_refs)))
        active, groups, current, normalized = None, [], [], []
        replay = self.Replay()
        for event in events:
            kind = event[0]
            if kind == 'timing':
                timing = self.Replay()
                timing.apply([event])
                expanded = [['write', ref, [value]] for ref, value in sorted(timing.words.items())]
            else:
                expanded = [event]
            if phase == 'control':
                for emitted in expanded:
                    if emitted[0] == 'write':
                        touched = set(range(emitted[1], emitted[1]+len(emitted[2])))
                        if touched.intersection((0, 1, 2, 4)):
                            raise ValueError('control write collides with host-owned header')
                        if active is None and touched.intersection(endpoints):
                            raise ValueError('plain control write targets phase-owned atomic endpoint')
            if kind == 'begin':
                pair = tuple(event[1:3])
                if active is not None or pair not in authorized:
                    raise ValueError('unowned or nested atomic pair')
                active = pair
            elif kind == 'write' and phase == 'control':
                if active is None and endpoints.intersection(range(event[1], event[1]+len(event[2]))):
                    raise ValueError('plain control write targets phase-owned atomic endpoint')
            normalized.extend(expanded)
            current.extend(expanded)
            if kind == 'end':
                if active is None or event[1] != active[1]:
                    raise ValueError('unmatched atomic end')
                active = None
            if active is None:
                encoded = codec.encode(current)
                if kind == 'end':
                    groups.append(encoded)
                else:
                    groups.extend(blocks(encoded))
                current = []
        if active is not None:
            raise ValueError('unterminated atomic group')
        replay.apply(normalized)
        encoded = chunks(groups)
        for group in encoded:
            check(self._arguments['private_rows'], group)
        return dict(writes=tuple(word for group in encoded for word in group), chunks=encoded,
                    shadow=replay.words, atomic_refs=tuple(sorted(endpoints)))
