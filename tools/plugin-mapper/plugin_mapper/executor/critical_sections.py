"""Bounded Win64 critical-section API model; no OS synchronization or threads.

RTL_CRITICAL_SECTION is a 40-byte, 8-aligned opaque application object on Win64.
Only API ownership semantics are modeled. Original-code reads/writes of internal
fields reject: this is deliberately not a claimed Windows implementation layout.
The registry must be checkpointed alongside emulator memory in a future worker.
"""
from bisect import bisect_left, insort
from copy import deepcopy
from unicorn import UC_HOOK_MEM_READ, UC_HOOK_MEM_WRITE

class CriticalSections:
    SIZE = 40
    def __init__(self, host):
        self.host = host
        self.registry = {}
        self.starts = []    # sorted registry keys: the access check is a bisect, not a scan (the hook runs on every memory access)
        self.thread = 1
        self.events = []
        self.hooks = {}     # object start -> hook handle: only the registered objects (plus one access width below) trap into Python, not every memory access
        self.hooks[None] = host.uc.hook_add(UC_HOOK_MEM_READ | UC_HOOK_MEM_WRITE, self._opaque_access, begin=1 << 62, end=(1 << 62) + 1)   # keeps the memory-hook path armed
    ACCESS = 64     # widest x86 access (AVX-512): an access that starts below an object can still overlap it
    def _watch(self, p):
        self.hooks[p] = self.host.uc.hook_add(UC_HOOK_MEM_READ | UC_HOOK_MEM_WRITE, self._opaque_access, begin=max(1, p - self.ACCESS + 1), end=p + self.SIZE - 1)
    def _unwatch(self, p):
        self.host.uc.hook_del(self.hooks.pop(p))
    def _opaque_access(self, uc, access, address, size, value, unused):
        i = bisect_left(self.starts, address + size) - 1   # registered objects never overlap, so the last start below the access end is the only candidate
        if i >= 0 and address < self.starts[i] + self.SIZE:
            self.host.failure = RuntimeError('critical-section opaque field access unsupported')
            uc.emu_stop()
    def _pointer(self, p):
        if type(p) is not int or p == 0 or p % 8:
            raise ValueError('critical-section invalid pointer/alignment')
        # Require an entire writable mapped object before any registry mutation.
        if not any(start <= p and p + self.SIZE - 1 <= end and prot & 2
                   for start, end, prot in self.host.uc.mem_regions()):
            raise ValueError('critical-section object not wholly writable')
    def apply(self, operation, p):
        self._pointer(p)
        if operation == 'initialize':
            if p in self.registry or any(p < q+self.SIZE and q < p+self.SIZE for q in self.registry):
                raise ValueError('critical-section duplicate/overlapping initialization')
            self.registry[p] = (0, 0)
            insort(self.starts, p)
            self._watch(p)
            result = 0
        else:
            if p not in self.registry:
                raise ValueError('critical-section uninitialized')
            owner, depth = self.registry[p]
            if operation in ('enter', 'try'):
                if owner not in (0, self.thread):
                    if operation == 'enter':
                        raise RuntimeError('critical-section contention requires unsupported thread scheduling')
                    result = 0
                else:
                    if depth >= 0x7fffffff: raise ValueError('critical-section recursion overflow')
                    self.registry[p] = (self.thread, depth + 1)
                    result = int(operation == 'try')
            elif operation == 'leave':
                if owner != self.thread or depth == 0:
                    raise ValueError('critical-section release by nonowner')
                self.registry[p] = (owner, depth-1) if depth > 1 else (0,0)
                result = 0
            elif operation == 'delete':
                if depth: raise ValueError('critical-section delete while owned')
                del self.registry[p]
                del self.starts[bisect_left(self.starts, p)]
                self._unwatch(p)
                result = 0
            else: raise ValueError('critical-section unsupported operation')
        self.events.append((operation, p, self.thread, result))
        return result
    def checkpoint(self):
        return (self.thread, tuple(sorted(self.registry.items())), tuple(self.events))
    def restore(self, checkpoint):
        thread, entries, events = deepcopy(checkpoint)
        if thread != 1: raise ValueError('only the single emulator thread can be restored')
        new = {}
        for p, state in entries:
            self._pointer(p)
            owner, depth = state
            if owner not in (0,1) or type(depth) is not int or depth < 0 or depth > 0x7fffffff or bool(owner) != bool(depth):
                raise ValueError('invalid critical-section checkpoint')
            if p in new or any(p < q+self.SIZE and q < p+self.SIZE for q in new):
                raise ValueError('overlapping critical-section checkpoint')
            new[p] = (owner,depth)
        self.thread, self.registry, self.events = thread, new, list(events)
        for q in [q for q in self.hooks if q is not None]:
            self._unwatch(q)
        self.starts = sorted(new)
        for q in self.starts:
            self._watch(q)
    def bind(self):
        names = {b'InitializeCriticalSection':'initialize', b'EnterCriticalSection':'enter',
                 b'TryEnterCriticalSection':'try', b'LeaveCriticalSection':'leave',
                 b'DeleteCriticalSection':'delete'}
        found=[]
        for name,label,stub in self.host.imports:
            if name == b'GetCurrentThreadId' and label.startswith('KERNEL32.dll!'):
                self.host.handlers[stub]=(label,lambda:self.thread)
                found.append(label)
            if name in names and label.startswith('KERNEL32.dll!'):
                operation=names[name]
                self.host.handlers[stub]=(label, lambda op=operation:self.apply(op,self.host.arg(0)))
                found.append(label)
        return found
