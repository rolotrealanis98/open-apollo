"""Offline executor for the Softube-engine family (CSoftubeEffectUAD: AudioEffectX + CUADPlugCallbackHandler).

The plug-in's own original code is run under Unicorn as the VST2 host would: CRT/DllMain startup
(static initializers), then the exported VSTPluginMain, whose original factory builds the CSoftubeEffectUAD
object (model, engine, parameter table). Nothing about the object graph is synthesized; the only fixtures are
the Win32 ABI (single emulated thread), the VST2 audioMaster callback and the IUADPlug cache interface
(shared with GenericCapture/DspApCapture). Unknown imports reject.
"""
import struct
from dspap_capture import DspApCapture, PROC, F32, F32BITS
from generic_capture import integer, HEAP, STUBS, BASE
from thread_runtime import ThreadRuntime
import dspap_capture as dc

SCHEMA = 'openapollo-softube-profile/1'
FRAMEWORK = 'Softube'
OPCODES = dict(open=0, set_sample_rate=10, set_block_size=11, mains_changed=12)


class SoftubeRuntime(ThreadRuntime):
    """ThreadRuntime with the (larger) dynamic-export allowlist this CRT variant resolves at startup."""
    MODULES = ('kernel32', 'kernel32.dll', 'api-ms-win-core-synch-l1-2-0', 'api-ms-win-core-localization-l1-2-1',
               'api-ms-win-core-fibers-l1-1-1', 'api-ms-win-core-fibers-l1-1-0', 'api-ms-win-core-datetime-l1-1-1',
               'api-ms-win-core-string-l1-1-0', 'api-ms-win-core-file-l2-1-1', 'api-ms-win-appmodel-runtime-l1-1-1')
    EXPORTS = ('InitializeCriticalSectionEx', 'InitializeCriticalSectionAndSpinCount', 'FlsAlloc', 'FlsGetValue', 'FlsSetValue',
               'FlsFree', 'LCMapStringEx', 'SleepConditionVariableCS', 'WakeAllConditionVariable')

    # Optional Vista+/Win8+ exports the MSVC STL probes and falls back from when absent (threadpool, semaphores, packages, links).
    ABSENT = ('CreateEventExW', 'CreateSemaphoreW', 'CreateSemaphoreExW', 'CreateThreadpoolTimer', 'SetThreadpoolTimer',
              'WaitForThreadpoolTimerCallbacks', 'CloseThreadpoolTimer', 'CreateThreadpoolWait', 'SetThreadpoolWait',
              'CloseThreadpoolWait', 'FlushProcessWriteBuffers', 'FreeLibraryWhenCallbackReturns', 'GetCurrentProcessorNumber',
              'CreateSymbolicLinkW', 'GetCurrentPackageId', 'GetFileInformationByHandleEx', 'SetFileInformationByHandle',
              'GetSystemTimePreciseAsFileTime', 'WakeConditionVariable', 'TryAcquireSRWLockExclusive', 'SleepConditionVariableSRW',
              'CreateThreadpoolWork', 'SubmitThreadpoolWork', 'CloseThreadpoolWork', 'GetTickCount64', 'EnumSystemLocalesEx', 'GetUserDefaultLocaleName', 'IsValidLocaleName', 'GetDateFormatEx',
              'GetTimeFormatEx')

    def WideCharToMultiByte(self):
        if self.cap.arg(0) & 0xffffffff == 65001 and self.cap.arg(1) & 0xffffffff == 0 and 0 < self.cap.arg(3) & 0xffffffff <= 0x10000:
            n, dest, capacity = self.cap.arg(3) & 0xffffffff, self.cap.arg(4), self.cap.arg(5) & 0xffffffff
            out = bytes(self.cap.uc.mem_read(self.cap.arg(2), 2 * n)).decode('utf-16-le').encode('utf-8')
            if capacity == 0:
                return len(out)
            if capacity < len(out):
                self.cap.win32.last_error = 122
                return 0
            self.cap.uc.mem_write(dest, out)
            return len(out)
        if self.cap.arg(3) & 0xffffffff == 0xffffffff:  # NUL-terminated source: resolve the length (with terminator) first
            from unicorn.x86_const import UC_X86_REG_R9
            raw, end = b'', None
            while end is None and len(raw) < 0x10000:
                raw += bytes(self.cap.uc.mem_read(self.cap.arg(2) + len(raw), 2))
                end = len(raw) - 2 if raw[-2:] == b'\0\0' and len(raw) % 2 == 0 else None
            if end is None:
                raise ValueError('bounded NUL-terminated wide text required')
            self.cap.uc.reg_write(UC_X86_REG_R9, end // 2 + 1)
        return ThreadRuntime.WideCharToMultiByte(self)

    def MultiByteToWideChar(self):
        cp, flags = self.cap.arg(0) & 0xffffffff, self.cap.arg(1) & 0xffffffff
        if cp == 65001 and flags in (0, 8):  # UTF-8 (source text is ASCII in every startup caller)
            src, count, dest, cap_ = self.cap.arg(2), self.cap.arg(3) & 0xffffffff, self.cap.arg(4), self.cap.arg(5) & 0xffffffff
            raw = bytes(self.cap.uc.mem_read(src, 4096 if count == 0xffffffff else count))
            if count == 0xffffffff:
                raw = raw[:raw.index(b'\0') + 1]
            out = raw.decode('utf-8').encode('utf-16-le')
            if cap_ == 0:
                return len(out) // 2
            if cap_ < len(out) // 2:
                self.cap.win32.last_error = 122
                return 0
            self.cap.uc.mem_write(dest, out)
            return len(out) // 2
        if cp not in (0, 1252) or flags not in (0, 1, 8):
            raise ValueError('unmodeled multibyte conversion codepage/flags %r' % ((cp, flags),))
        return ThreadRuntime.MultiByteToWideChar(self)

    def GetCPInfo(self):
        if self.cap.arg(0) & 0xffffffff == 65001:
            ptr = self.cap.arg(1)
            self.cap.uc.mem_write(ptr, bytes(20))
            self.cap.wd(ptr, 4)
            self.cap.uc.mem_write(ptr + 4, b'?\0')
            return 1
        return ThreadRuntime.GetCPInfo(self)

    def IsValidCodePage(self):
        return 1 if self.cap.arg(0) & 0xffffffff == 65001 else ThreadRuntime.IsValidCodePage(self)

    def LCMapStringEx(self):
        from unicorn.x86_const import UC_X86_REG_RCX
        if self.cap.arg(0):
            if self._string(self.cap.arg(0), True).lower() not in ('en-us', 'en', 'invariant'):
                raise ValueError('unmodeled LCMapStringEx locale name')
            self.cap.uc.reg_write(UC_X86_REG_RCX, 0)  # BMP simple case mapping is locale independent for English
        try:
            return ThreadRuntime.LCMapStringEx(self)
        except ValueError as exc:
            raise ValueError(str(exc) + ' args=' + repr([hex(self.cap.arg(i)) for i in range(9)]))

    ARENA, ARENA_SIZE = 0x100000000, 0x20000000

    def _arena_alloc(self, size, zero):
        if not hasattr(self, 'arena_next'):
            self.cap.uc.mem_map(self.ARENA, self.ARENA_SIZE)
            self.arena_next, self.free_lists = self.ARENA, {}
        size = max(16, (size + 15) & ~15)
        free = self.free_lists.get(size)
        if free:
            ptr = free.pop()
            self.cap.uc.mem_write(ptr, bytes(size)) if zero else None
        else:
            if self.arena_next + size + 16 > self.ARENA + self.ARENA_SIZE:
                raise ValueError('bounded process heap exhausted')
            ptr = self.arena_next + 16
            self.arena_next = ptr + size
        self.blocks[ptr] = size
        return ptr

    def HeapAlloc(self):
        self._heap()
        flags, size = self.cap.arg(1) & 0xffffffff, self.cap.arg(2)
        if flags not in (0, 8) or not 0 < size <= 0x8000000:
            raise ValueError('bounded HeapAlloc flags/size required %r' % ((flags, size),))
        return self._arena_alloc(size, flags == 8)

    def HeapFree(self):
        self._heap()
        ptr = self.cap.arg(2)
        if self.cap.arg(1) & 0xffffffff != 0 or ptr not in self.blocks:
            raise ValueError('owned HeapFree block required')
        self.free_lists.setdefault(self.blocks.pop(ptr), []).append(ptr)
        return 1

    def HeapReAlloc(self):
        self._heap()
        flags, ptr, size = self.cap.arg(1) & 0xffffffff, self.cap.arg(2), self.cap.arg(3)
        if flags not in (0, 8) or ptr not in self.blocks or not 0 < size <= 0x8000000:
            raise ValueError('owned bounded HeapReAlloc required')
        old = self.blocks[ptr]
        new = self._arena_alloc(size, flags == 8)
        self.cap.uc.mem_write(new, bytes(self.cap.uc.mem_read(ptr, min(size, old))))
        self.free_lists.setdefault(self.blocks.pop(ptr), []).append(ptr)
        return new

    def _module(self, name):
        handle = next((h for h, n in self.modules.items() if n == name), None)
        if handle is None:
            handle = 0x75000000 + len(self.modules) * 0x1000
            self.modules[handle] = name
        return handle

    def LoadLibraryExW(self):
        name = self._string(self.cap.arg(0), True).lower()
        if name not in self.MODULES and not name.startswith('api-ms-win-'):
            raise ValueError('unmodeled virtual Win32 module ' + name)
        handle = self._module(name)
        self.events.append(('LoadLibraryExW', name, handle))
        return handle

    def GetProcAddress(self):
        handle = self.cap.arg(0)
        if handle not in self.modules:
            raise ValueError('owned virtual module required')
        name = self._string(self.cap.arg(1))
        extra = self.cap.win32.table()
        if name in self.ABSENT:
            self.events.append(('GetProcAddress', self.modules[handle], name, 'absent'))
            self.cap.win32.last_error = 127
            return 0
        if name not in self.EXPORTS and name not in extra:
            # UCRT/STL optional-API probe: an export the offline OS does not provide resolves NULL (original code falls back)
            self.events.append(('GetProcAddress', self.modules[handle], name, 'absent'))
            self.cap.win32.last_error = 127
            return 0
        if name not in self.exports:
            address = self.dynamic_next
            self.dynamic_next += 16
            self.cap.handlers[address] = ('bounded_dynamic_' + name, getattr(self, name) if name in self.EXPORTS else extra[name])
            self.exports[name] = address
        self.events.append(('GetProcAddress', self.modules[handle], name))
        return self.exports[name]


class Win32:
    """Extra single-thread Win32 ABI beyond ThreadRuntime: only what the original startup reaches."""

    def __init__(self, cap, tr):
        self.cap, self.tr = cap, tr
        self.tls = {}
        self.env = None
        self.last_error = 0
        self.init_once = {}

    def table(self):
        cap = self.cap
        a = cap.arg
        zero = lambda: 0
        one = lambda: 1
        t = {
            'GetCurrentProcessId': lambda: 0x1000, 'GetCurrentProcess': lambda: 0xffffffffffffffff,
            'GetTickCount': lambda: 100000, 'IsProcessorFeaturePresent': zero, 'IsDebuggerPresent': zero,
            'SetUnhandledExceptionFilter': zero, 'UnhandledExceptionFilter': zero,
            'EncodePointer': lambda: a(0), 'DecodePointer': lambda: a(0),
            'SetLastError': self.set_last_error, 'GetLastError': lambda: self.last_error,
            'TlsAlloc': self.tls_alloc, 'TlsGetValue': lambda: self.tls.get(a(0) & 0xffffffff, 0),
            'TlsSetValue': self.tls_set, 'TlsFree': one,
            'InitializeSListHead': self.slist_init, 'InterlockedFlushSList': zero,
            'InitializeSRWLock': zero, 'AcquireSRWLockShared': zero, 'AcquireSRWLockExclusive': zero,
            'ReleaseSRWLockShared': zero, 'ReleaseSRWLockExclusive': zero, 'InitializeConditionVariable': zero,
            'GetEnvironmentStringsW': self.env_strings, 'FreeEnvironmentStringsW': one,
            'GetFileType': lambda: 2, 'SetStdHandle': one, 'GetConsoleMode': zero, 'GetConsoleOutputCP': lambda: 437,
            'InitOnceExecuteOnce': self.init_once_exec,
            'GetModuleHandleExW': self.module_handle_ex, 'GetModuleFileNameW': self.module_filename,
            'GetUserDefaultLCID': lambda: 0x409, 'IsValidLocale': one,
            'GetLocaleInfoEx': zero, 'GetLocaleInfoW': zero, 'LCMapStringW': zero, 'CompareStringW': lambda: 2,
            'CompareStringEx': lambda: 2, 'EnumSystemLocalesW': one,
            'HeapSize': self.heap_size, 'RtlPcToFileHeader': self.pc_to_file_header,
            'SetEnvironmentVariableW': one, 'GetTimeZoneInformation': self.timezone,
            # Headless host with an empty profile: file system / registry / shell lookups report "absent" with the
            # OS error the original code branches on (ERROR_FILE_NOT_FOUND 2, ERROR_PATH_NOT_FOUND 3, access denied 5).
            'SHGetKnownFolderPath': self.known_folder, 'SHGetSpecialFolderPathA': zero, 'GetTempPathW': self.temp_path,
            'CreateFileW': lambda: self.fail(2, 0xffffffffffffffff), 'FindFirstFileW': lambda: self.fail(2, 0xffffffffffffffff),
            'FindFirstFileA': lambda: self.fail(2, 0xffffffffffffffff), 'FindFirstFileExW': lambda: self.fail(2, 0xffffffffffffffff),
            'FindNextFileW': lambda: self.fail(18, 0), 'FindNextFileA': lambda: self.fail(18, 0), 'FindClose': one,
            'CreateDirectoryW': lambda: self.fail(5, 0), 'DeleteFileW': lambda: self.fail(2, 0), 'MoveFileW': lambda: self.fail(2, 0),
            'MoveFileExW': lambda: self.fail(2, 0), 'CopyFileW': lambda: self.fail(2, 0), 'CreateHardLinkW': lambda: self.fail(5, 0),
            'SHCreateDirectoryExW': lambda: 5, 'SHFileOperationW': lambda: 0x402, 'ShellExecuteA': lambda: 2,
            'RegOpenKeyExW': lambda: 2, 'RegOpenKeyExA': lambda: 2, 'RegOpenKeyA': lambda: 2, 'RegQueryValueExW': lambda: 2,
            'RegQueryValueExA': lambda: 2, 'RegCreateKeyExA': lambda: 5, 'RegSetValueExA': lambda: 5, 'RegDeleteValueA': lambda: 2,
            'RegCloseKey': zero, 'CoInitializeEx': zero, 'CoInitialize': zero, 'CoUninitialize': zero, 'CoTaskMemFree': zero,
            'SysFreeString': zero, 'VariantClear': zero, 'PropVariantClear': zero, 'CloseHandle': one,
            'RtlCaptureContext': zero, 'GetSystemInfo': zero, 'AreFileApisANSI': one,
        }
        return t

    def fail(self, error, result):
        self.last_error = error
        return result

    def wide_out(self, text):
        raw = text.encode('utf-16-le') + b'\0\0'
        ptr = self.cap.reserve(len(raw))
        self.cap.uc.mem_write(ptr, raw)
        return ptr

    def known_folder(self):
        self.cap.wq(self.cap.arg(3), self.wide_out('C:\\offline\\KnownFolder'))
        return 0

    def temp_path(self):
        size, buf = self.cap.arg(0) & 0xffffffff, self.cap.arg(1)
        raw = 'C:\\offline\\Temp\\'.encode('utf-16-le') + b'\0\0'
        if size * 2 < len(raw):
            return len(raw) // 2
        self.cap.uc.mem_write(buf, raw)
        return len(raw) // 2 - 1

    def set_last_error(self):
        self.last_error = self.cap.arg(0) & 0xffffffff
        return 0

    def tls_alloc(self):
        i = next(i for i in range(64) if i not in self.tls)
        self.tls[i] = 0
        return i

    def tls_set(self):
        self.tls[self.cap.arg(0) & 0xffffffff] = self.cap.arg(1)
        return 1

    def slist_init(self):
        self.cap.uc.mem_write(self.cap.arg(0), bytes(16))
        return 0

    def env_strings(self):
        if self.env is None:
            self.env = self.cap.reserve(16)
            self.cap.uc.mem_write(self.env, bytes(16))
        return self.env

    def init_once_exec(self):
        # InitOnceExecuteOnce(once, fn, param, ctx): run the original callback once.
        once, fn, param, ctx = (self.cap.arg(i) for i in range(4))
        if once in self.init_once:
            return 1
        self.init_once[once] = 1
        self.cap.uc.mem_write(once, (2).to_bytes(8, 'little'))
        return 1

    def module_handle_ex(self):
        flags, name, out = self.cap.arg(0), self.cap.arg(1), self.cap.arg(2)
        self.cap.wq(out, BASE)
        return 1

    def module_filename(self):
        buf, size = self.cap.arg(1), self.cap.arg(2) & 0xffffffff
        text = 'C:\\offline\\plugin.dll'.encode('utf-16-le') + b'\0\0'
        self.cap.uc.mem_write(buf, text[:size * 2])
        return min(len(text) // 2 - 1, size)

    def heap_size(self):
        return self.tr.blocks.get(self.cap.arg(2), 0)

    def pc_to_file_header(self):
        pc, out = self.cap.arg(0), self.cap.arg(1)
        self.cap.wq(out, BASE)
        return BASE

    def timezone(self):
        self.cap.uc.mem_write(self.cap.arg(0), bytes(172))
        return 0


class SoftubeCapture(DspApCapture):
    """Same host API as GenericCapture (what run_profile.py and the carrier provider call)."""

    def __init__(self, profile, auto_start=True):
        spec = profile.get('softube')
        if profile.get('schema') != SCHEMA or profile.get('framework') != FRAMEWORK or not isinstance(spec, dict):
            raise ValueError('Softube profile required')
        self.soft = spec
        h = spec['handler']
        ho = integer(h['offset'])
        params = spec['parameters']
        dummy = dict(processor_vtable=spec['primary_vtable'],
                     handler=dict(offset=h['offset'], vtable=h['vtable'], slots={}),
                     iface_offset=hex(ho + 0x10), wrapper_offset=hex(ho + 0x18), sample_rate_offset='0x30',
                     parameters=[dict(name=p['name'], pointer_offset='0x80000', maximum=p['maximum'], default=p['default'],
                                      raw_min=0.0, raw_max=1.0) for p in params],
                     quiet_iface_slots=[], plain_fields={}, startup=[], update=dict(va='0'), formatter=dict(va='0'),
                     descriptor=dict(getter_va=spec['descriptor']['getter_va']))
        DspApCapture.__init__(self, dict(profile, schema=dc.SCHEMA, dspap=dummy), auto_start=False)
        self.profile = profile
        self.phase = 'crt_startup'
        import pefile
        pe = pefile.PE(profile['dll']['path'], fast_load=True)
        pe.parse_data_directories(directories=[0])
        self.runtime = SoftubeRuntime(self)
        self.runtime.bind()
        self.win32 = Win32(self, self.runtime)
        for name, fn in self.win32.table().items():
            for n, label, stub in self.imports:
                if n is not None and n.decode() == name:
                    self.handlers[stub] = ('win32 ' + name, fn)
        self.sdk = []
        self.bind_sdk()
        self.names_enabled = False
        self.sdk_configured = False  # UAD2SDK session state: set by IUADPlug slot 0x160 (device/rate configuration at resume)
        self.bind(0x20, 'iface_mode', lambda: 2 if self.sdk_configured else 1)  # 2 once +0x2ec (configured buffer) is nonzero
        self.bind(0x30, 'iface_flag', lambda: 0)  # CUADPlug+0x2d0 flag (0 until the driver sets it; no effect at 48 kHz)
        self.bind(0x40, 'iface_sample_rate', self.iface_sample_rate)  # UAD2SDK CUADPlug::GetSampleRate (float at +0x2e0)
        self.bind(0x88, 'iface_editor_query', lambda: int(self.names_enabled))  # editor-only query, answered 0
        self.bind(0x130, 'iface_vector_size', lambda: 0x20 if self.sdk_configured else 0x40)  # UAD2SDK: 0x20 below 150 kHz once configured
        self.bind(0x160, 'iface_configure', self.iface_configure)  # UAD2SDK applies the device configuration (called at resume)
        self.entry = pe.OPTIONAL_HEADER.AddressOfEntryPoint + BASE
        if self.call(self.entry, BASE, 1, 0) & 0xffffffff != 1:
            raise ValueError('original DllMain(DLL_PROCESS_ATTACH) failed')
        self.exports = {e.name.decode(): e.address + BASE for e in pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}
        self.phase = 'constructed_crt'
        self.open_effect()
        self.configure_host()
        self.apply_raw_values()  # captured defaults must equal the original constructor's state
        self.constructor_values = self.values
        self.phase = 'constructed'
        if auto_start:
            self.start()

    # --- host fixtures -------------------------------------------------------------------------------------
    def bind_sdk(self):
        def named(sym):
            return next(stub for n, label, stub in self.imports if n is not None and n.decode().startswith(sym))
        self.handlers[named('?UAD_GetPluginBarHeight')] = ('UAD_GetPluginBarHeight', lambda: 40)  # editor geometry only
        self.sdk_stubs = []

        def inert(label, answers=None, slots=0x40):  # editor / Unison side objects: modelled slots answer, every other rejects by name
            obj, vt = self.reserve(0x100), self.reserve(8 * slots)
            self.wq(obj, vt)
            for i in range(slots):
                stub = STUBS + 0xe000 + 16 * (len(self.sdk_stubs) + i)
                self.wq(vt + 8 * i, stub)
                answer = (answers or {}).get(8 * i)
                self.handlers[stub] = ('%s slot %#x' % (label, 8 * i), None) if answer is None else \
                    ('%s slot %#x' % (label, 8 * i), lambda a=answer: a[0])
                if answer is None:
                    self.handlers[stub] = ('unknown ' + self.handlers[stub][0], None)
            self.sdk_stubs.extend(range(slots))
            return obj

        def create_plug():  # UAD_CreateIUADPlug(audioMaster, AEffect*, CUADPlugCallbackHandler*, version) -> IUADPlug*
            self.sdk.append(('CreateIUADPlug', [self.arg(i) & 0xffffffffffffffff for i in range(4)]))
            return self.iface

        def create_bar():  # UAD_CreatePluginBar(hwnd, IUADPlug*, handler, height, version) -> IUADPluginBar*
            self.sdk.append(('CreatePluginBar', [self.arg(i) for i in range(5)]))
            return inert('IUADPluginBar')

        def create_unison():  # UAD_CreateIUADUnison(callbackA, callbackB, uint) -> IUADUnison*
            self.sdk.append(('CreateIUADUnison', [self.arg(i) for i in range(3)]))
            # UAD2SDK UADUnison slot 0x18 = byte +0x1d1 "Unison slot active": 0 for a plain insert, so the descriptor
            # carries no Unison resources (slot 0xb8 would append them).
            return inert('IUADUnison', {0x18: (0,)})
        self.handlers[named('?UAD_CreateIUADPlug')] = ('UAD_CreateIUADPlug', create_plug)
        self.handlers[named('?UAD_CreatePluginBar')] = ('UAD_CreatePluginBar', create_bar)
        self.handlers[named('?UAD_CreateIUADUnison')] = ('UAD_CreateIUADUnison', create_unison)

    def iface_configure(self):
        self.sdk_configured = True
        return 0

    def iface_sample_rate(self):
        from unicorn.x86_const import UC_X86_REG_XMM0
        self.uc.reg_write(UC_X86_REG_XMM0, F32BITS(float(self.soft['host']['sample_rate'])))
        return 0

    MASTER = STUBS + 0xf800

    def audio_master(self):
        # VST2 audioMasterCallback(AEffect*, opcode, index, value, ptr, opt): version handshake only; the rest is "unsupported" (0)
        op = self.arg(1) & 0xffffffff
        self.master_calls.append(op)
        return 2400 if op == 1 else 0

    # --- IUADPlug write model -------------------------------------------------------------------------------
    ARRAY_EVENT_LIMIT = 4096  # carrier cache_writes.py bound per write event; a longer array is recorded as consecutive events

    def set_array(self):
        # SetIntArray/SetFloatArray(ref, src, count, stride, flags): flags bit0 = force (write even when the cached word is
        # unchanged; analysis/chain-parameter-shadow-contract.txt). Every Softube write is forced (stride 1, flags 1), so the
        # shadow and the event stream are exactly the plug-in's writes.
        ref, src, count = self.arg(1), self.arg(2), self.arg(3) & 0xffffffff
        stride, flags = self.arg(4) & 0xffffffff, self.arg(5) & 0xffffffff
        if not (0 <= ref < 0x80000000 and 0 < count and stride == 1 and flags in (0, 1) and (ref & 0xffffff) + count <= 0x1000000):
            raise ValueError('unsupported shared array ABI ' + repr((ref, count, stride, flags)))
        words = struct.unpack('<%dI' % count, bytes(self.uc.mem_read(src, 4 * count)))
        live = {self.rd(c) for c in self.sequence_counters}
        for k in range(0, count, self.ARRAY_EVENT_LIMIT):
            chunk = list(words[k:k + self.ARRAY_EVENT_LIMIT])
            self.shadow.update({ref + k + n: v for n, v in enumerate(chunk)})
            self.sequence_words.update({ref + k + n: (v if v in live else None) for n, v in enumerate(chunk)})
            self.note_loader_event()
            self.events.append(['write', ref + k, chunk])
        return 0

    # --- the VST2 object -----------------------------------------------------------------------------------
    def open_effect(self):
        """The original VST2 entry: VSTPluginMain(audioMaster) -> factory -> CSoftubeEffectUAD (AEffect*)."""
        spec = self.soft
        self.phase = 'vst_plugin_main'
        self.master_calls = []
        self.handlers[self.MASTER] = ('vst audioMaster', self.audio_master)
        self.effect = self.call(self.exports['VSTPluginMain'], self.MASTER)
        if not self.effect:
            raise ValueError('original VSTPluginMain returned no effect')
        self.this = self.parent = self.effect - integer(spec['aeffect_offset'])
        self.handler = self.this + integer(spec['handler']['offset'])
        for off, va in ((0, spec['primary_vtable']), (integer(spec['handler']['offset']), spec['handler']['vtable'])):
            if self.rq(self.this + off) != integer(va):
                raise ValueError('constructed object vtable differs from the pinned one at +' + hex(off))
        for slot, va in spec['handler']['slots'].items():
            if self.rq(integer(spec['handler']['vtable']) + integer(slot)) != integer(va):
                raise ValueError('handler vtable slot differs from the original image ' + slot)
        if [tuple(c) for c in self.sdk if c[0] == 'CreateIUADPlug'] != [('CreateIUADPlug', [self.MASTER, self.effect, self.handler, int(spec['interface_version'])])]:
            raise ValueError('UAD_CreateIUADPlug arguments differ: ' + repr(self.sdk))
        if self.rd(self.effect) != 0x56737450:
            raise ValueError('AEffect magic differs')
        if self.rd(self.effect + 0x2c) != len(spec['parameters']):
            raise ValueError('AEffect numParams differs from the profile parameter count')
        self.wf(self.fs, float(spec['host']['sample_rate']))

    def configure_host(self):
        """Host configuration, in the order a VST2 host applies it after creation."""
        host = self.soft['host']
        self.dispatch(OPCODES['open'])
        self.dispatch(OPCODES['set_sample_rate'], opt=float(host['sample_rate']))
        self.dispatch(OPCODES['set_block_size'], value=int(host['block_size']))

    def dispatch(self, opcode, index=0, value=0, ptr=0, opt=0.0):
        """AEffect::dispatcher(effect, opcode, index, value, ptr, opt) through the original entry."""
        return self.call(self.rq(self.effect + 8), self.effect, opcode, index, value, ptr, F32BITS(F32(opt)))

    def text_op(self, opcode, index):
        self.names_enabled = True
        try:
            buf = self.string_buffer
            self.uc.mem_write(buf, b'\0' * 256)
            self.dispatch(opcode, index, 0, buf)
            raw = bytes(self.uc.mem_read(buf, 256))
            return raw[:raw.index(b'\0')].decode('latin-1')
        finally:
            self.names_enabled = False

    def get_parameter(self, index):
        from unicorn.x86_const import UC_X86_REG_XMM0
        self.call(self.rq(self.effect + 0x20), self.effect, index)
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(UC_X86_REG_XMM0) & 0xffffffff))[0]

    def set_parameter(self, index, norm):
        from unicorn.x86_const import UC_X86_REG_XMM2
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(norm)))
        self.call(self.rq(self.effect + 0x18), self.effect, index, 0)

    def grid(self, index, norm):
        """Integer on the profile grid for the plug-in float, or ValueError when it is off the grid."""
        top = self.maxima[index]
        value = round(norm * top)
        tolerance = 1e-6 if self.soft['parameters'][index].get('exact', True) else 0.5 / top  # off-grid captured default: nearest step
        if abs(F32(value / top) - norm) > tolerance:
            raise ValueError('plug-in parameter %d value %r is off the profile grid 1/%d' % (index, norm, top))
        return value

    def build_processor(self):
        pass  # the object graph is built by the original VSTPluginMain (open_effect)

    def apply_raw_values(self):
        if not getattr(self, 'effect', 0) or self.soft.get('bootstrap'):
            return  # base-class construction / discovery bootstrap (parameter table not yet derived)
        actual = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if actual != tuple(self.values):
            raise ValueError('captured defaults differ from the original constructor ' + repr((actual, tuple(self.values))))

    # --- host API used by run_profile.py / the carrier provider ----------------------------------------------
    def push(self):
        """The plug-in's own state -> DSP push: original CUADPlugCallbackHandler slot 0x88 (startup / apply parameters)."""
        self.call(self.rq(self.rq(self.handler) + integer(self.soft['startup']['slot'])), self.handler)

    def format_parameter(self, index):
        slot = self.rq(self.rq(self.handler) + 0x78)
        before = dict(self.shadow)
        buf = self.string_buffer
        self.uc.mem_write(buf, b'\0' * 64)
        from unicorn.x86_const import UC_X86_REG_XMM2
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(self.values[index] / self.maxima[index])))
        self.call(slot, self.handler, index, 0, buf)
        raw = bytes(self.uc.mem_read(buf, 64))
        value = raw[:raw.index(b'\0')].decode('latin-1')
        if before != self.shadow:
            raise ValueError('original formatter altered coefficient shadow')
        return dict(raw=value, console_trimmed=value.strip())

    def start(self):
        self.phase = 'original_startup'
        self.dispatch(OPCODES['mains_changed'], value=1)  # resume
        self.push()
        self.defaults_values = tuple(self.values)
        self.defaults = dict(self.shadow)
        self.startup_events = list(self.events)
        self.startup_formatted = [self.format_parameter(i) for i in range(len(self.maxima))]
        self._verify_replay()
        self.captured_properties = None
        shown = [p.get('captured_text') for p in self.soft['parameters']]
        for i, text in enumerate(self.startup_formatted):
            if shown[i] is not None and text['console_trimmed'] != shown[i]:
                raise ValueError('captured original formatter mismatch ' + repr((i, text, shown[i])))
        self.phase = 'ready'

    def set_control(self, index, value):
        if type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        self.set_parameter(index, value / self.maxima[index])
        self.push()
        self.values = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if self.values[index] != value:
            raise ValueError('original setter did not keep the requested value')
        self._verify_replay()
        return value, dict(self.shadow)


def descriptor_call(cap, profile):
    """Original handler slot 3 on the constructed effect with the host's channel request; returns the AllocInfo buffer.
    The channel request (one channel unless the carrier opts into stereo) is the plug-in's configured IO."""
    spec = profile['softube']['descriptor']
    fn = integer(spec['getter_va'])
    if cap.rq(cap.rq(cap.handler) + 0x18) != fn:
        raise ValueError('descriptor getter is not the original handler slot 3')
    requested = 2 if profile.get('stereo_channel_request') is True else 1
    gf = profile['softube']['getter_fields']
    for disp in (gf['num_inputs_disp'], gf['num_outputs_disp']):
        cap.wd(cap.handler + disp, requested)
    cap.wd(cap.effect + 0x30, requested)
    cap.wd(cap.effect + 0x34, requested)
    cap.host_channels = requested
    before = dict(cap.shadow)
    out = cap.reserve(0x1000)
    cap.call(fn, cap.handler, out)
    if any(cap.shadow.get(k) != v for k, v in before.items()) or cap.shadow.keys() - before.keys():
        raise ValueError('descriptor call touched DSP memory')
    return out, requested, fn


def descriptor_bills(cap, out):
    """The Bill records the getter returned (the engine patches the rate-specific data Bills in place, so their bytes are
    not the static ones in the plug-in image and a module store built from the image lacks them)."""
    raw = bytes(cap.uc.mem_read(out, 0xb20))
    bills = []
    for i in range(struct.unpack_from('<I', raw, 4)[0]):
        pointer = struct.unpack_from('<Q', raw, 8 + 8 * i)[0]
        size = struct.unpack('<5I', bytes(cap.uc.mem_read(pointer, 20)))[3]
        bills.append(bytes(cap.uc.mem_read(pointer, size + 20)))
    return bills


def extract_descriptor(cap, profile, decode_descriptor):
    """Original handler slot 3 (GetAllocInfo) on the constructed effect; decoded by the shared decoder."""
    from pathlib import Path
    out, requested, fn = descriptor_call(cap, profile)
    spec = profile['softube']['descriptor']
    data = decode_descriptor(cap, out, plugin=profile['name'], dll_sha256=profile['dll']['sha256'])
    if any(n not in (1, 2) for n in data['io']):
        raise ValueError('original IO capabilities outside mono/stereo')
    data['host_requested_io'] = [requested, requested]
    data['normalized_io'] = [min(requested, n) for n in data['io']]
    data['configured_input_channels'] = data['normalized_io'][0]
    data['channel_normalization_evidence'] = str(Path(__file__).resolve().parents[1] / 'carrier/channel-normalization.json')
    data['descriptor_function'] = hex(fn)
    data['receiver_evidence'] = dict(status='original_handler_slot3_descriptor', framework=FRAMEWORK,
                                     handler_offset=profile['softube']['handler']['offset'], vtable=profile['softube']['handler']['vtable'],
                                     getter_va=spec['getter_va'])
    return data
