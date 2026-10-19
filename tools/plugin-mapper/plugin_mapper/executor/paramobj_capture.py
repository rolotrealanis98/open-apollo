"""Offline executor for the UAD plug-in whose parameters are `juce::AudioProcessorParameter` objects and whose processor does not
override setParameter/getParameter (Auto-Tune Realtime X: `AutoTunePluginAudioProcessor`, 29 parameters, JUCE 7 wrapper, dynamic C runtime).

Same contract as vstmain_capture.VstMainCapture: nothing is synthesized. The original `DllMain` (CRT start-up and C++ dynamic
initializers), `VSTPluginMain`, `JuceVSTWrapper` and the processor constructor build the whole object graph, and
`UAD_CreateIUADPlug` hands the plug-in the shared IUADPlug cache model. A Console control is what a VST2 host does:
`AEffect.setParameter(index, normalized)` -> the original `JuceVSTWrapper::setParameter` -> the parameter object's own `setValue`
-> the processor's parameter listener -> the engine, whose methods write the DSP words through `[engine+8]` (the IUADPlug). The host never
touches the parameter objects itself: the original wrapper owns the list (`AudioProcessor::flatParameterList`), the lookup by index and
the notification, so no offset of it is declared anywhere.

What this module adds to VstMainCapture, each item justified by a trace of the original code reaching it (test_paramobj_capture.py
asserts that every entry of CRT_STUBS is reached by the construction + startup + a control on every parameter, so nothing is dead):

  * the modern dynamic C runtime (api-ms-win-crt-*, MSVCP140, VCRUNTIME140) that the earlier static-CRT plug-ins never import
    (CRT_STUBS). `_initterm` / `_initterm_e` are guest code (a loop that calls the table entries), everything else is a host function;
  * the TEB self pointer / stack bounds the CRT start-up lock reads (GS:[0x30], [0x8], [0x40], [0x48]);
  * threads are recorded, never scheduled (single-thread host): the processor constructor starts the Antares engine thread
    (juce::Thread 'Antares_Thread'); it is not on the parameter -> DSP path (see families-dspaudioprocessor.md section 9);
  * one replaced original routine, `juce::String::String(double, int, bool)` (the only user of the MSVCP140 iostream imports), pinned
    by the profile (`paramobjects.host_services`); the replacement formats like the iostream it replaces and tail-calls the original
    `String(const char*, size_t)` constructor;
  * the Console text of a parameter is the VST2 `effGetParamDisplay` (the parameter's own text), not handler slot 15 (the UAD plug-in
    bar's short text: 'ALTO/TEN'); all 29 captured defaults match byte for byte.
"""
import math
import re
import struct
from unicorn import UC_HOOK_CODE
from unicorn.x86_const import (UC_X86_REG_RIP, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9, UC_X86_REG_XMM1, UC_X86_REG_XMM2)
from generic_capture import HEAP, STACK, BASE, integer
from vstmain_capture import VstMainCapture
from dspap_capture import DspApCapture, F32, F32BITS, INTERNAL, SCHEMA

KEY = 'paramobjects'
THUNKS = HEAP + 0x3a0000                  # guest-code thunks (hand assembled below)
FROZEN_TIME = 1760000000                  # the host clock: 2025-10-09T08:53:20Z, never advances
GET_PARAM_DISPLAY = 7                     # VST2 effGetParamDisplay

# `_initterm(first, last)` / `_initterm_e`: call every non-null function pointer of [first, last); the _e form stops at the first non-zero result.
#   push rbx,rsi,rdi; sub rsp,0x20; mov rsi,rcx; mov rdi,rdx
#   L: cmp rsi,rdi; jae done; mov rax,[rsi]; test rax,rax; jz next; call rax; [test eax,eax; jnz done_keep_eax]
#   next: add rsi,8; jmp L; done: xor eax,eax; add rsp,0x20; pop rdi,rsi,rbx; ret
INITTERM = bytes.fromhex('53 56 57 48 83 ec 20 48 89 ce 48 89 d7'
                         '48 39 fe 73 10 48 8b 06 48 85 c0 74 02 ff d0 48 83 c6 08 eb eb'
                         '31 c0 48 83 c4 20 5f 5e 5b c3')
INITTERM_E = bytes.fromhex('53 56 57 48 83 ec 20 48 89 ce 48 89 d7'
                           '48 39 fe 73 14 48 8b 06 48 85 c0 74 06 ff d0 85 c0 75 08 48 83 c6 08 eb e7'
                           '31 c0 48 83 c4 20 5f 5e 5b c3')
THUNK_TABLE = (('_initterm', INITTERM), ('_initterm_e', INITTERM_E))

# name -> (phase in which the traced original code first reaches it, what the call is). Every entry is bound, nothing else of these DLLs is;
# test_paramobj_capture.py asserts the bound set equals the set reached by construction, startup and controls on every parameter.
CRT_STUBS = {
    '_initialize_onexit_table': ('crt_startup', 'DllMain CRT start-up: the module-local atexit table'),
    '_register_onexit_function': ('crt_startup', 'C++ dynamic initializers register their destructors'),
    '_initterm': ('crt_startup', 'DllMain CRT start-up: the initializer tables (guest thunk)'),
    '_initterm_e': ('crt_startup', 'DllMain CRT start-up: the C initializer table (guest thunk)'),
    '_Mtx_init_in_situ': ('crt_startup', 'std::mutex members of the statics'),
    'towupper': ('crt_startup', 'juce::CharacterFunctions in the static initializers'),
    '_Cnd_init_in_situ': ('vst_plugin_main', 'std::condition_variable of the juce::WaitableEvent of the engine thread'),
    '_Cnd_broadcast': ('vst_plugin_main', 'WaitableEvent::signal in juce::Thread::startThread'),
    '_Mtx_lock': ('vst_plugin_main', 'std::mutex::lock around the same signal'),
    '_Mtx_trylock': ('vst_plugin_main', 'std::mutex::try_lock'),
    '_Mtx_unlock': ('vst_plugin_main', 'std::mutex::unlock'),
    '_beginthreadex': ('vst_plugin_main', 'juce::Thread::launchThread (Antares engine thread): recorded, not scheduled'),
    'SetThreadPriority': ('vst_plugin_main', 'juce::Thread::setPriority right after launchThread'),
    '_ftime64_s': ('vst_plugin_main', 'juce::Time during the processor constructor'),
    'GetModuleHandleA': ('vst_plugin_main', 'JUCE probes user32 for optional DPI APIs (SetThreadDpiAwarenessContext)'),
    'FormatMessageW': ('vst_plugin_main', 'JUCE File error text after the settings file open fails'),
    '__RTDynamicCast': ('vst_plugin_main', 'the VST wrapper casts the processor to the handler class (UAD_CreateIUADPlug argument)'),
    'atoi': ('vst_plugin_main', 'string to int in the processor constructor'),
    'labs': ('control_2', 'engine scale handling (Scale parameter)'),
}
# the host's vetted C library subset (DspApCapture.import_table) for the names the trace reaches
BASE_CRT = {'malloc': 'crt_startup', 'calloc': 'crt_startup', 'free': 'crt_startup', 'memcpy': 'crt_startup', 'memset': 'crt_startup',
            'strlen': 'vst_plugin_main', 'memmove': 'vst_plugin_main', 'realloc': 'vst_plugin_main', 'strncpy': 'vst_plugin_main',
            'log': 'vst_plugin_main', 'exp': 'vst_plugin_main', 'floorf': 'vst_plugin_main', 'ceilf': 'vst_plugin_main', 'fabs': 'vst_plugin_main',
            'sin': 'original_startup', 'cos': 'original_startup', 'pow': 'original_startup', 'powf': 'original_startup', 'sinf': 'control_11'}


def _i32(v):
    v &= 0xffffffff
    return v - (1 << 32) if v >> 31 else v


class ParamObjectsCapture(VstMainCapture):
    """Same host API as GenericCapture (run_profile.py and the carrier provider call it)."""

    def __init__(self, profile, auto_start=True):
        self.paramobjects = profile['dspap'][KEY]
        self.crt_hits, self.crt_first = {}, {}
        VstMainCapture.__init__(self, profile, auto_start)

    # --- host fixtures -------------------------------------------------------------------------------------------
    def extra_win32(self):
        table = VstMainCapture.extra_win32(self)
        stubs = self.crt_table()
        redirects = getattr(self, 'sdk_redirects', None)
        if redirects is None:
            redirects = self.sdk_redirects = {}
        for k, (name, code) in enumerate(THUNK_TABLE):
            addr = THUNKS + 0x100 * k
            self.uc.mem_write(addr, code)
            for n, label, stub in self.imports:
                if n is not None and n.decode() == name:
                    redirects[stub] = addr
                    self.uc.hook_add(UC_HOOK_CODE, lambda uc, address, size, data, name=name: self.crt_hits.__setitem__(name, self.crt_hits.get(name, 0) + 1),
                                     begin=addr, end=addr)
        table.update(stubs)
        return table

    def bind_sdk(self):
        """TEB (GS base), the UAD2SDK entry and the profile's replaced original routines."""
        teb = HEAP + 0x370000                  # the GS base DspApCapture sets
        self.wq(teb + 0x30, teb)               # NT_TIB.Self: __scrt_acquire_startup_lock reads GS:[0x30]
        self.wq(teb + 8, STACK + 0x100000)     # StackBase
        self.wq(teb + 0x40, 0x1000)            # ClientId.UniqueProcess
        self.wq(teb + 0x48, 0x2000)            # ClientId.UniqueThread (= GetCurrentThreadId of extra_win32)

        def create_plug():      # UAD_CreateIUADPlug(audioMaster, AEffect*, CUADPlugCallbackHandler*, version) -> IUADPlug*
            self.sdk.append(('CreateIUADPlug', [self.arg(i) & 0xffffffffffffffff for i in range(4)]))
            return self.iface
        stub = next(s for n, label, s in self.imports if n is not None and n.decode().startswith('?UAD_CreateIUADPlug'))
        self.handlers[stub] = ('UAD_CreateIUADPlug', create_plug)
        self.install_host_services()

    def install_host_services(self):
        for k, svc in enumerate(self.paramobjects.get('host_services', [])):
            va, tail = integer(svc['va']), integer(svc['tail_va'])
            head = bytes(self.uc.mem_read(va, len(bytes.fromhex(svc['head']))))
            if head != bytes.fromhex(svc['head']) or svc['name'] != 'juce_string_from_double':
                raise ValueError('replaced original routine differs from the profile pin ' + svc['name'])
            stub = INTERNAL + 16 * (220 + k)
            self.handlers[stub] = ('host service ' + svc['name'], self.string_from_double(tail))
            self.uc.mem_write(va, b'\x48\xb8' + struct.pack('<Q', stub) + b'\xff\xe0')       # mov rax, stub; jmp rax

    def string_from_double(self, tail):
        """juce::String::String(double n, int decimals, bool scientific): the original builds an ostringstream (fixed precision `decimals`
        when decimals > 0, else the stream default %g with precision 6), streams the double and constructs the String from its chars.
        Entry registers: rcx = result String*, xmm1 = n, r8d = decimals, r9b = scientific. The host formats the text exactly as that
        iostream does (printf %.Nf), then continues in the original `String(const char*, size_t)` (rdx = text, r8 = length), which
        returns rcx like the original function does."""
        def run():
            u = self.uc
            x = struct.unpack('<d', struct.pack('<Q', u.reg_read(UC_X86_REG_XMM1) & 0xffffffffffffffff))[0]
            decimals, scientific = _i32(u.reg_read(UC_X86_REG_R8)), u.reg_read(UC_X86_REG_R9) & 1
            if not math.isfinite(x) or scientific:
                raise ValueError('juce::String(double): non-finite or scientific text is not part of the traced contract')
            mode = 'fixed' if decimals > 0 else 'general'
            self.crt_hits['juce_string_from_double/' + mode] = self.crt_hits.get('juce_string_from_double/' + mode, 0) + 1
            text = ('%.*f' % (decimals, x)) if decimals > 0 else ('%g' % x)
            buf = self.reserve(len(text) + 1)
            u.mem_write(buf, text.encode('ascii') + b'\0')
            u.reg_write(UC_X86_REG_RDX, buf)
            u.reg_write(UC_X86_REG_R8, len(text))
            u.reg_write(UC_X86_REG_RIP, tail)
        return run

    def crt_table(self):
        u, a = self.uc, self.arg
        hits = self.crt_hits
        self.onexit, self.mutexes, self.mutex_types, self.threads = [], {}, {}, []

        def counted(name, fn):
            def run():
                hits[name] = hits.get(name, 0) + 1
                self.crt_first.setdefault(name, self.phase)
                return fn()
            return run

        def cstr(p, n=4096):
            return bytes(u.mem_read(p, n)).split(b'\0')[0]

        def onexit_init():      # first = last = end = EncodePointer(null); EncodePointer is the identity here
            u.mem_write(a(0), bytes(24))
            return 0

        def onexit_register():
            self.onexit.append(a(1))
            return 0

        def mutex_init():       # _Mtx_init_in_situ(mtx, type): _Thrd_success; the lock state lives on the host
            self.mutexes[a(0)] = 0
            self.mutex_types[a(0)] = a(1) & 0xffffffff
            u.mem_write(a(0), bytes(8))
            return 0

        def mutex_lock():
            if self.mutexes.get(a(0), 0) and not self.mutex_types.get(a(0), 0) & 0x100:      # _Mtx_recursive
                raise ValueError('single-thread host: non-recursive mutex locked twice (deadlock)')
            self.mutexes[a(0)] = self.mutexes.get(a(0), 0) + 1
            return 0

        def mutex_try_lock():   # _Thrd_success 0, _Thrd_busy 3
            if self.mutexes.get(a(0), 0) and not self.mutex_types.get(a(0), 0) & 0x100:
                return 3
            self.mutexes[a(0)] = self.mutexes.get(a(0), 0) + 1
            return 0

        def mutex_unlock():
            if self.mutexes.get(a(0), 0) <= 0:
                raise ValueError('unlock of an unlocked mutex')
            self.mutexes[a(0)] -= 1
            return 0

        def begin_thread():     # _beginthreadex(security, stack, start, arg, initflag, &id): recorded, never scheduled
            handle = 0x7d000000 + 16 * len(self.threads)
            self.threads.append(dict(entry=a(2), arg=a(3), flags=a(4) & 0xffffffff, handle=handle))
            if a(5):
                u.mem_write(a(5), struct.pack('<I', 0x3000 + len(self.threads)))
            return handle

        def module_handle_a():
            name = cstr(a(0), 64).decode('ascii').lower()
            if name == 'user32.dll':       # a virtual module whose optional exports resolve absent (SoftubeRuntime.GetProcAddress)
                return self.runtime._module(name)
            self.win32.last_error = 126
            return 0

        def format_message():   # FormatMessageW(flags, src, id, lang, buf, size, args): FROM_SYSTEM | IGNORE_INSERTS
            flags, code, buf, size = a(0) & 0xffffffff, a(2) & 0xffffffff, a(4), a(5) & 0xffffffff
            text = {2: 'The system cannot find the file specified.', 3: 'The system cannot find the path specified.', 5: 'Access is denied.'}.get(code)
            if flags != 0x1200 or text is None:
                raise ValueError('unmodeled FormatMessageW request %r' % ((hex(flags), code),))
            raw = (text + ' \r\n').encode('utf-16-le') + b'\0\0'        # the system appends a space and CRLF
            if size * 2 < len(raw):
                self.win32.last_error = 122
                return 0
            u.mem_write(buf, raw)
            return len(raw) // 2 - 1

        def dynamic_cast():     # __RTDynamicCast(ptr, vfDelta, srcType, targetType, isReference): the original RTTI of the object
            ptr, target, by_reference = a(0), a(3), a(4) & 0xffffffff
            if not ptr:
                return 0
            rd = lambda p: int.from_bytes(u.mem_read(p, 4), 'little')
            rds = lambda p: int.from_bytes(u.mem_read(p, 4), 'little', signed=True)
            locator = self.rq(self.rq(ptr) - 8)
            if rd(locator) != 1 or rd(locator + 8) != 0:
                raise ValueError('dynamic_cast: object without a plain RTTI locator')
            complete = ptr - rd(locator + 4)
            hierarchy = BASE + rd(locator + 16)
            found = set()
            for i in range(rd(hierarchy + 8)):
                base_class = BASE + rd(BASE + rd(hierarchy + 12) + 4 * i)
                if BASE + rd(base_class) == target:
                    if rds(base_class + 12) != -1:      # PMD at +8: mdisp, pdisp (-1 = not a virtual base), vdisp
                        raise ValueError('dynamic_cast: virtual base target')
                    found.add(complete + rds(base_class + 8))
            if len(found) != 1 and by_reference:
                raise ValueError('dynamic_cast to reference failed')
            return found.pop() if len(found) == 1 else 0

        def case(f):
            return lambda: ord(f(chr(a(0) & 0xffff))) if (a(0) & 0xffff) < 128 else a(0) & 0xffff      # C locale: only ASCII has case

        stubs = {'_initialize_onexit_table': onexit_init, '_register_onexit_function': onexit_register,
                 '_Mtx_init_in_situ': mutex_init, '_Mtx_lock': mutex_lock, '_Mtx_trylock': mutex_try_lock, '_Mtx_unlock': mutex_unlock,
                 '_Cnd_init_in_situ': lambda: (u.mem_write(a(0), bytes(8)), 0)[1], '_Cnd_broadcast': lambda: 0,
                 '_beginthreadex': begin_thread, 'SetThreadPriority': lambda: 1,
                 '_ftime64_s': lambda: (u.mem_write(a(0), struct.pack('<qHhh2x', FROZEN_TIME, 0, 0, 0)), 0)[1],     # struct __timeb64, UTC, no DST
                 'GetModuleHandleA': module_handle_a, 'FormatMessageW': format_message, '__RTDynamicCast': dynamic_cast,
                 'atoi': lambda: max(-2 ** 31, min(2 ** 31 - 1, int((re.match(rb'[ \t\n\v\f\r]*([+-]?\d+)', cstr(a(0), 64)) or [0, b'0'])[1]))) & 0xffffffff,
                 'labs': lambda: abs(_i32(a(0))) & 0xffffffff, 'towupper': case(str.upper)}
        # `_initterm` and `_initterm_e` are guest thunks (extra_win32); everything below is the shared C library subset
        base = DspApCapture.import_table(self)
        imported = {n.decode() for n, label, stub in self.imports if n is not None}
        stubs.update({name: base[name] for name in BASE_CRT if name in imported and name in base})
        return {name: counted(name, fn) for name, fn in stubs.items() if name in imported}

    # --- the Console view of a parameter ------------------------------------------------------------------------
    def format_parameter(self, index):
        """The text the Console shows is the parameter's own text through the original wrapper (VST2 effGetParamDisplay)."""
        before = dict(self.shadow)
        buf = self.string_buffer
        self.uc.mem_write(buf, b'\0' * 64)
        self.dispatch(GET_PARAM_DISPLAY, index=index, ptr=buf)
        raw = bytes(self.uc.mem_read(buf, 64))
        value = raw[:raw.index(b'\0')].decode('latin-1')
        if before != self.shadow:
            raise ValueError('original formatter altered coefficient shadow')
        return dict(raw=value, console_trimmed=value.strip())
