"""Offline executor for the UAD DSPAudioProcessor-family (JUCE VST2 wrapper + CUADPlugCallbackHandler).

No CHypParamBlock, no PlugIn RTTI: the plug-in's own original code runs under Unicorn against a
synthetic *processor object* whose layout is declared by the profile (profile['dspap']):

  * handler  = processor + handler_offset (the CUADPlugCallbackHandler sub-object); its original
               vtable slot 3 fills the AllocInfo descriptor (same layout as the CHyp families);
  * iface    = processor + iface_offset: the IUADPlug the SDK hands the plug-in. It is the same cache
               interface GenericCapture already models (lock 0x18, begin/end 0x90/0x98, set_int 0xa0,
               set_float 0xa8, set/get array 0xb0..0xd8): every DSP write lands in the coefficient shadow;
  * parameters = JUCE raw (denormalised) values the original update function reads through pointer
               fields of the processor (the value-tree atomics); the host sets them from the integer
               domain (raw = min + (max-min) * v / maximum) and runs the original update function.

Second flavor (profile dspap.setter present): the plug-in has no value-tree listener. Its processor's own
JUCE setParameter(index, normalized) virtual is the parameter -> DSP function (it stores the value in plug-in
state, converts it and pushes the DSP words itself), so the host runs it per control:

  * the plug-in's own constructor runs (dspap.constructor) from its entry to the call that loads user presets from
    disk: base constructor, field literals, parameter state block and its default loop, all with the IUADPlug pointer
    still null (no DSP writes); state-less plug-ins then load their factory default program through setParameter;
  * the host attaches the IUADPlug / wrapper pointers (what the VST wrapper does), the original AllocInfo call runs,
    and handler slot 17 is the load-time call;
  * processor and heap memory start uninitialised (operator new): every byte original code reads must have been
    written by original code or declared by the host; an uninitialised read rejects instead of reading zero;
  * the statically linked CRT's malloc/free (and sprintf_s, which needs per-thread CRT data) are answered by the host.
"""
import pe_cache
import math, re, struct
from collections import deque
from generic_capture import GenericCapture, OfflineHost, UC_HOOK_BLOCK, HEAP, STACK, STUBS, STOP, BASE, integer, _ARGUMENTS, UcError
from probe_transactions import Replay
from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE
from unicorn.x86_const import (UC_X86_REG_MXCSR, UC_X86_REG_FPCW, UC_X86_REG_GS_BASE, UC_X86_REG_XMM0,
                               UC_X86_REG_XMM1, UC_X86_REG_XMM2, UC_X86_REG_RIP, UC_X86_REG_RSP)

PROC = HEAP + 0x200000          # synthetic processor object
WRAPPER = HEAP + 0x2f0000       # stand-in for the JuceVSTWrapper (AudioEffectX) the handler stores at +0x18
WRAPPER_VT = HEAP + 0x2f1000
CELLS = HEAP + 0x2f2000         # raw parameter cells (one float each, 16 bytes apart)
SCHEMA = 'openapollo-dspaudioprocessor-profile/1'
INTERNAL = STUBS + 0xf000        # stubs for statically linked CRT functions the host answers (imports end below this)
PROC_SPAN = 0x1000              # processor bytes tracked by the strict-read check (setter flavor)
F32 = lambda v: struct.unpack('<f', struct.pack('<f', v))[0]
F32BITS = lambda v: struct.unpack('<I', struct.pack('<f', v))[0]


def field_bytes(field):
    """Little-endian bytes of a declared plain field."""
    return int(field['value']).to_bytes(int(field['size']), 'little')


class DspApCapture(GenericCapture):
    """Same host API as GenericCapture (what run_profile.py and the carrier provider call)."""

    trace_blocks = True

    @property
    def host_channels(self):
        return self._host_channels

    @host_channels.setter
    def host_channels(self, value):
        """Channel count the host configured; the processor mirrors it in its play-config fields (setter flavor)."""
        self._host_channels = value
        if getattr(self, 'channel_offsets', None) and getattr(self, 'phase', '') != 'profile_validation':
            for off in self.channel_offsets:
                self.uc.mem_write(PROC + off, int(value).to_bytes(4, 'little'))
                self.mark(PROC + off, 4)

    def __init__(self, profile, auto_start=True):
        if type(self) is DspApCapture and 'paramobjects' in (profile.get('dspap') or {}):
            # JUCE parameter objects behind the original VST2 wrapper, dynamic C runtime (paramobj_capture.py: Auto-Tune Realtime X)
            from paramobj_capture import ParamObjectsCapture
            self.__class__ = ParamObjectsCapture
            return ParamObjectsCapture.__init__(self, profile, auto_start)
        if type(self) is DspApCapture and 'vstmain' in (profile.get('dspap') or {}):
            # the original VSTPluginMain builds the whole object graph (vstmain_capture.py: Sphere family)
            from vstmain_capture import VstMainCapture
            self.__class__ = VstMainCapture
            return VstMainCapture.__init__(self, profile, auto_start)
        self.profile = profile
        self.phase = 'profile_validation'
        spec = profile.get('dspap')
        if profile.get('schema') != SCHEMA or not isinstance(spec, dict):
            raise ValueError('DSPAudioProcessor profile required')
        self.spec = spec
        self.carrier_channels = profile.get('carrier_channels', 1)
        self.host_channels = 2 if profile.get('stereo_channel_request') is True else 1
        params = spec['parameters']
        self.maxima = tuple(int(p['maximum']) for p in params)
        if not self.maxima or len(self.maxima) > 256 or any(not 0 < m <= 32768 for m in self.maxima):
            raise ValueError('bounded integer parameter domains required')
        self.phase = 'map_original_image'
        OfflineHost.__init__(self, profile['dll']['path'], profile['dll']['sha256'])
        u = self.uc
        u.mem_map(HEAP + 0x10000, 0x7f0000)
        self.last_blocks = deque(maxlen=32)
        if self.trace_blocks:       # diagnostic only (the block that failed); a Python callback per block costs ~5x emulation speed
            u.hook_add(UC_HOOK_BLOCK, lambda uc, a, size, data: self.last_blocks.append(a))
        self.iface = HEAP + 0x300000
        self.vtable = HEAP + 0x310000
        self.lock = HEAP + 0x320000
        self.lockvt = HEAP + 0x330000
        self.fs = HEAP + 0x350000
        self.string_buffer = HEAP + 0x360000
        self.alloc_next = HEAP + 0x400000
        self.allocations = {}
        self.shadow = {}
        self.atomic = None
        self.trace = []
        self.events = []
        self.notifications = []
        self.notification_events = []
        self.atomic_refs = set()
        self.last_error = 0
        self.sequence_counters = set()
        self.sequence_words = {}
        self.random_words = {}
        self.pending_rand = None
        self.loader_event_indices = set()
        self.loader_depth = 0
        self.loader_calls = 0
        self.deferred_reads = set()
        self.descriptor = None
        self.crt_initialization = {}
        self.crt_thread_initialization = []
        self.parent_identity_bindings = []
        self.constructor_values_outside_domain = []
        self.parent_allocation = None
        self.parent = PROC
        self.this = PROC
        self.last_unknown = None
        self.setter = spec.get('setter')
        self.alloc_sites = {}
        # reads of never-written bytes the profile accepts, by instruction address (each carries its justification)
        self.uninitialised_ok = {integer(x['pc']) for x in spec.get('uninitialised_ok', [])}
        self.channel_offsets = [integer(o) for o in spec.get('channel_fields', [])]
        self.state = spec.get('state_block')
        self.initialised = set()
        self.uninitialised_reads = {}
        self.tracked = []
        self.state_ptr = None
        if self.setter:
            self.track(PROC, PROC_SPAN)
            self.track(HEAP + 0x400000, 0x400000)      # private allocation arena: malloc memory starts uninitialised
        u.reg_write(UC_X86_REG_GS_BASE, HEAP + 0x370000)
        self.wq(HEAP + 0x370010, STACK)
        # Loader TLS semantics (as GenericCapture / VstMain): GS:[0x58] -> TLS vector, slot 0 = copy of the PE TLS template
        # (the JUCE AudioProcessor base constructor of the Auto-Tune DLLs reads its per-thread block through it).
        import pefile
        tls_pe = pe_cache.parsed(profile['dll']['path'], [9])
        self.image_size = tls_pe.OPTIONAL_HEADER.SizeOfImage
        tls = getattr(tls_pe, 'DIRECTORY_ENTRY_TLS', None)
        if tls is not None:
            template = bytes(tls_pe.get_memory_mapped_image()[tls.struct.StartAddressOfRawData - BASE:tls.struct.EndAddressOfRawData - BASE])
            if len(template) + tls.struct.SizeOfZeroFill > 0x1000:
                raise ValueError('bounded TLS template required')
            self.tls_vector, self.tls_slot0 = HEAP + 0x380000, HEAP + 0x381000
            self.wq(HEAP + 0x370058, self.tls_vector)
            self.wq(self.tls_vector, self.tls_slot0)
            self.uc.mem_write(self.tls_slot0, template)
        u.reg_write(UC_X86_REG_MXCSR, 0x1f80)
        u.reg_write(UC_X86_REG_FPCW, 0x37f)
        self.wf(self.fs, 48000)
        self.wf(self.fs + 4, 1 / 48000)
        # IUADPlug fixture: unknown slots reject, modelled slots are the shared cache interface.
        self.wq(self.iface, self.vtable)
        self.wq(self.lock, self.lockvt)
        for vt, start, label, count in ((self.vtable, STUBS, 'cache', 0x40), (self.lockvt, STUBS + 0x1000, 'lock', 0x40),
                                        (WRAPPER_VT, STUBS + 0x2000, 'wrapper', 0x40)):
            for i in range(count):
                addr = start + i * 16
                self.wq(vt + i * 8, addr)
                self.handlers[addr] = ('unknown ' + label + ' ' + hex(i * 8), None)
        self.bind(0x18, 'cache_lock', lambda: self.lock)
        for off in (8, 16):
            self.handlers[STUBS + 0x1000 + (off // 8) * 16] = ('lock_ref', lambda: 0)
        for off, name in ((0x90, 'begin'), (0x98, 'end'), (0xa0, 'set_int'), (0xa8, 'set_float'), (0xb0, 'set_array'),
                          (0xb8, 'set_array'), (0xc0, 'get_var'), (0xc8, 'get_var'), (0xd0, 'get_array'), (0xd8, 'get_array'),
                          (0xe0, 'header_timing')):
            self.bind(off, name, getattr(self, name))
        if spec.get('channel_fields'):
            # IUADPlug +0x28 = configured channel count, +0x40 = sample rate (float in xmm0): the two queries the handler's load-unit
            # virtual makes; both answered from the host configuration (mono request unless the profile asks for stereo).
            self.bind(0x28, 'iface_channels', lambda: self.host_channels)
            self.bind(0x40, 'iface_sample_rate', lambda: (self.uc.reg_write(UC_X86_REG_XMM0, F32BITS(48000.0)), 0)[1])
        for slot in spec.get('quiet_iface_slots', []):
            self.bind(integer(slot), 'iface_flush', lambda: 0)
        # Wrapper (AudioEffectX) services the original handler calls: int2string(value, text, maxLen).
        self.handlers[STUBS + 0x2000 + (0x140 // 8) * 16] = ('wrapper_int2string', self.int2string)
        self.wq(WRAPPER, WRAPPER_VT)
        self.bind_imports()
        self.build_processor()
        if self.setter:
            self.construct_setter_flavor()
        else:
            self.values = tuple(int(p['default']) for p in params)
            self.apply_raw_values()
        self.constructor_values = self.values
        self.phase = 'constructed'
        if auto_start:
            self.start()

    # --- synthetic processor ---------------------------------------------------------------------------
    def build_processor(self):
        s = self.spec
        self.handler = PROC + integer(s['handler']['offset'])
        for off, va in ((0, s['processor_vtable']), (integer(s['handler']['offset']), s['handler']['vtable'])):
            vt = integer(va)
            if not BASE <= vt < BASE + max(0x4000000, self.image_size):
                raise ValueError('processor vtable outside original image')
            if not (self.setter and s.get('constructor')):
                self.wq(PROC + off, vt)
        for slot, va in s['handler']['slots'].items():
            if self.rq(integer(s['handler']['vtable']) + integer(slot)) != integer(va):
                raise ValueError('handler vtable slot differs from the original image ' + slot)
        if not (self.setter and s.get('constructor')):
            self.wq(PROC + integer(s['iface_offset']), self.iface)
            self.wq(PROC + integer(s['wrapper_offset']), WRAPPER)
            self.wq(self.handler + 0x10, self.iface)
            self.wq(self.handler + 0x18, WRAPPER)
            self.uc.mem_write(PROC + integer(s['sample_rate_offset']), struct.pack('<d', 48000.0))
        self.cells = []
        if not self.setter:
            for i, p in enumerate(s['parameters']):
                cell = CELLS + 16 * i
                self.cells.append(cell)
                self.wq(PROC + integer(p['pointer_offset']), cell)
        for name, field in s.get('plain_fields', {}).items():
            self.uc.mem_write(PROC + integer(field['offset']), field_bytes(field))
            self.mark(PROC + integer(field['offset']), int(field['size']))

    def raw_value(self, index, value):
        p = self.spec['parameters'][index]
        lo, hi = float(p['raw_min']), float(p['raw_max'])
        return F32(lo + (hi - lo) * value / int(p['maximum']))

    def apply_raw_values(self):
        for i, v in enumerate(self.values):
            self.wf(self.cells[i], self.raw_value(i, v))

    # --- strict initialisation tracking (setter flavor) ------------------------------------------------
    def track(self, start, size):
        """Reads of bytes in [start, start+size) that nobody initialised are recorded and reject at the end of the call."""
        def written(uc, access, address, length, value, data):
            self.initialised.update(range(address, address + length))

        def read(uc, access, address, length, value, data):
            for a in range(address, address + length):
                if a not in self.initialised:
                    pc = uc.reg_read(UC_X86_REG_RIP)
                    if pc not in self.uninitialised_ok and not self.zeroing_and(pc):
                        self.uninitialised_reads.setdefault(a, pc)
        self.uc.hook_add(UC_HOOK_MEM_WRITE, written, None, start, start + size - 1)
        self.uc.hook_add(UC_HOOK_MEM_READ, read, None, start, start + size - 1)
        self.tracked.append((start, size))

    def zeroing_and(self, pc):
        """`and [mem], 0` (compiler zeroing idiom): its read of the old value cannot influence the result."""
        code = bytes(self.uc.mem_read(pc, 8))
        skip = 1 if 0x40 <= code[0] <= 0x4f else 0
        return code[skip] == 0x83 and (code[skip + 1] >> 3) & 7 == 4 and (code[skip + 1] >> 6) != 3 and self._and_imm(code, skip) == 0

    @staticmethod
    def _and_imm(code, skip):
        modrm = code[skip + 1]
        length = 2 + (1 if modrm >> 6 == 1 else 4 if modrm >> 6 == 2 else 0) + (1 if modrm & 7 == 4 else 0)
        if modrm >> 6 == 0 and modrm & 7 == 5:
            length += 4
        return code[skip + length]

    def mark(self, address, size):
        if any(s <= address < s + n for s, n in self.tracked):
            self.initialised.update(range(address, address + size))

    def call(self, fn, *args):
        result = GenericCapture.call(self, fn, *args)
        if self.uninitialised_reads:
            self.call_failed_uninitialised()
        return result

    # --- setter flavor -------------------------------------------------------------------------------------
    def run_until(self, fn, stop, *args):
        """Run original code from fn until execution reaches `stop` (a mid-function address)."""
        self.failure = None
        rsp = STACK + 0xff008
        self.wq(rsp, STOP)
        for i, value in enumerate(args[:4]):
            self.uc.reg_write(_ARGUMENTS[i], value)
        self.uc.reg_write(UC_X86_REG_RSP, rsp)
        try:
            self.uc.emu_start(fn, stop, count=20000000)
        except UcError as exc:
            raise RuntimeError('emulator at ' + hex(self.uc.reg_read(UC_X86_REG_RIP)) + ': ' + str(exc)) from exc
        if self.failure:
            raise RuntimeError(str(self.failure)) from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP) != stop:
            raise RuntimeError('original constructor did not reach its stop address (pc %#x)' % self.uc.reg_read(UC_X86_REG_RIP))
        if self.uninitialised_reads:
            self.call_failed_uninitialised()

    def call_failed_uninitialised(self):
        def where(a):
            if PROC <= a < PROC + PROC_SPAN:
                return hex(a - PROC)
            blocks = [p for p in self.allocations if p <= a < p + self.allocations[p]]
            if blocks:
                b = max(blocks)
                return '%s=heap+%s of %#x bytes from malloc caller %s' % (hex(a), hex(a - b), self.allocations[b], hex(self.alloc_sites.get(b, 0)))
            return hex(a)
        raise ValueError('original code read uninitialised host memory: ' + ', '.join(
            '%s (pc %s)' % (where(a), hex(pc)) for a, pc in sorted(self.uninitialised_reads.items())[:48:4]))

    def construct_setter_flavor(self):
        """The plug-in's own constructor runs (processor memory starts uninitialised, like operator new), from its entry up to
        the call that loads user presets from disk: the base constructor, the field literals, the parameter state block and
        its default loop (setParameter with the interface pointer still null). The host then does what the VST wrapper does:
        attach the IUADPlug / wrapper pointers and the sample rate, and (state-less plug-ins) load the default program."""
        s = self.spec
        c = s['constructor']
        si = s.get('static_initializers')
        if si:      # C++ dynamic initializers of the image (what the CRT start-up would run before the first processor is created)
            # atexit(dtor): the static CRT's table is set up by CRT start-up (not run here); the offline process never exits, so
            # registering a destructor is a no-op.
            stub = INTERNAL + 16 * 200
            self.handlers[stub] = ('static crt atexit', lambda: 0)
            self.uc.mem_write(integer(si['atexit']), b'\x48\xb8' + struct.pack('<Q', stub) + b'\xff\xe0')
            for i in range(int(si['count'])):
                try:
                    self.call(self.rq(integer(si['table']) + 8 * i))
                except Exception as exc:
                    raise RuntimeError('static initializer %d (%#x) failed: %s' % (i, self.rq(integer(si['table']) + 8 * i), exc)) from exc
        self.run_until(integer(c['va']), integer(c['stop_va']), PROC)
        for off, want in ((0, s['processor_vtable']), (integer(s['handler']['offset']), s['handler']['vtable'])):
            if self.rq(PROC + off) != integer(want):
                raise ValueError('original constructor stored a different vtable at +' + hex(off))
        for off in (integer(s['iface_offset']), integer(s['wrapper_offset'])):
            if self.rq(PROC + off) != 0:
                raise ValueError('constructor left the host-owned pointer +' + hex(off) + ' non-null')
        self.uc.mem_write(PROC + integer(s['sample_rate_offset']), struct.pack('<d', 48000.0))
        self.mark(PROC + integer(s['sample_rate_offset']), 8)
        self.host_channels = self.host_channels      # write the play-config channel fields
        params = s['parameters']
        self.domain_min = [int(p.get('raw_min', 0)) for p in params]
        if self.state:
            self.state_ptr = self.rq(PROC + integer(self.state['pointer_offset']))
        else:
            for i, p in enumerate(params):      # default program through the plug-in's own setParameter (interface still null)
                self.invoke_setter(i, F32(int(p['default']) / int(p['maximum'])))
        for off, value in ((integer(s['iface_offset']), self.iface), (integer(s['wrapper_offset']), WRAPPER)):
            self.wq(PROC + off, value)               # what the VST wrapper does after UAD_CreateIUADPlug
            self.mark(PROC + off, 8)
        self.values = self.read_stored_values()

    def read_state_int(self, i):
        raw = bytes(self.uc.mem_read(self.state_ptr + integer(self.state['values_offset']) + 4 * i, 4))
        return struct.unpack('<i', raw)[0]

    def read_stored_values(self):
        """Host integer vector (0..maximum) as the plug-in itself stores it: state-block ints, or its processor float fields."""
        params = self.spec['parameters']
        if self.state:
            return tuple(self.read_state_int(i) - self.domain_min[i] for i in range(len(params)))
        out = []
        for p in params:
            raw = bytes(self.uc.mem_read(PROC + integer(p['field_offset']), 4))
            out.append(min(int(p['maximum']), max(0, round(struct.unpack('<f', raw)[0] * int(p['maximum'])))))
        return tuple(out)

    def invoke_setter(self, index, norm):
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(norm)))
        self.call(integer(self.setter['va']), PROC, index)

    # --- imports -----------------------------------------------------------------------------------------
    def bind_imports(self):
        table = self.import_table()
        for name, label, stub in self.imports:
            key = name.decode() if name else None
            if key in table:
                self.handlers[stub] = ('crt ' + key, table[key])
        # Statically linked CRT heap (no import): redirect the original malloc/free entries to the host allocator.
        for k, (name, va) in enumerate(sorted(self.spec.get('crt', {}).items())):
            stub = INTERNAL + 16 * k
            self.handlers[stub] = ('static crt ' + name, table[name])
            self.uc.mem_write(integer(va), b'\x48\xb8' + struct.pack('<Q', stub) + b'\xff\xe0')

    def f32_arg(self, reg):
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(reg) & 0xffffffff))[0]

    def f64_arg(self, reg):
        return struct.unpack('<d', struct.pack('<Q', self.uc.reg_read(reg) & 0xffffffffffffffff))[0]

    def import_table(self):
        u = self.uc

        def ret_f32(v):
            u.reg_write(UC_X86_REG_XMM0, F32BITS(F32(v)))
            return 0

        def ret_f64(v):
            u.reg_write(UC_X86_REG_XMM0, struct.unpack('<Q', struct.pack('<d', v))[0])
            return 0

        def mem(dst, n, fill):
            if n:
                u.mem_write(dst, fill)
                self.mark(dst, n)
            return dst

        def memset():
            n = self.arg(2) & 0xffffffffffffffff
            if n > 0x100000:
                raise ValueError('bounded memset')
            return mem(self.arg(0), n, bytes([self.arg(1) & 0xff]) * n)

        def memcpy():
            n = self.arg(2)
            if n > 0x100000:
                raise ValueError('bounded memcpy')
            return mem(self.arg(0), n, bytes(u.mem_read(self.arg(1), n)) if n else b'')

        def cstring(ptr, limit=4096):
            raw = bytes(u.mem_read(ptr, limit))
            return raw[:raw.index(b'\0')] if b'\0' in raw else raw

        def strncpy():
            dst, src, n = self.arg(0), self.arg(1), self.arg(2)
            text = cstring(src)[:n]
            u.mem_write(dst, text + b'\0' * (n - len(text)))
            self.mark(dst, n)
            return dst

        def malloc():
            ptr = self.reserve(max(16, self.arg(0)))
            self.alloc_sites[ptr] = self.rq(u.reg_read(UC_X86_REG_RSP))      # caller of malloc (diagnostics of uninitialised reads)
            return ptr

        def realloc():
            ptr, size = self.arg(0), self.arg(1)
            if not ptr:
                return malloc()
            if not size:
                return 0
            new = self.reserve(max(16, size))
            keep = min(self.allocations.get(ptr, 0), size)
            if keep:
                u.mem_write(new, bytes(u.mem_read(ptr, keep)))
                self.mark(new, keep)
            return new

        def calloc():
            size = max(16, self.arg(0) * self.arg(1))
            ptr = self.reserve(size)
            self.mark(ptr, size)
            return ptr

        def unary32(fn):
            return lambda: ret_f32(fn(self.f32_arg(UC_X86_REG_XMM0)))

        def unary64(fn):
            return lambda: ret_f64(fn(self.f64_arg(UC_X86_REG_XMM0)))

        def binary32(fn):
            return lambda: ret_f32(fn(self.f32_arg(UC_X86_REG_XMM0), self.f32_arg(UC_X86_REG_XMM1)))

        def binary64(fn):
            return lambda: ret_f64(fn(self.f64_arg(UC_X86_REG_XMM0), self.f64_arg(UC_X86_REG_XMM1)))

        def safe(fn, bad=float('nan')):
            def inner(*a):
                try:
                    return fn(*a)
                except (ValueError, OverflowError):
                    return bad
            return inner

        def rnd(x):  # C round(): half away from zero
            return math.copysign(math.floor(abs(x) + 0.5), x) if math.isfinite(x) else x

        def noreturn():
            raise ValueError('original code reached a noreturn CRT failure')

        def wide_text(text, buf_arg=0):
            """GetCurrentDirectoryW / GetTempPathW / GetModuleFileNameW semantics: returns the length when it fits, else the size needed
            (arguments are (size, buffer) for the first two and (module, buffer, size) for GetModuleFileNameW)."""
            size, buf = (self.arg(0), self.arg(1)) if not buf_arg else (self.arg(2), self.arg(1))
            data = text.encode('utf-16-le') + b'\0\0'
            if size * 2 < len(data):
                return len(data) // 2
            u.mem_write(buf, data)
            self.mark(buf, len(data))
            return len(data) // 2 - 1

        cs = lambda: 0  # single-threaded offline host: critical sections never contend
        t = {'EnterCriticalSection': cs, 'LeaveCriticalSection': cs, 'InitializeCriticalSection': cs, 'DeleteCriticalSection': cs,
             'InitializeCriticalSectionAndSpinCount': lambda: 1, 'InitializeCriticalSectionEx': lambda: 1, 'TryEnterCriticalSection': lambda: 1,
             'memset': memset, 'memcpy': memcpy, 'memmove': memcpy, 'strncpy': strncpy,
             'strlen': lambda: len(cstring(self.arg(0))), 'malloc': malloc, 'calloc': calloc, 'free': lambda: 0, 'realloc': realloc,
             '__RTDynamicCast': lambda: 0, '_invalid_parameter_noinfo_noreturn': noreturn, 'terminate': noreturn,
             # Auto-Tune: the constructor asks the shell for the per-user data folder (settings / licence cache); FALSE = not available
             'SHGetSpecialFolderPathW': lambda: 0,
             # JUCE File helpers the constructor touches while resolving its settings folder: every path is "not present"
             'GetCurrentDirectoryW': lambda: wide_text('C:\\'), 'GetTempPathW': lambda: wide_text('C:\\'),
             'GetModuleFileNameW': lambda: wide_text('C:\\plugin.dll', 1),
             'EncodePointer': lambda: self.arg(0), 'DecodePointer': lambda: self.arg(0),   # identity: no cookie to hide pointers behind
             'GetModuleHandleW': lambda: 0, 'GetModuleHandleA': lambda: 0, 'GetProcAddress': lambda: 0,
             'LoadLibraryExW': lambda: 0, 'LoadLibraryW': lambda: 0,
             'GetLastError': lambda: 0, 'SetLastError': lambda: 0,
             'timeBeginPeriod': lambda: 0, 'timeGetTime': lambda: 0,
             'QueryPerformanceFrequency': lambda: (u.mem_write(self.arg(0), struct.pack('<q', 10000000)), self.mark(self.arg(0), 8), 1)[2],
             'QueryPerformanceCounter': lambda: (u.mem_write(self.arg(0), struct.pack('<q', 0)), self.mark(self.arg(0), 8), 1)[2],
             # JUCE threading / timing statics: handles are opaque non-null tokens, waits are already signalled, no thread is ever created
             'CreateEventW': lambda: 0x1111, 'SetEvent': lambda: 1, 'CloseHandle': lambda: 1, 'WaitForSingleObject': lambda: 0,
             'GetCurrentThreadId': lambda: 1, 'GetCurrentProcessId': lambda: 1, 'GetCurrentThread': lambda: 0xfffffffffffffffe,
             'GetCurrentProcess': lambda: 0xffffffffffffffff, 'Sleep': lambda: 0, 'GetTickCount64': lambda: 0, 'OutputDebugStringW': lambda: 0,
             'GetFileAttributesW': lambda: 0xffffffff, 'GetFileAttributesExW': lambda: 0, 'CreateFileW': lambda: 0xffffffffffffffff}
        for n, f in (('floor', math.floor), ('ceil', math.ceil), ('round', rnd), ('sqrt', safe(math.sqrt)), ('exp', safe(math.exp, math.inf)),
                     ('log', safe(math.log, -math.inf)), ('log10', safe(math.log10, -math.inf)), ('sin', safe(math.sin)), ('cos', safe(math.cos)),
                     ('tan', safe(math.tan)), ('fabs', abs), ('atan', math.atan), ('tanh', math.tanh)):
            fn = (lambda g: lambda x: float(g(x)) if math.isfinite(x) else x)(f)
            t[n] = unary64(fn)
            t[n + 'f'] = unary32(fn)
        for n, f in (('pow', safe(math.pow)), ('fmod', safe(math.fmod)), ('atan2', math.atan2)):
            t[n] = binary64(f)
            t[n + 'f'] = binary32(f)
        t['roundf'] = unary32(lambda x: rnd(x))
        t['sprintf_s'] = self.sprintf_s
        return t

    FORMAT = re.compile(r'%([-+ #0]*)(\d+)?(?:\.(\d+))?(?:hh|h|ll|l|I64|I32|I)?([diuxXcsfeEgG%])')

    def sprintf_s(self):
        """Statically linked CRT sprintf_s(buf, size, fmt, ...): the original code's formatting call (its CRT needs per-thread
        data this host does not model). Windows x64 varargs: arg 3 in r9, the rest on the stack; doubles travel as raw bits."""
        buf, size, fmt = self.arg(0), self.arg(1), self.arg(2)
        raw = bytes(self.uc.mem_read(fmt, 64))
        text = raw[:raw.index(b'\0')].decode('ascii')
        index = [3]

        def vararg():
            value = self.arg(index[0])
            index[0] += 1
            return value

        def convert(m):
            flags, width, precision, kind = m.group(1), m.group(2) or '', m.group(3), m.group(4)
            if kind == '%':
                return '%'
            spec = '%' + flags + width + ('.' + precision if precision is not None else '')
            value = vararg()
            if kind in 'feEgG':
                return (spec + kind) % struct.unpack('<d', struct.pack('<Q', value & 0xffffffffffffffff))[0]
            if kind == 's':
                return (spec + 's') % bytes(self.uc.mem_read(value, 64)).split(b'\0')[0].decode('ascii')
            if kind == 'c':
                return chr(value & 0xff)
            value &= 0xffffffff
            if kind in 'di' and value & 0x80000000:
                value -= 1 << 32
            return (spec + ('d' if kind in 'diu' else kind)) % value
        out = self.FORMAT.sub(convert, text).encode('ascii')
        if len(out) + 1 > size:
            raise ValueError('sprintf_s output exceeds its buffer: ' + repr(out))
        self.uc.mem_write(buf, out + b'\0')
        self.mark(buf, len(out) + 1)
        return len(out)

    def int2string(self):
        value, buf, limit = self.arg(1) & 0xffffffff, self.arg(2), self.arg(3) & 0xffffffff
        value = value - (1 << 32) if value & 0x80000000 else value
        text = str(value).encode()[:max(0, limit - 1)] + b'\0'
        self.uc.mem_write(buf, text)
        return 0

    # IUADPlug array calls (ref, ptr, count, stride, flags). Auto-Tune passes flags=1 (the C-Vox/DMX/Wood plug-ins pass 0): the same
    # contiguous write at ref that the BX family records (executor/vst2_bx_host.py); the flag's meaning is qualified by the live gate only.
    def set_array(self):
        if self.arg(5) & 0xffffffff != 1:
            return GenericCapture.set_array(self)
        ref, ptr, count, stride = self.arg(1), self.arg(2), self.arg(3) & 0xffffffff, self.arg(4) & 0xffffffff
        if not (0 <= ref < 0x80000000 and 0 < count and stride == 1 and (ref & 0xffffff) + count <= 0x1000000):
            raise ValueError('unsupported shared array ABI ' + repr((ref, count, stride, 1)))
        values = [self.rd(ptr + 4 * i) for i in range(count)]
        self.shadow.update({ref + i: v for i, v in enumerate(values)})
        self.sequence_words.update({ref + i: None for i in range(count)})
        self.note_loader_event()
        self.events.append(['write', ref, values])
        return 0

    # --- host API used by run_profile.py / the carrier provider ------------------------------------------
    def vcall(self, slot, *args):
        if slot == 0x10 and len(args) == 1 and 0 <= args[0] < len(self.values):
            return self.values[args[0]]
        raise ValueError('unmodeled parameter block slot ' + hex(slot))

    def read_values(self):
        return tuple(self.values)

    def read_ceiling(self):
        return self.maxima

    def push(self):
        """The plug-in's own parameter -> DSP update (what its parameter listener runs)."""
        self.call(integer(self.spec['update']['va']), PROC)

    def format_parameter(self, index):
        fmt = self.spec['formatter']
        before = dict(self.shadow)
        buf = self.string_buffer
        self.uc.mem_write(buf, b'\0' * 64)
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(self.values[index] / self.maxima[index])))
        self.call(integer(fmt['va']), self.handler, index, 0, buf)
        raw = bytes(self.uc.mem_read(buf, 64))
        value = raw[:raw.index(b'\0')].decode('ascii')
        if before != self.shadow:
            raise ValueError('original formatter altered coefficient shadow')
        return dict(raw=value, console_trimmed=value.strip())

    def check_deferred_reads(self):
        pass

    def ensure_descriptor_call(self):
        """The SDK asks the plug-in for its AllocInfo (handler slot 3) before the plug-in is resumed, and plug-ins keep
        results of that call in processor fields (DMX 15-80 S: +0x2d8, +0x330, read by the resume path). Hosts that start the
        plug-in without extracting the descriptor still get the original call first."""
        if self.setter and not getattr(self, 'descriptor_called', False):
            self.phase = 'original_descriptor_call'
            before = dict(self.shadow)
            self.call(self.rq(self.rq(self.handler) + 0x18), self.handler, self.reserve(0x1000))
            if self.shadow != before:
                raise ValueError('original descriptor call wrote DSP memory')
            self.descriptor_called = True

    def start(self):
        self.ensure_descriptor_call()
        self.phase = 'original_startup'
        for step in self.spec['startup']:
            self.call(integer(step['va']), *[self.handler if a == 'handler' else PROC if a == 'processor' else integer(a)
                                              for a in step['arguments']])
        self.defaults_values = tuple(self.values)
        self.defaults = dict(self.shadow)
        self.startup_events = list(self.events)
        self.startup_formatted = [self.format_parameter(i) for i in range(len(self.maxima))]
        self._verify_replay()
        self.captured_properties = None
        shown = self.spec.get('captured_text')
        if shown is not None:
            for i, text in enumerate(self.startup_formatted):
                if text['console_trimmed'] != shown[i] and not (
                        i in self.spec.get('blank_for_zero', ()) and text['console_trimmed'] == '0' and shown[i] == ''):
                    raise ValueError('captured original formatter mismatch ' + repr((i, text, shown[i])))
        self.phase = 'ready'

    def _verify_replay(self):
        replay = Replay()
        replay.apply(self.events)
        if replay.words != self.shadow:
            raise ValueError('shared event replay differs from complete original shadow')

    def set_control(self, index, value):
        if type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        if self.setter:
            self.invoke_setter(index, value / self.maxima[index])
            self.values = self.read_stored_values()
            self._verify_replay()
            return self.values[index], dict(self.shadow)
        values = list(self.values)
        values[index] = value
        self.values = tuple(values)
        self.apply_raw_values()
        self.push()
        self._verify_replay()
        return value, dict(self.shadow)


def extract_descriptor(cap, profile, decode_descriptor):
    """Original handler slot 3 (GetAllocInfo) on the synthetic processor; decoded by the shared decoder."""
    import hashlib
    from pathlib import Path
    spec = profile['dspap']['descriptor']
    fn = integer(spec['getter_va'])
    if cap.rq(cap.rq(cap.handler) + 0x18) != fn:
        raise ValueError('descriptor getter is not the original handler slot 3')
    before = dict(cap.shadow)
    first = len(cap.events)
    out = cap.reserve(0x1000)
    cap.call(fn, cap.handler, out)
    cap.descriptor_called = True
    if any(cap.shadow.get(k) != v for k, v in before.items()):
        raise ValueError('descriptor call overwrote coefficient shadow words')
    if cap.shadow.keys() - before.keys():
        raise ValueError('descriptor call wrote DSP memory')
    data = decode_descriptor(cap, out, plugin=profile['name'], dll_sha256=profile['dll']['sha256'])
    carrier = 2 if list(data['io']) == [2, 2] and profile.get('stereo_channel_request') is True else 1
    cap.host_channels = carrier
    requested = [carrier, carrier]
    if any(n not in (1, 2) for n in data['io']):
        raise ValueError('original IO capabilities outside mono/stereo')
    data['host_requested_io'] = requested
    data['normalized_io'] = [min(a, b) for a, b in zip(requested, data['io'])]
    data['configured_input_channels'] = data['normalized_io'][0]
    data['channel_normalization_evidence'] = str(Path(__file__).resolve().parents[1] / 'carrier/channel-normalization.json')
    data['descriptor_function'] = hex(fn)
    data['receiver_evidence'] = dict(status='original_handler_slot3_descriptor', framework='DSPAudioProcessor',
                                     handler_offset=profile['dspap']['handler']['offset'], vtable=profile['dspap']['handler']['vtable'],
                                     getter_va=spec['getter_va'])
    return data


import vstmain_capture  # noqa: E402,F401  (loads the VstMain host so the runtime manifest hashes it; this module is imported first)
import paramobj_capture  # noqa: E402,F401  (so the runtime manifest hashes it; vstmain_capture above is loaded first)
