"""Offline executor for Brainworx `BXEffect` plug-ins behind a `UADAdapter` (UAD ADA Flanger, Ampeg, bx_*): sub-family B.

The original DLL runs under Unicorn as a VST2 host would drive it (see vst2_host.py for sub-family A). Differences:
the effect is a heap `BXEffect` (static CRT, XML/resource machinery, BXDSPMemory) wrapped by a heap `UADAdapter`;
the CUADPlugCallbackHandler is a separate heap object, so the descriptor exists only after the BX effect is constructed.
Statically linked allocator entry points are replaced by pinned intercepts; everything else is original code.
"""
import os, struct
from vst2_host import Vst2Capture, SCHEMA, FRAMEWORK, OPCODES
from generic_capture import integer


class Win32Extras:
    """Win32 services the static CRT and the BX/UAD adapter constructors use (module identity, TLS slots, env, heap queries)."""

    def win32_extras(self):
        from generic_capture import BASE
        def module_ex():
            self.uc.mem_write(self.arg(2), struct.pack('<Q', BASE)); return 1
        def module_name():
            path = self.vst2['module_path'].encode() + b'\0'
            n = min(len(path), self.arg(2))
            self.uc.mem_write(self.arg(1), path[:n - 1] + b'\0'); return n - 1
        tls = {}
        def tls_alloc():
            i = len(tls); tls[i] = 0; return i
        def tls_set():
            tls[self.arg(0) & 0xffffffff] = self.arg(1); return 1
        self.ticks = 100000
        def tick():
            self.ticks += 16; return self.ticks
        envblock = []
        def env_strings():
            if not envblock:
                envblock.append(self.reserve(16)); self.uc.mem_write(envblock[0], b'\0' * 16)   # empty environment: L"\0"
            return envblock[0]
        def heap_size():
            return self.allocations.get(self.arg(2), 0xffffffffffffffff)
        def heap_create():
            self.thread_runtime.heap_obtained = True     # the CRT's private heap is the runtime's single bounded heap
            return self.thread_runtime.heap
        def module_name_w():
            text = self.vst2['module_path'].encode('utf-16-le') + b'\0\0'
            n = min(len(text), self.arg(2) * 2)
            self.uc.mem_write(self.arg(1), text[:n - 2] + b'\0\0'); return n // 2 - 1
        def pc_to_file_header():
            pc = self.arg(0)
            base = BASE if BASE <= pc < BASE + 0x4000000 else 0   # only the one image is loaded
            self.uc.mem_write(self.arg(1), struct.pack('<Q', base)); return base
        def startup_info_a():
            self.uc.mem_write(self.arg(0), b'\0' * 0x68)
            self.uc.mem_write(self.arg(0), struct.pack('<I', 0x68))
            return 0
        def dec():
            return self.arg(0)
        return {'GetStartupInfoA': startup_info_a, 'SetHandleCount': lambda: self.arg(0), 'HeapSetInformation': lambda: 1, 'GetVersion': lambda: 0x23f00206, 'HeapCreate': heap_create, 'HeapDestroy': lambda: 1, 'HeapSize': heap_size, 'GetEnvironmentStringsW': env_strings, 'FreeEnvironmentStringsW': lambda: 1, 'IsDebuggerPresent': lambda: 0,
                'SetUnhandledExceptionFilter': lambda: 0, 'GetFileType': lambda: 2, 'GetTickCount': tick, 'GetCurrentThreadId': lambda: 1, 'GetCurrentProcessId': lambda: 1, 'GetCurrentProcess': lambda: 0xffffffffffffffff,
                'GetEnvironmentVariableA': lambda: 0, 'TlsAlloc': tls_alloc, 'TlsGetValue': lambda: tls.get(self.arg(0) & 0xffffffff, 0), 'TlsSetValue': tls_set,
                'GetLastError': lambda: self.last_error, 'SetLastError': lambda: setattr(self, 'last_error', self.arg(0) & 0xffffffff) or 0,
                'DecodePointer': dec, 'EncodePointer': dec, 'IsProcessorFeaturePresent': lambda: 0, 'InitializeSListHead': lambda: 0,
                'OutputDebugStringA': lambda: 0, 'Sleep': lambda: 0, 'GetModuleHandleA': lambda: BASE if not self.arg(0) else 0,
                'GetModuleHandleExA': module_ex, 'GetModuleFileNameA': module_name, 'GetModuleFileNameW': module_name_w,
                # headless host with an empty registry: opens/queries report ERROR_FILE_NOT_FOUND, creates ERROR_ACCESS_DENIED
                'RegOpenKeyExW': lambda: 2, 'RegOpenKeyExA': lambda: 2, 'RegQueryValueExW': lambda: 2, 'RegQueryValueExA': lambda: 2,
                'RtlPcToFileHeader': pc_to_file_header, 'RegCreateKeyExW': lambda: 5, 'RegCreateKeyExA': lambda: 5, 'RegCloseKey': lambda: 0}


def bx_thread_runtime():
    from thread_runtime import ThreadRuntime

    class BxThreadRuntime(ThreadRuntime):
        """ThreadRuntime with the extra virtual API-set modules the plug-in's static UCRT probes (still single-thread, owned handles)."""
        MODULES = ('kernel32', 'kernel32.dll', 'api-ms-win-core-synch-l1-2-0', 'api-ms-win-core-localization-l1-2-1',
                   'api-ms-win-core-fibers-l1-1-1', 'api-ms-win-core-fibers-l1-1-0', 'advapi32', 'advapi32.dll')
        EXPORTS = ('InitializeCriticalSectionEx', 'InitializeCriticalSectionAndSpinCount', 'FlsAlloc', 'FlsGetValue', 'FlsSetValue', 'FlsFree',
                   'LCMapStringEx', 'SleepConditionVariableCS', 'WakeAllConditionVariable')

        def bind(self):
            found = super().bind()
            for name, label, stub in self.cap.imports:
                if name is not None and name.decode('ascii') in ('LCMapStringW',):
                    self.cap.handlers[stub] = ('bounded_thread_' + name.decode('ascii'), getattr(self, name.decode('ascii')))
                    found.append(name.decode('ascii'))
            return found

        def __init__(self, cap):
            super().__init__(cap)
            self.absent_exports = []
            self.dynamic_next = 0x7100f800     # above the intercept stubs (INTERCEPTS = STUBS + 0xf000 grows upward)

        def _string(self, ptr, wide=False):
            """The base reads a fixed 512 bytes, which runs past the end of the mapped image when the name sits in its last page."""
            unit = 2 if wide else 1
            raw = b''
            while len(raw) < 512:
                raw += bytes(self.cap.uc.mem_read(ptr + len(raw), 16))
                end = next((i for i in range(0, len(raw), unit) if raw[i:i + unit] == bytes(unit)), None)
                if end is not None:
                    return raw[:end].decode('utf-16-le' if wide else 'ascii')
            raise ValueError('bounded Win32 symbol required')

        def MultiByteToWideChar(self):
            """CP_UTF8 (65001, flags 0) for ASCII text, widened one-to-one; every other code page/flag set is the base model."""
            import struct
            cp, flags = self.cap.arg(0) & 0xffffffff, self.cap.arg(1) & 0xffffffff
            if cp != 65001:
                return super().MultiByteToWideChar()
            source, count, dest, capacity = self.cap.arg(2), self.cap.arg(3) & 0xffffffff, self.cap.arg(4), self.cap.arg(5) & 0xffffffff
            if flags:
                raise ValueError('unmodeled UTF-8 conversion flags')
            if count == 0xffffffff:
                raw = b''
                while b'\0' not in raw and len(raw) < 65536:
                    raw += bytes(self.cap.uc.mem_read(source + len(raw), 16))
                raw = raw[:raw.index(b'\0') + 1]
            else:
                raw = bytes(self.cap.uc.mem_read(source, count))
            if any(b >= 0x80 for b in raw):
                raise ValueError('bounded ASCII UTF-8 text required')
            if capacity == 0:
                return len(raw)
            if capacity < len(raw):
                self.cap.last_error = 122
                return 0
            self.cap.uc.mem_write(dest, struct.pack('<' + str(len(raw)) + 'H', *raw))
            return len(raw)

        def WideCharToMultiByte(self):
            """WideCharToMultiByte(cp, flags, src, cch, dst, cchDst, default, used): the base model plus cch = -1 (NUL-terminated source,
            terminator converted too); everything else stays the bounded strict CP1252 conversion."""
            import struct
            count = self.cap.arg(3) & 0xffffffff
            if count == 0xffffffff:
                raw = b''
                while True:   # chunked: the string may sit at the end of the mapped image
                    raw += bytes(self.cap.uc.mem_read(self.cap.arg(2) + len(raw), 16))
                    end = next((i for i in range(0, len(raw), 2) if raw[i:i + 2] == b'\0\0'), None)
                    if end is not None or len(raw) >= 65536:
                        break
                count = end // 2 + 1
                saved = self.cap.arg
                self.cap.arg = lambda i: count if i == 3 else saved(i)
                try:
                    return super().WideCharToMultiByte()
                finally:
                    del self.cap.arg
            return super().WideCharToMultiByte()

        def LCMapStringW(self):
            """LCMapStringW(locale, flags, src, cch, dst, cchDst): the same bounded BMP case mapping as LCMapStringEx (no locale name)."""
            import struct
            from thread_runtime import nls_tables
            locale, flags, source, count, dest, capacity = [self.cap.arg(i) for i in range(6)]
            flags &= 0xffffffff
            count &= 0xffffffff
            capacity &= 0xffffffff
            if count == 0xffffffff:
                raw = bytes(self.cap.uc.mem_read(source, 65536))
                count = next(i for i in range(0, 65536, 2) if raw[i:i + 2] == b'\0\0') // 2 + 1
            if flags not in (0x100, 0x200) or not 0 < count <= 65536:
                raise ValueError('bounded nonlinguistic BMP case mapping required')
            codes = struct.unpack('<' + str(count) + 'H', self.cap.uc.mem_read(source, count * 2))
            if capacity == 0:
                return count
            if capacity < count:
                self.cap.last_error = 122
                return 0
            table = nls_tables()['lower' if flags == 0x100 else 'upper']
            self.cap.uc.mem_write(dest, struct.pack('<' + str(count) + 'H', *[table[ch] for ch in codes]))
            return count

        def LoadLibraryExW(self):
            name = self._string(self.cap.arg(0), True).lower()
            if name not in self.MODULES:
                raise ValueError('unmodeled virtual Win32 module ' + name)
            handle = next((h for h, n in self.modules.items() if n == name), None)
            if handle is None:
                handle = 0x75000000 + len(self.modules) * 0x1000
                self.modules[handle] = name
            self.events.append(('LoadLibraryExW', name, handle))
            return handle

        def GetProcAddress(self):
            handle = self.cap.arg(0)
            if handle not in self.modules:
                raise ValueError('owned virtual module required')
            name = self._string(self.cap.arg(1))
            if name not in self.EXPORTS:
                # Not modelled: report it absent (GetProcAddress NULL, ERROR_PROC_NOT_FOUND). The static CRT/STL probe tables treat a
                # missing export as an older OS and take their own fallback; code that cannot do that faults in the emulator.
                self.absent_exports.append(name)
                self.cap.last_error = 127
                return 0
            if name not in self.exports:
                address = self.dynamic_next
                self.dynamic_next += 16
                if self.dynamic_next > 0x71010000:
                    raise ValueError('bounded dynamic exports exhausted')
                self.cap.handlers[address] = ('bounded_dynamic_' + name, getattr(self, name))
                self.exports[name] = address
            self.events.append(('GetProcAddress', self.modules[handle], name))
            return self.exports[name]
    return BxThreadRuntime


class Vst2BxCapture(Win32Extras, Vst2Capture):
    def import_table(self):
        table = super().import_table()
        table.update(self.win32_extras())
        return table

    LARGE_HEAP, LARGE_HEAP_SIZE = 0x50000000, 0x2000000

    def reserve(self, size):
        """Static initializers of some BX effects allocate one block above the 1 MB bound of the shared arena (1 MB + 7 bytes in the
        guitar-amp family): serve those from a dedicated, lazily mapped 32 MB region; everything else is the shared bounded arena."""
        if type(size) is int and 0x100000 < size <= 0x1000000:
            if not hasattr(self, 'large_next'):
                self.uc.mem_map(self.LARGE_HEAP, self.LARGE_HEAP_SIZE)
                self.large_next = self.LARGE_HEAP
            ptr = self.large_next
            if ptr + size > self.LARGE_HEAP + self.LARGE_HEAP_SIZE:
                raise ValueError('bounded large allocation arena exhausted')
            self.large_next = (ptr + size + 15) & ~15
            self.allocations[ptr] = size
            return ptr
        return super().reserve(size)

    # statically linked CRT heap entry points (pinned by prologue in the profile)
    def intercept_malloc(self):
        return self.reserve(max(16, self.arg(0)))

    def intercept_calloc(self):
        return self.reserve(max(16, self.arg(0) * self.arg(1)))

    def intercept_no_editor(self):
        return 0   # BXEffect::createEditor: no GUI offline (the adapter skips the editor wrapper when it is NULL)

    def intercept_memset(self):
        """Static CRT memset (`rep stosb` above 128 bytes): Unicorn counts every repeated byte as an instruction, so a few MB of static-initializer
        fills exhaust the call budget and take minutes; the native fill writes the same bytes."""
        dst, byte, size = self.arg(0), self.arg(1) & 0xff, self.arg(2)
        if size > 0x2000000:
            raise ValueError('bounded memset required')
        if size:
            self.uc.mem_write(dst, bytes([byte]) * size)
        return dst

    def intercept_free(self):
        return 0

    def intercept_realloc(self):
        old, n = self.arg(0), self.arg(1)
        new = self.reserve(max(16, n))
        if old:
            self.uc.mem_write(new, bytes(self.uc.mem_read(old, min(self.allocations.get(old, n), n))))
        return new

    def setup_tls(self):
        """Loader TLS semantics: slot 0 of the TEB TLS vector (GS:[0x58]) is a copy of the PE TLS template plus zero fill."""
        import pefile
        from generic_capture import BASE, HEAP
        pe = pefile.PE(self.profile['dll']['path'], fast_load=True)
        pe.parse_data_directories(directories=[pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_TLS']])
        self.tls_vector, self.tls_slot0 = HEAP + 0x380000, HEAP + 0x390000
        self.wq(HEAP + 0x370058, self.tls_vector)
        self.wq(self.tls_vector, self.tls_slot0)
        tls = getattr(pe, 'DIRECTORY_ENTRY_TLS', None)
        if tls is not None:
            image = pe.get_memory_mapped_image()
            template = bytes(image[tls.struct.StartAddressOfRawData - BASE:tls.struct.EndAddressOfRawData - BASE])
            if len(template) + tls.struct.SizeOfZeroFill > 0x10000:
                raise ValueError('bounded TLS template required')
            self.uc.mem_write(self.tls_slot0, template)

    # Static initializers and the effect constructors of some BX plug-ins build tens of thousands of heap objects (ADA STD-1: 140k mallocs,
    # >20M instructions); the shared 20M-instruction call budget is raised for this host only, still bounded.
    INSTRUCTION_BUDGET = 600_000_000

    def call_with_budget(self, fn, *args):
        """GenericCapture.call with this host's instruction budget."""
        from unicorn import UcError
        from unicorn.x86_const import UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_RAX
        from generic_capture import STACK, STOP, _ARGUMENTS
        self.failure = None
        rsp = STACK + 0xff008
        self.wq(rsp, STOP)
        for i, value in enumerate(args):
            if i < 4:
                self.uc.reg_write(_ARGUMENTS[i], value)
            else:
                self.wq(rsp + 0x28 + (i - 4) * 8, value)
        self.uc.reg_write(UC_X86_REG_RSP, rsp)
        try:
            self.uc.emu_start(fn, STOP, count=self.INSTRUCTION_BUDGET)
        except UcError as exc:
            raise RuntimeError('emulator at ' + hex(self.uc.reg_read(UC_X86_REG_RIP)) + ': ' + str(exc)) from exc
        if self.failure:
            raise RuntimeError(str(self.failure)) from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP) != STOP:
            raise RuntimeError('%dM instruction budget exceeded' % (self.INSTRUCTION_BUDGET // 1000000))
        self._check_completed_call()
        result = self.uc.reg_read(UC_X86_REG_RAX)
        if self.uninitialised_reads:   # DspApCapture.call's post-condition
            self.call_failed_uninitialised()
        return result

    def call(self, fn, *args):
        try:
            return self.call_with_budget(fn, *args)
        except Exception as exc:
            if os.environ.get('OA_BX_DEBUG'):
                from unicorn.x86_const import UC_X86_REG_RSP
                sp = self.uc.reg_read(UC_X86_REG_RSP)
                print('STACK', [hex(v) for v in (self.rq(sp + 8 * i) for i in range(0, 160)) if 0x180000000 <= v < 0x180150000][:24])
                print('LAST', self.last_unknown, [hex(x) for x in list(self.last_blocks)[-12:]], self.calls[-6:])
            raise

    # IUADPlug array calls: (ref, ptr, count, stride, flags). The BX adapter always passes flags=1 (the family-A plug-ins pass 0); it
    # is the same contiguous write at ref (memory<<24 | offset), recorded so the live gate, not this model, qualifies its meaning.
    def set_array(self):
        ref, ptr, count, stride, flags = [self.arg(i) for i in range(1, 6)]
        count, stride, flags = count & 0xffffffff, stride & 0xffffffff, flags & 0xffffffff
        if not (0 <= ref < 0x80000000 and 0 < count and stride == 1 and flags in (0, 1) and (ref & 0xffffff) + count <= 0x1000000):
            raise ValueError('unsupported shared array ABI ' + repr((ref, count, stride, flags)))
        values = [self.rd(ptr + 4 * i) for i in range(count)]
        self.shadow.update({ref + i: v for i, v in enumerate(values)})
        self.sequence_words.update({ref + i: None for i in range(count)})
        self.array_flags.add(flags)
        # One SetIntArray is one contiguous write; the carrier's cache codec bounds a write event at 4096 words (bx_masterdesk reloads 7304-8849 word
        # tables), so longer arrays are recorded as consecutive contiguous writes of the same words (the device writer sends 33-word blocks anyway).
        for first in range(0, count, 4096):
            self.note_loader_event()
            self.events.append(['write', ref + first, values[first:first + 4096]])
        return 0

    def get_array(self):
        """IUADPlug +0xd0/+0xd8 GetIntArray/GetFloatArray(ref, out*, count, stride): four arguments. UAD2SDK.dll's CUADPlug (IUADPlug subobject vtable
        0x180436e30) reads exactly [entry rsp+0x28] (stride) in both slots (0x18019f620, 0x18019f4e0) and forwards four values, while its +0xb0/+0xb8 set
        wrappers (0x18019fb50, 0x18019fa60) also read [entry rsp+0x30] (flags). A fifth value seen here is the caller's stack residue, never an argument."""
        ref, ptr, count, stride = [self.arg(i) for i in range(1, 5)]
        ref, count, stride = ref & 0xffffffff, count & 0xffffffff, stride & 0xffffffff
        if not (0 < count and stride == 1):
            raise ValueError('unsupported shared array ABI ' + repr((ref, count, stride)))
        for i in range(count):
            self.wd(ptr + 4 * i, self.shadow_word(ref + i))
        return 0

    def set_control(self, index, value):
        """Vst2Capture.set_control, plus the pinned `remapped` requests: a request the original's own setParameter override resolves to another
        step (Brainworx TMT Chan top step -> 1 - field(0.05) -> step 18). The live result must equal the pinned one; any other drift is an error."""
        mapped = self.vst2.get('remapped', {}).get(str(index), {}).get(str(value))
        if mapped is None:
            return super().set_control(index, value)
        if type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        self.set_parameter(index, value / self.maxima[index])
        self.values = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if self.values[index] != mapped:
            raise ValueError('original setter result differs from the pinned remap: %d -> %d != %d' % (value, self.values[index], mapped))
        self._verify_replay()
        return mapped, dict(self.shadow)

    def read_descriptor(self):
        """Original handler slot 3 (GetAllocInfo). The SDK asks for it before the first resume: the getter fills the
        memory -> descriptor-row map (adapter+0x248) that the DSP flush uses to form IUADPlug refs."""
        out = self.reserve(0x1000)
        before = dict(self.shadow)
        self.call(integer(self.vst2['descriptor']['getter_va']), self.handler, out)
        if self.shadow != before:
            raise ValueError('descriptor call touched DSP memory')
        return out

    def create_unison(self):
        self.unison_requests.append(tuple(self.arg(i) for i in range(3)))
        return 0

    def run_dllmain(self):
        """Original static CRT start-up exactly as the loader runs it: entry(hinstDLL, DLL_PROCESS_ATTACH, NULL)."""
        from generic_capture import BASE
        self.phase = 'dllmain'
        ok = self.call(integer(self.vst2['dllmain_va']), BASE, 1, 0)
        if ok & 0xffffffff != 1:
            raise ValueError('original DllMain(PROCESS_ATTACH) failed')

    def prepare_runtime(self):
        """Everything the original image needs before VSTPluginMain: intercepts, IUADPlug answers, SDK entry points, static CRT start-up."""
        spec = self.vst2
        for item in spec['intercepts']:
            self.intercept(item)
        for slot, answer in spec.get('iface_answers', {}).items():
            # 'host_channels': the instance's channel count (IUADPlug +0x28 in the Relab template), the same value the descriptor normalisation uses
            self.bind(integer(slot), 'iface_answer', (lambda: self.host_channels) if answer == 'host_channels' else (lambda answer=answer: answer))
        by_name = {(name.decode() if name else ''): stub for name, label, stub in self.imports}
        creator = next(stub for name, stub in by_name.items() if name.startswith('?UAD_CreateIUADPlug@'))
        self.handlers[creator] = ('UAD_CreateIUADPlug', self.create_iuadplug)
        self.unison_requests = []
        for name, stub in by_name.items():
            if name.startswith('?UAD_CreateIUADUnison@'):
                self.handlers[stub] = ('UAD_CreateIUADUnison', self.create_unison)   # no Unison session offline: NULL interface
        self.thread_runtime = bx_thread_runtime()(self)
        self.thread_runtime.bind()
        self.array_flags = set()
        self.setup_tls()
        self.run_dllmain()
        self.audio_master_calls, self.created = [], None

    def build_processor(self):
        spec = self.vst2
        self.prepare_runtime()
        master = self.stub('audioMaster', self.audio_master)
        self.aeffect = self.call(integer(spec['entry_va']), master)
        if not self.aeffect:
            raise ValueError('VSTPluginMain returned no effect')
        if self.rd(self.aeffect) != 0x56737450:
            raise ValueError('AEffect magic differs')
        self.this = self.parent = self.adapter = self.aeffect - integer(spec['aeffect_offset'])
        if self.rq(self.adapter) != integer(spec['processor_vtable']):
            raise ValueError('constructed UADAdapter vtable differs from the pinned one')
        self.effect = self.rq(self.adapter + integer(spec['effect_offset']))
        if self.rq(self.effect) != integer(spec['effect_vtable']):
            raise ValueError('constructed BXEffect vtable differs from the pinned one')
        self.handler = self.rq(self.adapter + integer(spec['handler']['pointer_offset']))
        if self.rq(self.handler) != integer(spec['handler']['vtable']):
            raise ValueError('constructed callback handler vtable differs from the pinned one')
        for slot, va in spec['handler']['slots'].items():
            if self.rq(integer(spec['handler']['vtable']) + integer(slot)) != integer(va):
                raise ValueError('handler vtable slot differs from the original image ' + slot)
        if self.created != (master, self.aeffect, self.handler, integer(spec['interface_version'])):
            raise ValueError('UAD_CreateIUADPlug arguments differ: ' + repr(self.created))
        if self.rq(self.adapter + integer(spec['iface_offset'])) != self.iface:
            raise ValueError('adapter did not store the IUADPlug it was given')
        if self.rd(self.aeffect + 0x2c) < len(spec['parameters']):
            raise ValueError('AEffect numParams smaller than the profile parameter count')
        self.dispatcher_fn = self.rq(self.aeffect + 8)
        self.set_parameter_fn = self.rq(self.aeffect + 0x18)
        self.get_parameter_fn = self.rq(self.aeffect + 0x20)
        host = spec['host']
        self.dispatch(OPCODES['open'])
        self.dispatch(OPCODES['set_sample_rate'], opt=float(host['sample_rate']))
        self.dispatch(OPCODES['set_block_size'], value=int(host['block_size']))
        self.uc.mem_write(self.fs, struct.pack('<f', float(host['sample_rate'])))
        self.read_descriptor()
        self.cells = []
        self.check_parameter_steps()

    def effect_steps(self, index):
        """BXEffect virtual +0x250: number of discrete values of a parameter (<= 1: continuous). The setter snaps to them."""
        return self.call(self.rq(self.rq(self.effect) + integer(self.vst2['steps_slot'])), self.effect, index) & 0xffffffff

    def check_parameter_steps(self):
        for i, p in enumerate(self.vst2['parameters']):
            actual = self.effect_steps(i)
            if (actual if actual > 1 else 1) != p['steps']:   # <= 1 (0 or 1): continuous
                raise ValueError('original BXEffect step count differs from the profile for parameter %d: %d != %d' % (i, actual, p['steps']))

    # --- parameter display: VST2 effGetParamDisplay through the original adapter (BX formatter) ---------------------
    def interface_0x67(self):
        return integer(self.vst2['interface_version']) == 0x67

    def format_parameter(self, index):
        before = dict(self.shadow)
        buf = self.reserve(64)
        self.uc.mem_write(buf, b'\0' * 64)
        if self.interface_0x67():
            # interface 0x67 generation (as sub-family C): the adapter's effGetParamDisplay stays empty; the Console text is handler slot 15
            # (UADAdapterPlugCallbackHandler: (this, index, -, float value, char* out) -> BXEffect virtual +0x2d8 (index, out, 8, value))
            from unicorn.x86_const import UC_X86_REG_XMM2
            from dspap_capture import F32BITS, F32
            self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(self.values[index] / self.maxima[index])))
            self.call(self.rq(self.rq(self.handler) + 0x78), self.handler, index, 0, buf)
            if not self.cstring(buf, 64):
                # the original label table holds an empty string for the step (e.g. Vitalizer MK3-T ProcMdl: label "" at 0x18017c256, TMT Chan: every
                # step); the Console lists the decimal step instead (captured: StepValue ["0", "1"], TMT Chan default "0"). A captured step list must agree.
                label = str(self.values[index])
                listed = self.vst2.get('step_labels', {}).get(str(index))
                if self.maxima[index] >= 1000 or (listed is not None and listed[self.values[index]] != label):
                    raise ValueError('original formatter gives empty text for parameter %d and the decimal step is not the captured Console text' % index)
                self.uc.mem_write(buf, label.encode('ascii') + b'\0')
        else:
            self.dispatch(7, index=index, pointer=buf)
        if before != self.shadow:
            raise ValueError('original formatter altered coefficient shadow')
        value = self.cstring(buf, 64).decode('ascii')
        return dict(raw=value, console_trimmed=value.strip())

    def start(self):
        self.phase = 'original_startup'
        self.dispatch(OPCODES['mains_changed'], value=1)  # resume(): sample rate, timing header, full BX DSP image
        if self.interface_0x67():   # there resume is only the SDK Resume notification (IUADPlug +0x160); the DSP image is handler slot 17 (load-time call)
            self.call(self.rq(self.rq(self.handler) + 0x88), self.handler)
        self.defaults_values = tuple(self.values)
        self.defaults = dict(self.shadow)
        self.startup_events = list(self.events)
        self.startup_formatted = [self.format_parameter(i) for i in range(len(self.maxima))]
        self._verify_replay()
        self.captured_properties = None
        shown = self.vst2.get('captured_text')
        waived = {int(k) for k in self.vst2.get('text_waivers', {})}
        if shown is not None:
            for i, text in enumerate(self.startup_formatted):
                if i not in waived and text['console_trimmed'] != shown[i]:
                    raise ValueError('captured original formatter mismatch ' + repr((i, text, shown[i])))
        self.phase = 'ready'


def extract_descriptor(cap, profile, decode_descriptor):
    """Original handler slot 3 on the constructed adapter; same decoder and same checks as sub-family A."""
    import vst2_host
    data = vst2_host.extract_descriptor(cap, profile, decode_descriptor)
    data['receiver_evidence']['family'] = 'BXEffect behind UADAdapter'
    return data
