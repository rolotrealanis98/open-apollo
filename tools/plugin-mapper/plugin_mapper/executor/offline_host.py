"""Private bounded Win64 coefficient-call mechanics. No native DLL execution.

Effect adapters own all constructors, startup order, imports and DSP contracts.
Every imported call is a rejecting trap until explicitly bound by that adapter.
"""
import hashlib
from pathlib import Path
import struct

import pefile
import pe_cache
from unicorn import Uc, UcError, UC_ARCH_X86, UC_MODE_64, UC_HOOK_CODE
from unicorn.x86_const import (
    UC_X86_REG_RAX, UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8,
    UC_X86_REG_R9, UC_X86_REG_RSP, UC_X86_REG_RIP,
)

BASE = 0x180000000
HEAP, STACK, STUBS, STOP = 0x60000000, 0x70000000, 0x71000000, 0x72000000
_ARGUMENTS = (UC_X86_REG_RCX, UC_X86_REG_RDX, UC_X86_REG_R8, UC_X86_REG_R9)


class OfflineHost:
    def __init__(self, dll, expected_sha256):
        raw = Path(dll).read_bytes()
        if hashlib.sha256(raw).hexdigest() != expected_sha256:
            raise ValueError('Plugin identity mismatch')
        pe, image = pe_cache.from_bytes(expected_sha256, raw)
        if pe.FILE_HEADER.Machine != 0x8664 or pe.OPTIONAL_HEADER.ImageBase != BASE:
            raise ValueError('Pinned Win64 image at the explicit base required')
        self._map_image(image)
        self.imports = []
        next_stub = STUBS + 0x3000
        for entry in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', ()):
            for imp in entry.imports:
                if next_stub >= STUBS + 0xf000:
                    raise ValueError('stub overflow')
                name = (entry.dll + b'!' + (imp.name or str(imp.ordinal).encode())).decode()
                self.handlers[next_stub] = ('unknown import ' + name, None)
                self.wq(imp.address, next_stub)
                self.imports.append((imp.name, name, next_stub))
                next_stub += 16

    def _map_image(self, image):
        self.uc = u = Uc(UC_ARCH_X86, UC_MODE_64)
        u.mem_map(BASE, (len(image) + 4095) & ~4095)
        u.mem_write(BASE, image)
        for addr, size in ((HEAP, 0x10000), (STACK, 0x100000),
                           (STUBS, 0x10000), (STOP, 0x1000)):
            u.mem_map(addr, size)
        u.mem_write(STUBS, b'\xc3' * 0x10000)
        self.handlers, self.calls, self.failure = {}, [], None
        u.hook_add(UC_HOOK_CODE, self.hook, begin=STUBS, end=STUBS + 0xffff)

    def wq(self, p, v): self.uc.mem_write(p, struct.pack('<Q', v))
    def wd(self, p, v): self.uc.mem_write(p, struct.pack('<I', v))
    def wf(self, p, v): self.uc.mem_write(p, struct.pack('<f', v))
    def rd(self, p): return struct.unpack('<I', self.uc.mem_read(p, 4))[0]

    def arg(self, n):
        if n < 4:
            return self.uc.reg_read(_ARGUMENTS[n])
        return struct.unpack('<Q', self.uc.mem_read(
            self.uc.reg_read(UC_X86_REG_RSP) + 0x28 + (n - 4) * 8, 8))[0]

    def hook(self, u, addr, size, unused):
        name, fn = self.handlers.get(addr, ('unbound stub ' + hex(addr), None))
        if fn is None:
            self.failure = RuntimeError(name)
            u.emu_stop()
            return
        try:
            u.reg_write(UC_X86_REG_RAX, fn() or 0)
            self.calls.append(name)
        except Exception as exc:
            self.failure = exc
            u.emu_stop()

    def _check_completed_call(self):
        """Adapter hook for invariants such as complete atomic brackets."""

    def call(self, fn, *args):
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
            self.uc.emu_start(fn, STOP, count=2000000)
        except UcError as exc:
            raise RuntimeError(f'emulator failure at{self.uc.reg_read(UC_X86_REG_RIP):x}: {exc}') from exc
        if self.failure:
            raise RuntimeError(str(self.failure)) from self.failure
        if self.uc.reg_read(UC_X86_REG_RIP) != STOP:
            raise RuntimeError('instruction budget exceeded')
        self._check_completed_call()
        return self.uc.reg_read(UC_X86_REG_RAX)
