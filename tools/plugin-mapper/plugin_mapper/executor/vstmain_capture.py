"""Offline executor for UAD plug-ins whose original VST2 entry can build the whole effect (JUCE VST2 wrapper + CUADPlugCallbackHandler).
First user: the JUCE 3.2 Sphere Mic Collection family (`SphereUADProcessor`); the profile (`dspap.vstmain`) carries everything plug-in specific.

Same host contract as dspap_capture.DspApCapture (CUADPlugCallbackHandler vtable + IUADPlug cache interface), but nothing is
synthesized: the whole original object graph is built by the plug-in's own code, the way a VST2 host does it (the same route
softube_capture.py takes):

  * the original `DllMain(DLL_PROCESS_ATTACH)` runs the statically linked CRT startup and every C++ dynamic initializer
    (juce::String::empty, the mic tables, ...), which is what a null-String crash in the 15 KB parameter-setup function lacked;
  * the original `VSTPluginMain(audioMaster)` -> JuceVSTWrapper -> `createPluginFilter` -> `SphereUADProcessor` constructor, and
    `UAD_CreateIUADPlug(host, AEffect*, handler, 0x67)` hands the plug-in the cache interface (the host returns the shared
    IUADPlug model); the handler it is given is the CUADPlugCallbackHandler the profile describes (19 slots);
  * a Console control is the VST2 `setParameter(index, float normalized)`: the processor's own parameter store recomputes the mic
    model and pushes memory 1 (170 words) and memory 2 (the mic impulse response taps) through IUADPlug set-array calls;
  * startup is the original handler slot 17 (`FUN_180267610`: memories 1, 2, 3 and 5), the descriptor handler slot 3, the Console
    text handler slot 15, exactly as in the other DSPAudioProcessor-family plug-ins.

The Win32 side is the bounded single-thread ABI of thread_runtime.py / softube_capture.py plus the few file-system / shell answers
JUCE's startup asks for; critical sections never contend (the processor places CRITICAL_SECTIONs at unaligned offsets, which the
owning CriticalSections model rejects, so they are plain no-ops here). Unknown imports and interface slots still reject.
Hardware is never touched.
"""
import hashlib
import struct
from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE
from unicorn.x86_const import UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_XMM0, UC_X86_REG_XMM2, UC_X86_REG_RSP, UC_X86_REG_RIP, UC_X86_REG_RAX as _RAX
from generic_capture import GenericCapture, integer, BASE, HEAP, STACK, STOP, UcError, _ARGUMENTS
from dspap_capture import DspApCapture, SCHEMA, F32, F32BITS, INTERNAL, Replay
import softube_capture as sc

VSTMAIN_KEY = 'vstmain'
OPCODES = dict(open=0, set_sample_rate=10, set_block_size=11, mains_changed=12)
CS_NAMES = ('EnterCriticalSection', 'LeaveCriticalSection', 'InitializeCriticalSection', 'DeleteCriticalSection',
            'InitializeCriticalSectionEx', 'InitializeCriticalSectionAndSpinCount', 'TryEnterCriticalSection')


class VirtualFiles:
    """Read-only file system of the host: only the data files the profile names (path on the original Windows install -> private
    file with a pinned sha256). Everything else is absent. Implements the Win32 calls original code uses to find and read them."""
    INVALID = 0xffffffffffffffff

    def __init__(self, cap, specs):
        import hashlib
        from pathlib import Path
        self.cap, self.files, self.handles, self.finds = cap, {}, {}, {}
        self.next_handle = 0x7c000000
        for spec in specs:
            path = Path(spec['host'])
            if hashlib.sha256(path.read_bytes()).hexdigest() != spec['sha256']:
                raise ValueError('data file identity differs: ' + spec['host'])
            self.files[self.norm(spec['guest'])] = dict(host=path, name=spec['guest'].rsplit('\\', 1)[-1], size=path.stat().st_size)

    @staticmethod
    def norm(path):
        return path.replace('/', '\\').lower().rstrip('\\')

    def wide(self, ptr):
        raw = bytes(self.cap.uc.mem_read(ptr, 1040))
        end = next((i for i in range(0, 1040, 2) if raw[i:i + 2] == b'\0\0'), None)
        if end is None:
            raise ValueError('bounded wide path required')
        return raw[:end].decode('utf-16-le')

    def is_dir(self, path):
        n = self.norm(path)
        return any(f.startswith(n + '\\') for f in self.files)

    def fail(self, error, result=0):
        self.cap.win32.last_error = error
        return result

    def attributes(self):
        n = self.norm(self.wide(self.cap.arg(0)))
        return 0x80 if n in self.files else 0x10 if self.is_dir(n) else self.fail(2, 0xffffffff)

    def attributes_ex(self):
        n = self.norm(self.wide(self.cap.arg(0)))
        if self.cap.arg(1) & 0xffffffff != 0 or not (n in self.files or self.is_dir(n)):
            return self.fail(2, 0)
        size = self.files[n]['size'] if n in self.files else 0
        data = struct.pack('<I', 0x80 if n in self.files else 0x10) + bytes(24) + struct.pack('<II', size >> 32, size & 0xffffffff)
        self.cap.uc.mem_write(self.cap.arg(2), data)
        return 1

    def create_file(self):
        path, access, disposition = self.wide(self.cap.arg(0)), self.cap.arg(1) & 0xffffffff, self.cap.arg(4) & 0xffffffff
        n = self.norm(path)
        if n not in self.files or disposition != 3 or access & 0x40000002:     # OPEN_EXISTING, no write access
            return self.fail(2 if n not in self.files else 5, self.INVALID)
        handle = self.next_handle
        self.next_handle += 16
        self.handles[handle] = [self.files[n], 0]
        return handle

    def read_file(self):
        handle, buf, count, got, overlapped = self.cap.arg(0), self.cap.arg(1), self.cap.arg(2) & 0xffffffff, self.cap.arg(3), self.cap.arg(4)
        if handle not in self.handles or overlapped:
            return self.fail(6, 0)
        entry = self.handles[handle]
        with entry[0]['host'].open('rb') as f:
            f.seek(entry[1])
            data = f.read(count)
        self.cap.uc.mem_write(buf, data)
        entry[1] += len(data)
        if got:
            self.cap.wd(got, len(data))
        return 1

    def seek(self, handle, distance, method):
        entry = self.handles[handle]
        entry[1] = {0: 0, 1: entry[1], 2: entry[0]['size']}[method] + distance
        return entry[1]

    def set_file_pointer_ex(self):
        handle, distance, new, method = self.cap.arg(0), self.cap.arg(1), self.cap.arg(2), self.cap.arg(3) & 0xffffffff
        if handle not in self.handles or method > 2:
            return self.fail(6, 0)
        distance = distance - (1 << 64) if distance >> 63 else distance
        position = self.seek(handle, distance, method)
        if new:
            self.cap.wq(new, position)
        return 1

    def set_file_pointer(self):
        # SetFilePointer(h, low, PLONG high, method): high points at the upper 32 bits (null = 32-bit distance) and gets the new
        # upper half back. JUCE FileInputStream always passes it (the .irz zip directory is at the end of the file).
        handle, distance, high, method = self.cap.arg(0), self.cap.arg(1) & 0xffffffff, self.cap.arg(2), self.cap.arg(3) & 0xffffffff
        if handle not in self.handles or method > 2:
            return self.fail(6, 0xffffffff)
        if high:
            distance |= self.cap.rd(high) << 32
            distance = distance - (1 << 64) if distance >> 63 else distance
        else:
            distance = distance - (1 << 32) if distance >> 31 else distance
        position = self.seek(handle, distance, method)
        if position < 0:
            return self.fail(131, 0xffffffff)       # ERROR_NEGATIVE_SEEK
        if high:
            self.cap.wd(high, position >> 32)
        return position & 0xffffffff

    def file_size_ex(self):
        handle = self.cap.arg(0)
        if handle not in self.handles:
            return self.fail(6, 0)
        self.cap.wq(self.cap.arg(1), self.handles[handle][0]['size'])
        return 1

    def file_type(self, fallback):
        return 1 if self.cap.arg(0) in self.handles else fallback()

    def close_handle(self, fallback):
        return (self.handles.pop(self.cap.arg(0)) and 1) if self.cap.arg(0) in self.handles else fallback()

    def find_data(self, entry):
        size = entry['size']
        name = entry['name'].encode('utf-16-le')
        return struct.pack('<I', 0x80) + bytes(24) + struct.pack('<II', size >> 32, size & 0xffffffff) + bytes(8) + name + bytes(592 - 44 - len(name))

    def find_first(self, data_arg):
        import fnmatch
        pattern = self.norm(self.wide(self.cap.arg(0)))
        folder, _, mask = pattern.rpartition('\\')
        matches = [f for f in sorted(self.files) if f.rpartition('\\')[0] == folder and fnmatch.fnmatch(f.rpartition('\\')[2], mask)]
        if not matches:
            return self.fail(2 if self.is_dir(folder) else 3, self.INVALID)
        handle = self.next_handle
        self.next_handle += 16
        self.finds[handle] = [self.files[f] for f in matches]
        self.cap.uc.mem_write(self.cap.arg(data_arg), self.find_data(self.finds[handle].pop(0)))
        return handle

    def find_next(self):
        pending = self.finds.get(self.cap.arg(0))
        if not pending:
            return self.fail(18, 0)
        self.cap.uc.mem_write(self.cap.arg(1), self.find_data(pending.pop(0)))
        return 1

    def find_close(self):
        self.finds.pop(self.cap.arg(0), None)
        return 1

    def table(self, fallback):
        """Handler entries to merge over the host's Win32 table (fallback: callables of the underlying implementations)."""
        return {'GetFileAttributesW': self.attributes, 'GetFileAttributesExW': self.attributes_ex, 'CreateFileW': self.create_file,
                'ReadFile': self.read_file, 'SetFilePointerEx': self.set_file_pointer_ex, 'SetFilePointer': self.set_file_pointer,
                'GetFileSizeEx': self.file_size_ex, 'GetFileType': lambda: self.file_type(fallback['GetFileType']),
                'CloseHandle': lambda: self.close_handle(fallback['CloseHandle']),
                'FindFirstFileW': lambda: self.find_first(1), 'FindFirstFileExW': lambda: self.find_first(2), 'FindNextFileW': self.find_next, 'FindClose': self.find_close}


class VstMainCapture(DspApCapture):
    """Same host API as GenericCapture (run_profile.py and the carrier provider call it)."""
    trace_blocks = False    # the per-block Python callback was ~half the Ocean Way construction (567M blocks in the .irz inflate); diagnostics only

    def __init__(self, profile, auto_start=True):
        spec = profile['dspap']
        sp = self.vstmain = spec[VSTMAIN_KEY]
        h = spec['handler']
        params = spec['parameters']
        dummy = dict(processor_vtable=h['vtable'], handler=dict(offset='0x0', vtable=h['vtable'], slots={}),
                     iface_offset='0x10', wrapper_offset='0x18', sample_rate_offset='0x30',
                     parameters=[dict(name=p['name'], pointer_offset='0x80000', maximum=p['maximum'], default=p['default'],
                                      raw_min=0.0, raw_max=1.0) for p in params],
                     quiet_iface_slots=[], plain_fields={}, startup=[], update=dict(va='0'), formatter=dict(va='0'),
                     descriptor=dict(getter_va=spec['descriptor']['getter_va']))
        DspApCapture.__init__(self, dict(profile, schema=SCHEMA, dspap=dummy), auto_start=False)
        self.profile = profile
        self.spec = spec
        self.phase = 'crt_startup'
        self.budget = int(sp['instruction_budget'])
        import pefile
        pe = pefile.PE(profile['dll']['path'], fast_load=True)
        pe.parse_data_directories(directories=[0, 9])
        # ThreadRuntime's critical-section registry installs a Python memory hook over the whole address space (it guards the
        # opaque RTL_CRITICAL_SECTION fields). This host never lets the original code reach that registry (critical sections are
        # plain no-ops below), and the hook costs a Python callback per load/store, so it is removed again.
        spy, installed = self.uc.hook_add, []

        def recording(kind, callback, *a, **k):
            handle = spy(kind, callback, *a, **k)
            if kind & (UC_HOOK_MEM_READ | UC_HOOK_MEM_WRITE) and not a and not k:
                installed.append(handle)
            return handle
        self.uc.hook_add = recording
        try:
            self.runtime = sc.SoftubeRuntime(self)
        finally:
            self.uc.hook_add = spy
        for handle in installed:
            self.uc.hook_del(handle)
        self.runtime.bind()
        self.win32 = sc.Win32(self, self.runtime)
        table = dict(self.win32.table(), **self.extra_win32())
        for name, fn in table.items():
            for n, label, stub in self.imports:
                if n is not None and n.decode() == name:
                    self.handlers[stub] = ('win32 ' + name, fn)
        for n, label, stub in self.imports:         # critical sections: single thread, never contend, any alignment
            if n is not None and n.decode() in CS_NAMES:
                self.handlers[stub] = ('critical section ' + n.decode(), (lambda: 1) if n.decode().startswith(('Try', 'InitializeCriticalSectionE', 'InitializeCriticalSectionA')) else (lambda: 0))
        if sp.get('data_files'):                    # the original install's data files (read-only), served at their Windows paths
            self.vfs = VirtualFiles(self, sp['data_files'])
            fallback = {n.decode(): self.handlers[stub][1] for n, label, stub in self.imports if n is not None and n.decode() in ('GetFileType', 'CloseHandle')}
            for name, fn in self.vfs.table(fallback).items():
                for n, label, stub in self.imports:
                    if n is not None and n.decode() == name:
                        self.handlers[stub] = ('virtual file ' + name, fn)
        # Loader TLS semantics (as GenericCapture): TEB TLS-vector pointer at GS:[0x58] -> slot 0 = copy of the PE TLS template,
        # the module's TLS index (0) stored at its AddressOfIndex.
        tls = pe.DIRECTORY_ENTRY_TLS.struct
        template = bytes(pe.get_memory_mapped_image()[tls.StartAddressOfRawData - BASE:tls.EndAddressOfRawData - BASE])
        if len(template) + tls.SizeOfZeroFill > 0x1000:
            raise ValueError('bounded TLS template required')
        self.tls_vector, self.tls_slot0 = HEAP + 0x380000, HEAP + 0x381000
        self.wq(HEAP + 0x370058, self.tls_vector)
        self.wq(self.tls_vector, self.tls_slot0)
        self.uc.mem_write(self.tls_slot0, template)
        self.wd(tls.AddressOfIndex, 0)
        self.sdk = []
        self.bind_sdk()
        self.install_native_routines()
        self.sdk_configured = False
        self.master_calls = []
        self.bind(0x28, 'iface_channels', lambda: self.host_channels)
        self.bind(0x30, 'iface_output_channels', lambda: self.host_channels)   # CUADPlug topology (as GenericCapture): the carrier's channel count
        self.bind(0x40, 'iface_sample_rate', self.iface_sample_rate)
        self.bind(0x118, 'iface_update_resource_profile', lambda: 0)   # UpdateRsrcProfile(0): nothing pending (power parameter)
        # UAD2SDK CUADPlug: the processing quantum is 0x40 until the device configuration is applied (IUADPlug +0x160, called by the
        # wrapper's resume), 0x20 afterwards (below 150 kHz); same model as softube_capture.py. SuprEsser DS shows it: its latency is
        # 210 - quantum, 146 while unconfigured and 178 = the captured Console latency once configured.
        self.bind(0x130, 'iface_processing_quantum', lambda: int(sp['host']['block_size']) if self.sdk_configured else 0x40)
        self.bind(0x160, 'iface_configure', self.iface_configure)
        self.bind(0x80, 'iface_flag_query', lambda: 0)   # wrapper constructor stores the answer in a byte nothing reads (inferred, unused)
        self.entry = pe.OPTIONAL_HEADER.AddressOfEntryPoint + BASE
        if self.call(self.entry, BASE, 1, 0) & 0xffffffff != 1:
            raise ValueError('original DllMain(DLL_PROCESS_ATTACH) failed')
        self.exports = {e.name.decode(): e.address + BASE for e in pe.DIRECTORY_ENTRY_EXPORT.symbols if e.name}
        self.phase = 'constructed_crt'
        self.open_effect()
        self.apply_raw_values()
        self.constructor_values = self.values
        self.phase = 'constructed'
        if auto_start:
            self.start()

    # --- host fixtures -----------------------------------------------------------------------------------------
    def extra_win32(self):
        """File system / shell / window-system answers the JUCE startup asks for (headless host, empty profile, no display):
        files and drives are absent (with the OS error the original code branches on), window and GDI calls succeed with
        inert handles, queries report nothing. Anything not listed here (or in softube_capture.Win32 / thread_runtime) rejects."""
        cap, a = self, self.arg

        def fail(error, result):
            def fn():
                cap.win32.last_error = error
                return result
            return fn

        def special_folder():           # SHGetSpecialFolderPathW(hwnd, buffer, csidl, create)
            text = 'C:\\Program Files' if a(2) & 0xffffffff == 0x26 else 'C:\\offline\\Special'
            cap.uc.mem_write(a(1), text.encode('utf-16-le') + b'\0\0')
            return 1

        def rect():
            cap.uc.mem_write(a(1), bytes(16))
            return 1

        def module_handle():            # GetModuleHandleW: NULL = this image; the virtual kernel32 family as ThreadRuntime models it; absent otherwise
            if a(0) == 0:
                return BASE
            try:
                return cap.runtime.GetModuleHandleW()
            except ValueError:
                cap.win32.last_error = 126
                return 0
        zero, one = (lambda: 0), (lambda: 1)
        handle = lambda n: (lambda: 0x7a000000 + n)
        resources = self.pe_resources()

        def find_resource():            # FindResourceA/W(hModule, name, type): the module's own resource directory (numeric ids only)
            hmod, name, kind = a(0), a(1), a(2)
            if hmod not in (0, BASE) or name >= 0x10000 or kind >= 0x10000 or (kind, name) not in resources:
                cap.win32.last_error = 1814     # ERROR_RESOURCE_NAME_NOT_FOUND
                return 0
            return 0x7b000000 + 16 * list(resources).index((kind, name))

        def resource_entry():
            handle = a(1) if cap.uc.reg_read(_ARGUMENTS[0]) in (0, BASE) and a(1) >= 0x7b000000 else a(0)
            return resources[list(resources)[(handle - 0x7b000000) // 16]]
        swap32 = lambda: int.from_bytes((a(0) & 0xffffffff).to_bytes(4, 'little'), 'big')      # WS2_32 byte-order conversions are pure
        swap16 = lambda: int.from_bytes((a(0) & 0xffff).to_bytes(2, 'little'), 'big')
        return {'ntohl': swap32, 'htonl': swap32, 'ntohs': swap16, 'htons': swap16, 'FindResourceA': find_resource, 'FindResourceW': find_resource,
                'LoadResource': lambda: a(1), 'FreeResource': zero, 'SizeofResource': lambda: resource_entry()[1],
                'LockResource': lambda: BASE + resources[list(resources)[(a(0) - 0x7b000000) // 16]][0],
                'GetCurrentThreadId': lambda: 0x2000, 'GetCurrentThread': lambda: 0xfffffffffffffffe, 'Sleep': zero, 'timeBeginPeriod': zero,
                'SHGetSpecialFolderPathW': special_folder, 'GetModuleHandleW': module_handle,
                'GetFileAttributesW': fail(2, 0xffffffff), 'GetFileAttributesExW': fail(2, 0), 'GetCurrentDirectoryW': zero,
                'GetNativeSystemInfo': zero, 'GetSystemDirectoryW': zero, 'GetVolumeInformationW': fail(5, 0), 'GetDriveTypeW': lambda: 3,
                'GetLogicalDriveStringsW': zero,
                # condition variables / SRW locks (MSVC STL, JUCE 7): single thread, never contended
                'WakeAllConditionVariable': zero, 'WakeConditionVariable': zero, 'SleepConditionVariableSRW': one,
                'TryAcquireSRWLockExclusive': one, 'AcquireSRWLockExclusive': zero, 'ReleaseSRWLockExclusive': zero, 'SwitchToThread': zero,
                'CoInitializeEx': zero, 'CoUninitialize': zero,
                # GDI / USER32 (fonts, desktop, message window): inert handles, empty metrics
                'CreateCompatibleDC': handle(1), 'DeleteDC': one, 'GetDeviceCaps': lambda: 96, 'SelectObject': handle(0x10), 'DeleteObject': one,
                'GetDC': handle(2), 'ReleaseDC': one, 'GetDesktopWindow': handle(3), 'AddFontMemResourceEx': handle(0x20),
                'RemoveFontMemResourceEx': one, 'SetMapperFlags': zero, 'SetMapMode': one, 'CreateFontIndirectW': handle(0x30),
                'GetTextMetricsW': zero, 'GetOutlineTextMetricsW': zero, 'EnumFontFamiliesExW': zero, 'GetGlyphIndicesW': lambda: 0xffffffff,
                'GetKerningPairsW': zero, 'GetGlyphOutlineW': lambda: 0xffffffff, 'GetObjectW': zero, 'SaveDC': one, 'RestoreDC': one,
                'SetWindowsHookExW': handle(0x40), 'UnhookWindowsHookEx': one, 'GetSystemMetrics': zero, 'SystemParametersInfoW': zero,
                'GetCursorPos': zero, 'GetKeyboardState': zero, 'GetAsyncKeyState': zero, 'PeekMessageW': zero, 'GetWindowLongPtrW': zero,
                'SetWindowLongPtrW': zero, 'EnumDisplayMonitors': one, 'GetMonitorInfoW': zero, 'LoadCursorW': handle(0x50), 'GetFocus': zero,
                'GetForegroundWindow': zero, 'GetActiveWindow': zero, 'GetCapture': zero, 'IsWindow': zero, 'GetWindowThreadProcessId': lambda: 0x2000,
                'GetWindowRect': rect, 'GetClientRect': rect, 'ShowWindow': zero, 'SetWindowPos': one, 'MoveWindow': one, 'DestroyWindow': one,
                'SetWindowTextW': one, 'GetWindowTextW': zero, 'IsWindowVisible': zero, 'GetParent': zero, 'GetAncestor': zero, 'IsChild': zero,
                'InvalidateRect': one, 'SendMessageW': zero, 'DefWindowProcW': zero, 'CallWindowProcW': zero, 'SetFocus': zero, 'ImmGetContext': zero,
                'ReleaseCapture': one, 'SetCapture': zero, 'UnregisterClassW': one, 'GetWindowLongW': zero, 'SetWindowLongW': zero,
                'SetLayeredWindowAttributes': one, 'RedrawWindow': one, 'ScreenToClient': one, 'WindowFromPoint': zero, 'GetWindowPlacement': zero,
                'GetWindowInfo': zero, 'GetUpdateRgn': zero, 'CreateRectRgn': handle(0x60), 'CombineRgn': one, 'GetRegionData': zero,
                'GetSystemMenu': zero, 'EnableMenuItem': zero, 'MessageBeep': one, 'GetClassNameA': zero, 'EnumChildWindows': one, 'EnumWindows': one,
                'GetMessageExtraInfo': zero, 'GetMessagePos': zero, 'GetMessageTime': zero, 'TrackMouseEvent': one, 'SetCursor': zero,
                'DestroyCursor': one, 'SetCursorPos': one, 'GetPixel': lambda: 0xffffffff, 'ExcludeClipRect': one, 'timeGetTime': lambda: 100000}

    def pe_resources(self):
        """{(type id, name id): (rva, size)} of the original image's resource directory (first language of each entry)."""
        import pefile
        pe = pefile.PE(self.profile['dll']['path'], fast_load=True)
        pe.parse_data_directories(directories=[2])
        table = {}
        for kind in getattr(getattr(pe, 'DIRECTORY_ENTRY_RESOURCE', None), 'entries', ()):
            for name in getattr(kind.directory, 'entries', ()):
                if kind.id is not None and name.id is not None and name.directory.entries:
                    data = name.directory.entries[0].data.struct
                    table[(kind.id, name.id)] = (data.OffsetToData, data.Size)
        return table

    def bind_sdk(self):
        def named(sym):
            return next(stub for n, label, stub in self.imports if n is not None and n.decode().startswith(sym))

        def create_plug():      # UAD_CreateIUADPlug(audioMaster, AEffect*, CUADPlugCallbackHandler*, version) -> IUADPlug*
            self.sdk.append(('CreateIUADPlug', [self.arg(i) & 0xffffffffffffffff for i in range(4)]))
            return self.iface
        self.handlers[named('?UAD_GetPluginBarHeight')] = ('UAD_GetPluginBarHeight', lambda: 40)   # editor geometry only
        self.handlers[named('?UAD_CreateIUADPlug')] = ('UAD_CreateIUADPlug', create_plug)

    def install_native_routines(self):
        """Original routines the host runs natively (profile `native_routines`): the routine and its callees must hash to the pinned image
        bytes; the guest entry becomes `mov rax, stub; jmp rax` and the stub answers like the original (then returns to the caller)."""
        for k, r in enumerate(self.vstmain.get('native_routines', ())):
            for part in [r] + list(r['callees']):
                body = bytes(self.uc.mem_read(integer(part['va']), int(part['size'])))
                if hashlib.sha256(body).hexdigest() != part['sha256']:
                    raise ValueError('native routine pin differs from the image: ' + r['name'] + ' ' + part['va'])
            stub = INTERNAL + 16 * (230 + k)
            self.handlers[stub] = ('native ' + r['name'], getattr(self, 'native_' + r['name'])(r))
            self.uc.mem_write(integer(r['va']), b'\x48\xb8' + struct.pack('<Q', stub) + b'\xff\xe0')     # mov rax, stub; jmp rax

    def native_xml_nth_child(self, r):
        """0x180119060 (rcx = holder whose first qword is an XML element, edx = index): the index-th child element named `literal`,
        0 when the element or the child is missing. Element: name String* at +0, first child at +8, next sibling at +0x10; the original
        compares names with wcscmp (equal pointers or equal text), so the host compares the UTF-16 text. Same walk as the original
        (first-child loop, then `index` next-sibling loops), without its per-step juce::String temporary."""
        va = integer(r['literal_va'])
        raw = bytes(self.uc.mem_read(va, 2 * (len(r['literal']) + 1)))
        if raw != r['literal'].encode('utf-16le') + b'\0\0':
            raise ValueError('native routine literal differs from the image')
        want, rq, read = raw, self.rq, self.uc.mem_read
        unpack = struct.Struct('<QQQ').unpack

        def next_named(node):       # first node from `node` along the sibling links whose name String equals the literal
            while node:
                name, _, sibling = unpack(read(node, 24))
                if bytes(read(name, len(want))) == want:
                    return node
                node = sibling
            return 0

        def run():
            element = rq(self.uc.reg_read(UC_X86_REG_RCX))
            index = self.uc.reg_read(UC_X86_REG_RDX) & 0xffffffff
            node = next_named(rq(element + 8)) if element else 0
            for _ in range(index):
                if not node:
                    break
                node = next_named(rq(node + 0x10))
            return node
        return run

    def iface_configure(self):
        self.sdk_configured = True
        return 0

    def iface_sample_rate(self):
        self.uc.reg_write(UC_X86_REG_XMM0, F32BITS(float(self.vstmain['host']['sample_rate'])))
        return 0

    MASTER = sc.SoftubeCapture.MASTER

    def audio_master(self):
        op = self.arg(1) & 0xffffffff
        self.master_calls.append(op)
        return 2400 if op == 1 else 0

    def call(self, fn, *args):
        """GenericCapture.call with the instruction budget of this plug-in's construction (the profile pins it)."""
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
            self.uc.emu_start(fn, STOP, count=getattr(self, 'budget', 20000000))
        except UcError as exc:
            raise RuntimeError('emulator at ' + hex(self.uc.reg_read(UC_X86_REG_RIP)) + ': ' + str(exc)) from exc
        if self.failure:
            raise RuntimeError(str(self.failure)) from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP) != STOP:
            raise RuntimeError('instruction budget exceeded')
        self._check_completed_call()
        return self.uc.reg_read(_RAX)

    # --- the VST2 object ---------------------------------------------------------------------------------------
    def open_effect(self):
        """The original VST2 entry: VSTPluginMain(audioMaster) -> JuceVSTWrapper -> SphereUADProcessor (AEffect*)."""
        sp = self.vstmain
        self.phase = 'vst_plugin_main'
        self.handlers[self.MASTER] = ('vst audioMaster', self.audio_master)
        self.effect = self.call(self.exports['VSTPluginMain'], self.MASTER)
        if not self.effect:
            raise ValueError('original VSTPluginMain returned no effect')
        calls = [c for c in self.sdk if c[0] == 'CreateIUADPlug']
        if len(calls) != 1 or calls[0][1][0] != self.MASTER or calls[0][1][1] != self.effect or calls[0][1][3] != int(sp['interface_version']):
            raise ValueError('UAD_CreateIUADPlug arguments differ: ' + repr(self.sdk))
        self.handler = calls[0][1][2]
        h = self.spec['handler']
        if sp.get('bootstrap'):         # discovery aid only: report the handler instead of checking it
            self.bootstrap_report = dict(handler=hex(self.handler), vtable=hex(self.rq(self.handler)), effect=hex(self.effect))
        elif self.rq(self.handler) != integer(h['vtable']):
            raise ValueError('CUADPlugCallbackHandler vtable differs from the profile: ' + hex(self.rq(self.handler)))
        for slot, va in ({} if sp.get('bootstrap') else h['slots']).items():
            if self.rq(integer(h['vtable']) + integer(slot)) != integer(va):
                raise ValueError('handler vtable slot differs from the original image ' + slot)
        if self.rd(self.effect) != 0x56737450:
            raise ValueError('AEffect magic differs')
        if self.rd(self.effect + 0x2c) != len(self.maxima):
            raise ValueError('AEffect numParams differs from the profile parameter count')
        self.wf(self.fs, float(sp['host']['sample_rate']))
        self.phase = 'vst_configure'
        self.dispatch(OPCODES['open'])
        self.dispatch(OPCODES['set_sample_rate'], opt=float(sp['host']['sample_rate']))
        self.dispatch(OPCODES['set_block_size'], value=int(sp['host']['block_size']))
        self.dispatch(OPCODES['mains_changed'], value=1)      # resume: the JUCE wrapper prepares the processor (rate, block size)

    def dispatch(self, opcode, index=0, value=0, ptr=0, opt=0.0):
        """AEffect::dispatcher(effect, opcode, index, value, ptr, opt) through the original entry."""
        return self.call(self.rq(self.effect + 8), self.effect, opcode, index, value, ptr, F32BITS(F32(opt)))

    def get_parameter(self, index):
        self.call(self.rq(self.effect + 0x20), self.effect, index)
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(UC_X86_REG_XMM0) & 0xffffffff))[0]

    def set_parameter(self, index, norm):
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(norm)))
        self.call(self.rq(self.effect + 0x18), self.effect, index, 0)

    def grid(self, index, norm):
        """Nearest integer on the profile grid for the plug-in float; ValueError when it is further than half a step off.
        Values the host sets are exact grid points and read back exactly; only a plug-in default like 0.1f sits between points."""
        top = self.maxima[index]
        value = round(norm * top)
        if abs(F32(value / top) - norm) > 0.5 / top * (1 + 1e-6):     # a tie (Ocean Way Far Mics 0.5 on 1/7) is within half a step
            raise ValueError('plug-in parameter %d value %r is off the profile grid 1/%d' % (index, norm, top))
        return value

    def build_processor(self):
        pass        # the object graph is built by the original VSTPluginMain (open_effect)

    def apply_raw_values(self):
        if not getattr(self, 'effect', 0) or self.vstmain.get('bootstrap'):
            return
        actual = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if actual != tuple(self.values):
            raise ValueError('captured defaults differ from the original constructor ' + repr((actual, tuple(self.values))))

    def format_parameter(self, index):
        """Console text of the value the plug-in holds: its own float when the integer view is that float's grid point (a default
        like 800 Hz is not on the 1/32768 grid and its text differs one step away), the grid point otherwise (host-set values)."""
        held = self.get_parameter(index)
        norm = held if self.grid(index, held) == self.values[index] else self.values[index] / self.maxima[index]
        fmt = self.spec['formatter']
        before = dict(self.shadow)
        buf = self.string_buffer
        self.uc.mem_write(buf, b'\0' * 64)
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(norm)))
        self.call(integer(fmt['va']), self.handler, index, 0, buf)
        raw = bytes(self.uc.mem_read(buf, 64))
        value = raw[:raw.index(b'\0')].decode('latin-1')
        if before != self.shadow:
            raise ValueError('original formatter altered coefficient shadow')
        return dict(raw=value, console_trimmed=value.strip())

    def start(self):
        DspApCapture.start(self)
        lat = self.vstmain.get('latency_check')
        if lat:     # after the original startup (handler slot 17): the Soup processor's own JUCE latencySamples equals the Console latency
            processor = self.rq(self.handler + integer(lat['handler_field']))
            if self.rd(processor + integer(lat['offset'])) != int(lat['expect']):
                raise ValueError('original processor latency differs from the captured Console latency: ' + str(self.rd(processor + integer(lat['offset']))))

    def _verify_replay(self):
        """DspApCapture's check (a replay of every event equals the complete shadow), made incremental: the event list only grows, so
        the replay keeps its state and applies the new events. Re-applying all ~30k startup events after every control was ~25 % of the Ocean Way run."""
        state = getattr(self, '_replay_state', None)
        if state is None or state[0] is not self.events or state[2] > len(self.events):
            state = self._replay_state = [self.events, Replay(), 0]
        try:
            state[1].apply(self.events[state[2]:])
            state[2] = len(self.events)
            if state[1].words != self.shadow:
                raise ValueError('shared event replay differs from complete original shadow')
        except Exception:
            self._replay_state = None
            raise

    def set_control(self, index, value):
        if type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        for need, level in (self.vstmain.get('control_requires', {}).get(str(index), ()) if value != self.values[index] else ()):
            if self.values[need] != level:      # ponytail: gated parameter (Ocean Way Predelay is zeroed by the original setter unless Mode = Reverb)
                self.set_control(need, level)
        self.phase = 'control_' + str(index)
        self.set_parameter(index, value / self.maxima[index])
        self.values = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if self.values[index] != value:
            raise ValueError('original setter did not keep the requested value')
        self._verify_replay()
        return value, dict(self.shadow)

