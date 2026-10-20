"""Insert-chain composition for profiled native plug-ins, no device I/O.

One Mic Input Strip and 1..4 native effects, shared head/resource ownership.
Runtime must authorize every render_ids entry, then verify physical activation.
"""
from copy import deepcopy
from dataclasses import dataclass
from plugin_native import ControlPlan
from native_carrier import L, NONE, check, write as _write
from native_profiled_carrier_provider import ProfiledCarrierProvider

# Bit 0x4 of a block's word 4 is the record tap (HARDWARE-SESSIONS.md, "record tap flag"):
# UAD REC taps after the last effect, UAD MON after the Mic Input Strip m0 (row 1).
RECORD_TAP = 0x4
MON_TAP = (1, 4)


def _tap(flags, last, record_pre_effects):
    return flags | RECORD_TAP if last and not record_pre_effects else flags & ~RECORD_TAP


def _blocks(commands):
    i = 0
    while i < len(commands):
        n = commands[i] & 0xffff
        if commands[i] >> 16 != 0x14 or n < 4 or i + n > len(commands):
            raise ValueError('bounded original WRITE blocks required')
        yield tuple(commands[i:i+n])
        i += n


def _writes(words):
    result = []
    for row in sorted({r for r, _ in words}):
        pending = {w: v for (r, w), v in words.items() if r == row}
        while pending:
            first = min(pending)
            group = {w: pending.pop(w) for w in sorted(pending) if w <= first + 32}
            result.extend(_write(row, group))
    return tuple(result)


def _chunks(commands, maximum=1016):
    output=[];current=[]
    for block in _blocks(commands):
        if len(block)>maximum:raise ValueError('single original WRITE block exceeds CREATE ABI')
        if current and len(current)+len(block)>maximum:
            output.append(tuple(current));current=[]
        current.extend(block)
    if current:output.append(tuple(current))
    if not output:raise ValueError('complete composed initialization required')
    return tuple(output)


def _shadow(commands):
    result = {}
    for block in _blocks(commands):
        row, start, mask = block[1] >> 24, block[1] & 0xffffff, block[2]
        pos = start
        for value in block[3:]:
            while 0 <= pos-start-1 < 32 and mask >> (pos-start-1) & 1:
                pos += 1
            result[row, pos] = value
            pos += 1
    return result


@dataclass(frozen=True)
class _SourcePlan:
    owner: object
    plan: ControlPlan


class ComposedEffect:
    """A logical slot whose controls address one effect in a physical chain."""
    hardware_qualified = False

    def __init__(self, provider, *, base_row, row_count, channels, slot, record_delay_samples):
        self.provider = provider
        self.base_row, self.row_count = base_row, row_count
        self.channels = channels
        self.slot = f'/devices/0/inputs/{channels[0]}/effects/{slot}'
        self.name, self.sample_rate = provider.name, provider.sample_rate
        self.record_delay_samples = record_delay_samples
        self.remote_reads = {key: (self._row(row), word)
                             for key, (row, word) in provider.remote_reads.items()}

    def _row(self, row):
        if not 10 <= row < 10 + self.row_count:
            raise ValueError('effect reference leaves its source private rows')
        return self.base_row + row - 10

    def _commands(self, commands):
        result = []
        for block in _blocks(commands):
            result.extend((block[0], self._row(block[1] >> 24) << 24 | (block[1] & 0xffffff), *block[2:]))
        return tuple(result)

    def _observations(self, observations):
        result = []
        for row, word, values in observations:
            if row == 1 and word == 16 and len(values) == 1:
                result.append((1, 16, (self.record_delay_samples,)))
            elif row >= 10:
                result.append((self._row(row), word, values))
            elif row in (0, 1) and word == 1 and values == (2, 2):
                result.append((row, word, values))
            else:
                raise ValueError('unexpected carrier observation')
        return tuple(result)

    def _plan(self, source):
        if type(source) is not ControlPlan:
            raise ValueError('source control plan required')
        return ControlPlan(self._commands(source.writes), self._observations(source.observations),
                           deepcopy(source.values), _SourcePlan(self, source))

    def _source(self, plan):
        if type(plan) is not ControlPlan or type(plan.state) is not _SourcePlan or plan.state.owner is not self:
            raise ValueError('plan belongs to another composed effect')
        source = plan.state.plan
        expected = self._plan(source)
        if plan.writes != expected.writes or plan.observations != expected.observations or plan.values != expected.values:
            raise ValueError('composed plan changed after preparation')
        return source

    def initial(self, lease):
        return self.provider.initial(lease)

    def observations(self):
        return self._observations(self.provider.observations())

    def snapshot(self, field):
        return self._plan(self.provider.snapshot(field))

    def plan_control(self, field, value):
        return self._plan(self.provider.plan_control(field, value))

    def plan_restore(self, prior, proposal):
        return self._plan(self.provider.plan_restore(self._source(prior), self._source(proposal)))

    def accept(self, plan):
        self.provider.accept(self._source(plan))

    @property
    def current_values(self):  # only where the provider has it
        return getattr(self.provider, 'current_values', None)


@dataclass(frozen=True)
class ComposedChain:
    arguments: dict
    effects: tuple
    render_ids: tuple
    observations: tuple
    hardware_qualified: bool = False
    taps: tuple = ()  # (effect base row, its word 4 without the tap)

    @property
    def needs_staging(self):
        return len(self.arguments['commands']) > 1016

    def create_arguments(self):
        if self.needs_staging:raise ValueError('composed startup requires staged initialization')
        return deepcopy(self.arguments)

    def stage_bundle(self):
        chunks=_chunks(self.arguments['commands'])
        arguments=deepcopy(self.arguments)
        arguments.update(stage=True,commands=chunks[0])
        return dict(create_arguments=arguments,initialize_chunks=chunks[1:])

    def record_tap_plan(self, record_pre_effects):
        """The live UAD REC (False) / MON (True) flip: word 4 of m0 and of each effect, all
        outside the chain directory, so one SERVICE writes it on the running chain."""
        words = {MON_TAP: RECORD_TAP if record_pre_effects else 0}
        for index, (row, flags) in enumerate(self.taps):
            words[row, 4] = _tap(flags, index == len(self.taps)-1, record_pre_effects)
        return ControlPlan(_writes(words), tuple((row, word, (value,)) for (row, word), value in sorted(words.items())),
                           {'RecordPreEffects': record_pre_effects}, None)


def compose(providers, *, channels, record_delay_samples=200,
            monitor_delay_samples=0, active_capacity=256, slots=None, record_pre_effects=False):
    """Compose ordered logical slots; the caller supplies already computed delay.

    All source providers are distinct retained state owners. No reset occurs.
    `slots` allows gaps while preserving the physical processing order.
    `record_pre_effects` is UAD MON: the record taps the Mic strip, the monitor stays wet.
    """
    if type(providers) not in (tuple, list) or not 1 <= len(providers) <= 4:
        raise ValueError('one to four effects required')
    if type(channels) is not tuple or channels not in ((0,), (1,), (2,), (3,), (0, 1), (2, 3)):
        raise ValueError('explicit analog mono or stereo channels required')
    if any(type(c) is not int for c in channels):
        raise ValueError('integer channel identity required')
    if type(record_delay_samples) is not int or not 0 <= record_delay_samples < 2048:
        raise ValueError('record delay must fit the retained 2048-word carrier buffers')
    if type(monitor_delay_samples) is not int or not 0 <= monitor_delay_samples < 2048:
        raise ValueError('monitor delay must fit the retained 2048-word carrier buffers')
    if (type(active_capacity) is not int or not 0 <= active_capacity <= 2048
            or (active_capacity and active_capacity & (active_capacity - 1))):
        raise ValueError('active capacity must be Off or a power of two up to 2048')
    if type(record_pre_effects) is not bool:
        raise ValueError('explicit UAD REC / MON required')
    slots = tuple(range(len(providers))) if slots is None else slots
    if (type(slots) is not tuple or len(slots) != len(providers)
            or any(type(s) is not int or not 0 <= s < 4 for s in slots)
            or tuple(sorted(set(slots))) != slots):
        raise ValueError('strictly increasing logical slots 0..3 required')
    underlying = list(providers)
    if any(not isinstance(p, ProfiledCarrierProvider) for p in underlying) or len({id(p) for p in underlying}) != len(providers):
        raise ValueError('distinct evidence-backed provider state owners required')
    if any(p.input_index != channels[0] or p.sample_rate != 48000 for p in underlying):
        raise ValueError('all providers must match the first input and 48k carrier')
    # A profiled carrier may require inactive chunk initialization. Its complete
    # recipe is still source-checked; defer chunking until this enclosing chain
    # has been assembled and staged by the native backend.
    args = [p.composition_arguments() for p in underlying]
    rows = list(args[0]['private_rows'][:10])
    if any(a['private_rows'][:10] != tuple(rows) for a in args):
        raise ValueError('all effects must share the same proven carrier')
    rows[0] = (1, 6 + 2*(1+len(providers)), NONE, 0)
    count = len(channels)
    head = {0: 0x40000000, 1: count, 2: count, 4: 0x22, 5: 1+len(providers), 7: 0x846}
    commands = []
    # Retain the captured carrier without its old two-entry head or effect data.
    for block in _blocks(args[0]['commands']):
        if 1 <= block[1] >> 24 < 10:
            commands.extend(block)
    commands.extend(_write(1, {1: count, 2: count, 5: active_capacity,
                               8: monitor_delay_samples, 16: record_delay_samples}))
    if record_pre_effects:
        commands.extend(_write(MON_TAP[0], {MON_TAP[1]: RECORD_TAP}))
    effects = []
    resources = []
    render_ids = []
    taps = []
    for index, (provider, a, slot) in enumerate(zip(underlying, args, slots)):
        base = len(rows)
        effect_rows = a['private_rows'][10:]
        for local, (pool, words, link, flags) in enumerate(effect_rows):
            target = L(0, 8 + 2*index) if local == 0 else L(base+(link >> 24)-10, link & 0xffffff)
            rows.append((pool, words, target, flags))
        view = ComposedEffect(provider, base_row=base, row_count=len(effect_rows), channels=channels,
                              slot=slot, record_delay_samples=record_delay_samples)
        effects.append(view)
        for block in _blocks(a['commands']):
            if block[1] >> 24 >= 10:
                commands.extend(view._commands(block))
        # Fresh initializers precede the retained accepted control state. This
        # preserves knobs when a neighboring insert causes chain reconstruction.
        current = {(row, word+i): v for row, word, values in provider.observations() if row >= 10
                   for i, v in enumerate(values)}
        commands.extend(view._commands(_writes(current)))
        shadow = _shadow(a['commands'])
        old_flags = shadow.get((10, 4), 0)
        taps.append((base, old_flags & ~RECORD_TAP))
        commands.extend(_write(base, {1: count, 2: count, 4: _tap(old_flags, index == len(providers)-1,
                                                                   record_pre_effects)}))
        head[9+2*index] = a['render_id']
        render_ids.append(a['render_id'])
        for resource in a['resources']:
            if resource not in resources:
                resources.append(resource)
    commands = list(_write(0, head)) + commands
    check(rows, commands)
    observations = [(0, 0, (0x40000000, count, count)), (0, 4, (0x22, 1+len(providers))),
                    (1, 1, (count, count)), (1, 5, (active_capacity,)),
                    (1, 8, (monitor_delay_samples,)), (1, 16, (record_delay_samples,))]
    if record_pre_effects:
        observations.append((*MON_TAP, (RECORD_TAP,)))
    for index, effect in enumerate(effects):
        observations.extend(effect.observations())
        observations.extend(((effect.base_row, 1, (count, count)),
                             (effect.base_row, 4, (_shadow(commands)[effect.base_row, 4],)),
                             (0, 9+2*index, (render_ids[index],))))
    arguments = dict(render_id=render_ids[0], resources=tuple(resources), private_rows=tuple(rows),
                     dsp=1, input_pair=(channels[0], 32) if count == 1 else channels,
                     commands=tuple(commands), scratch=tuple(max(a['scratch'][p] for a in args) for p in range(3)))
    return ComposedChain(arguments, tuple(effects), tuple(render_ids), tuple(dict.fromkeys(observations)),
                         taps=tuple(taps))
