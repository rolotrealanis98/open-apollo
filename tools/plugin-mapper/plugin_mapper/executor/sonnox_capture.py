"""Offline executor for the Sonnox "Soup" layer UAD plug-ins (JUCE 7 VST2 wrapper, Oxford Limiter family).

Same host contract as dspap_capture.DspApCapture (handler vtable + IUADPlug cache), but the parameter side is the
Soup framework instead of a JUCE value tree. The original plug-in code runs under Unicorn on a synthetic
processor object; nothing here knows a coefficient, text or domain:

  * Console domain: the original `Limiter_Defs` object (Soup::DefsBase factory + static data initializer) builds
    one Soup::ParameterBase per parameter; its law strategy maps normalized -> physical (float code), its display
    strategy produces the Console text (the statically linked printf helper is the only replaced routine);
  * DSP: the original `Limiter_UAParameterCalculation` object (constructor, setup, setParameter, the two writers)
    driven exactly the way the processor drives it: listener slot 5 (`FUN_18010fca0`) = calc.setParameter then the
    processor's own atomic flush (begin / calc writer / end); startup = the original handler slot 17 (reload);
  * descriptor: the original handler slot 3 reaches the IUASonnoxAudioProcessor sub-object through the
    statically linked dynamic_cast, which is the one replaced RTTI service.

Replaced original-image routines (host services only, each pinned by its leading bytes in the profile):
allocator family, the printf helper, the process-name host detection, the wall clock, the latency-change
broadcast and the dynamic_cast. Unknown imports and interface calls still reject. Hardware is never touched.
"""
import hashlib, struct
from unicorn import UC_HOOK_CODE
from unicorn.x86_const import UC_X86_REG_RAX, UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_XMM0, UC_X86_REG_XMM1, UC_X86_REG_XMM2
from generic_capture import integer, BASE
from dspap_capture import DspApCapture, PROC, WRAPPER, SCHEMA, F32, F32BITS

SONNOX_KEY = 'sonnox'


def printf_format(read_cstr, fmt, arg):
    """C printf subset the Soup display code uses (flags, width, precision, d i u x s f e g c), arguments in the
    Windows x64 variadic order (integer class slots; doubles arrive as their bit pattern)."""
    import re
    pending = [2]

    def nxt():
        pending[0] += 1
        return arg(pending[0] - 1)

    def conv(m):
        flags, width, prec, kind = m.groups()
        if kind == '%':
            return '%'
        if width == '*':
            width = str(struct.unpack('<i', struct.pack('<I', nxt() & 0xffffffff))[0])
        if prec == '.*':
            prec = '.' + str(struct.unpack('<i', struct.pack('<I', nxt() & 0xffffffff))[0])
        spec = '%' + flags + (width or '') + (prec or '')
        if kind == 's':
            return (spec + 's') % read_cstr(nxt())
        if kind in 'di':
            return (spec + 'd') % struct.unpack('<i', struct.pack('<I', nxt() & 0xffffffff))[0]
        if kind in 'uxXo':
            return (spec + kind) % (nxt() & 0xffffffff)
        if kind in 'fFeEgG':
            return (spec + kind) % struct.unpack('<d', struct.pack('<Q', nxt() & 0xffffffffffffffff))[0]
        if kind == 'c':
            return chr(nxt() & 0xff)
        raise ValueError('unsupported printf conversion ' + kind)
    return re.sub(r'%([-+ #0]*)(\*|\d+)?(\.\*|\.\d+)?(?:hh|h|ll|l|z|I64)?([diuxXofFeEgGcs%])', conv, fmt)


class SonnoxCapture(DspApCapture):
    """Same host API as GenericCapture (run_profile.py and the carrier provider call it)."""

    # --- original-image service replacement --------------------------------------------------------------
    def replace(self, va, fn):
        def hook(u, address, size, data):
            try:
                result = fn()
            except Exception as exc:
                self.failure = exc
                u.emu_stop()
                return
            sp = u.reg_read(UC_X86_REG_RSP)
            u.reg_write(UC_X86_REG_RAX, (result or 0) & 0xffffffffffffffff)
            u.reg_write(UC_X86_REG_RIP, struct.unpack('<Q', bytes(u.mem_read(sp, 8)))[0])
            u.reg_write(UC_X86_REG_RSP, sp + 8)
        self.uc.hook_add(UC_HOOK_CODE, hook, begin=va, end=va)

    def cstring(self, ptr, limit=1024):
        raw = bytes(self.uc.mem_read(ptr, limit))
        return raw[:raw.index(b'\0')].decode('latin-1')

    def install_hooks(self):
        sx = self.sx
        for h in sx['hooks']:
            va = integer(h['va'])
            if hashlib.sha256(bytes(self.uc.mem_read(va, 16))).hexdigest() != h['head_sha256']:
                raise ValueError('hooked original routine differs: ' + h['role'] + ' ' + h['va'])
        by_role = {}
        for h in sx['hooks']:
            by_role.setdefault(h['role'], []).append(integer(h['va']))
        u = self.uc

        def block(size):
            return self.reserve(max(16, size))

        def realloc():
            old, size = self.arg(0), self.arg(1)
            new = block(size)
            if old and old in self.allocations:
                u.mem_write(new, bytes(u.mem_read(old, min(self.allocations[old], size))))
            return new

        def printf():
            out = self.arg(0)
            text = printf_format(self.cstring, self.cstring(self.arg(1)), self.arg).encode('latin-1') + b'\0'
            u.mem_write(out, text)
            return len(text) - 1

        def clock():
            u.mem_write(self.arg(0), b'\0' * 8)
            return self.arg(0)

        def cast():
            target = self.arg(3)
            return PROC + integer(sx['casts'][hex(target)]) if self.arg(0) == PROC and hex(target) in sx['casts'] else 0
        table = dict(alloc=lambda: block(self.arg(0)), calloc=lambda: block(self.arg(0) * self.arg(1)), realloc=realloc,
                     free=lambda: 0, msize=lambda: self.allocations.get(self.arg(0), 0), printf=printf, host_detect=lambda: 0,
                     clock=clock, broadcast=lambda: 0, cast=cast, defs=lambda: self.defs)  # defs: the Defs singleton accessor
        for role, vas in by_role.items():
            if role not in table:
                raise ValueError('unknown hook role ' + role)
            for va in vas:
                self.replace(va, table[role])

    def set_array(self):
        """IUADPlug +0xb0/+0xb8: (ref, ptr, count, stride, force). The shared model accepts only force=0; the Soup setup
        writers also pass force=1 (SDK `_setIntArray` compares against the old shadow word unless force is set), which
        changes only whether an unchanged word is re-sent, never the word itself, so the written words are identical."""
        ref, ptr, count, stride, force = [self.arg(i) for i in range(1, 6)]
        count &= 0xffffffff
        stride &= 0xffffffff
        force &= 0xffffffff
        if not (0 <= ref < 0x80000000 and 0 < count and stride == 1 and force in (0, 1) and (ref & 0xffffff) + count <= 0x1000000):
            raise ValueError('unsupported shared array ABI ' + repr((ref, count, stride, force)))
        values = [self.rd(ptr + 4 * i) for i in range(count)]
        self.shadow.update({ref + i: v for i, v in enumerate(values)})
        live = {self.rd(c) for c in self.sequence_counters}
        self.sequence_words.update({ref + i: (v if v in live else None) for i, v in enumerate(values)})
        self.note_loader_event()
        self.events.append(['write', ref, values])
        return 0

    # --- float calls into original code -------------------------------------------------------------------
    def vslot(self, obj, offset):
        return self.rq(self.rq(obj) + offset)

    def fcall(self, fn, *args, xmm1=None, xmm2=None):
        """Original call with Windows x64 float arguments (xmm1 / xmm2); returns the float32 in xmm0."""
        if xmm1 is not None:
            self.uc.reg_write(UC_X86_REG_XMM1, F32BITS(F32(xmm1)))
        if xmm2 is not None:
            self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(xmm2)))
        self.call(fn, *args)
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(UC_X86_REG_XMM0) & 0xffffffff))[0]

    # --- synthetic processor -----------------------------------------------------------------------------------
    def build_processor(self):
        sx = self.sx = self.spec[SONNOX_KEY]
        off = sx['fields']
        self.install_hooks()
        # the original CRT encoded-null initializer, then the original static data initializer of the Defs tables
        self.phase = 'original_crt_encoded_null_initializer'
        from crt_init import initialize
        self.crt_initialization = initialize(self, self.profile['dll']['path'])
        self.wd(integer(sx['host_id']['global']), integer(sx['host_id']['value']))  # what the original plug-in entry stores
        self.phase = 'original_static_initializers'
        for va in sx['static_initializers']:
            self.call(integer(va))
        self.phase = 'original_defs_factory'
        self.defs = self.call(integer(sx['defs']['factory']))
        count = self.rd(self.defs + 0x14)
        if count != len(self.spec['parameters']):
            raise ValueError('original Defs parameter count differs: ' + str(count))
        table = self.rq(self.defs + 8)
        self.soup = [self.rq(table + 8 * i) for i in range(count)]
        for i, p in enumerate(self.soup):
            if self.rq(p) != integer(sx['defs']['parameter_vtable']):
                raise ValueError('original Defs parameter object differs')
            if self.cstring(p + 0x18, 16) != self.spec['parameters'][i]['id']:
                raise ValueError('original Defs parameter id differs ' + str(i))
        # processor object: vtables of the original class layout, host-owned fields
        self.processor = PROC
        self.wrapper = WRAPPER
        for name in ('primary', 'listener', 'iface'):
            self.wq(PROC + (0 if name == 'primary' else integer(off[name])), integer(sx['vtables'][name]))
        self.wq(PROC + integer(off['defs']), self.defs)
        self.uc.mem_write(PROC + integer(off['rate']), struct.pack('<d', float(sx['host']['sample_rate'])))
        self.wd(PROC + integer(off['block']), int(sx['host']['block_size']))
        # wrapper: the JuceVSTWrapper holds the handler at +0x128 (its vtable slot 3 is the descriptor getter)
        hw = sx['wrapper']
        self.handler = WRAPPER + integer(hw['handler_offset'])
        self.wq(self.handler, integer(self.spec['handler']['vtable']))
        self.wq(self.handler + integer(hw['processor_field']), PROC)
        self.wq(self.handler + integer(hw['iface_field']), self.iface)
        io = sx['host']['io_channels']  # the wrapper's declared numInputs / numOutputs (descriptor IO)
        self.wd(WRAPPER + integer(hw['io_in']), io)
        self.wd(WRAPPER + integer(hw['io_out']), io)
        self.cells = []
        # the original calculation object
        self.phase = 'original_calculation_constructor'
        c = sx['calc']
        self.calc = self.reserve(integer(c['size']))
        if c.get('constructor'):
            self.call(integer(c['constructor']), self.calc)
        else:
            # constructor inlined in the plug-in entry: the object vtable and the fields it stores (the rest is
            # filled by the processor's own default-parameter pushes below)
            self.wq(self.calc, integer(c['vtable']))
            for f in c['plain']:
                self.uc.mem_write(self.calc + integer(f['offset']), int(f['value']).to_bytes(int(f['size']), 'little'))
        if self.rq(self.calc) != integer(c['vtable']):
            raise ValueError('original calculation object differs')
        self.wq(PROC + integer(off['calc']), self.calc)
        # processor construction pushes every default through the listener while no IUADPlug is attached (no flush)
        self.phase = 'original_default_parameters'
        self.values = tuple(int(p['default']) for p in self.spec['parameters'])
        for i, v in enumerate(self.values):
            self.listener(i, self.physical(i, v))
        # the host hands the IUADPlug over (IUASonnoxAudioProcessor slot 3), then prepares the processor
        self.phase = 'original_iface_and_setup'
        sub = PROC + integer(off['iface'])
        self.call(self.vslot(sub, 0x18), sub, self.iface)
        if self.rq(PROC + integer(off['iuad'])) != self.iface:
            raise ValueError('IUASonnoxAudioProcessor did not retain the IUADPlug')
        h = sx['host']
        # the configured channel count is what the carrier requests (mono unless the stereo request applies)
        self.call(self.vslot(self.calc, 0x8), self.calc, int(h['sample_rate']), self.host_channels, int(h['block_size']), 0)
        latency = self.call(self.vslot(self.calc, 0x38), self.calc) & 0xffffffff
        if sx['latency_samples'] is not None and latency != int(sx['latency_samples']):
            raise ValueError('original calculation latency differs from the captured Console latency: ' + str(latency))
        self.wd(PROC + integer(off['latency_cache']), latency)

    def apply_raw_values(self):
        pass

    # --- Console domain: original law + display ----------------------------------------------------------------
    def physical(self, index, value):
        """Original Soup law: integer step -> normalized float -> physical float (ParameterBase slot 0x108)."""
        p = self.soup[index]
        return self.fcall(self.vslot(p, 0x108), p, 0, 0, xmm1=value / self.maxima[index])

    def listener(self, index, physical):
        """The processor's DSP parameter listener (listener sub-object slot 5): calc.setParameter + atomic flush."""
        sub = PROC + integer(self.sx['fields']['listener'])
        self.fcall(self.vslot(sub, 0x28), sub, index, 0, xmm2=physical)

    def push(self):
        raise ValueError('Soup layer pushes per parameter through the listener')

    def format_parameter(self, index):
        before = dict(self.shadow)
        p = self.soup[index]
        display = self.rq(p + 0x10)
        buf = self.string_buffer
        self.uc.mem_write(buf, b'\0' * 64)
        self.fcall(self.vslot(display, 0x58), display, 0, buf, 0, xmm1=self.physical(index, self.values[index]))
        value = self.cstring(buf, 64)[:31]  # the display strategy fills a 32-byte C buffer (the processor trims it to maxLen 32)
        if before != self.shadow:
            raise ValueError('original display strategy altered coefficient shadow')
        return dict(raw=value, console_trimmed=value.strip())

    def set_control(self, index, value):
        if type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        values = list(self.values)
        values[index] = value
        self.values = tuple(values)
        self.listener(index, self.physical(index, value))
        self._verify_replay()
        return value, dict(self.shadow)
