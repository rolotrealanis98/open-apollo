"""Bounded single-thread Win32 FLS/heap ABI for original CRT code.

No OS allocation, code loading, device, callback execution, or DLL field seeding.
Original CRT initializers must allocate their own FLS index and obtain the heap.
"""
from pathlib import Path
import sys,json,hashlib
from functools import lru_cache

@lru_cache(maxsize=1)
def nls_tables():
    raw=Path(__file__).with_name("win32-ctype.json").read_bytes()
    if hashlib.sha256(raw).hexdigest()!="8dc7ccec70eb18271031441cffc2c032c16b6b7f6a160686beda395eabcc118e":
        raise ValueError("pinned Win32 NLS artifact identity differs")
    return json.loads(raw)
# Reuse the previously verified opaque ownership model, without altering it.
from critical_sections import CriticalSections

class ThreadRuntime:
    def __init__(self,cap):
        self.cap=cap;self.fls={};self.callbacks={};self.heap=0x74000000
        self.heap_obtained=False;self.blocks={};self.events=[]
        self.critical_sections=CriticalSections(cap)
        self.modules={};self.exports={};self.dynamic_next=0x7100f000
        self.ole_count=0;self.apartment_mode=None
        self.window_classes={};self.next_window_class_atom=1
        self.posted_messages=[];self.windows={};self.next_window_handle=0x76000000
        self.event_handles={};self.next_event_handle=0x78000000
        self.clock_frequency=10000000;self.clock_ticks=10000000
    def bind(self):
        found=self.critical_sections.bind()
        for name,label,stub in self.cap.imports:
            if name is None:continue
            key=name.decode('ascii')
            if key in ('FlsAlloc','FlsGetValue','FlsSetValue','FlsFree','GetProcessHeap','HeapAlloc','HeapFree','HeapReAlloc','InitializeCriticalSectionEx','InitializeCriticalSectionAndSpinCount','LoadLibraryExW','GetProcAddress','FreeLibrary','GetStartupInfoW','GetStdHandle','GetCommandLineA','GetCommandLineW','GetACP','GetOEMCP','GetCPInfo','IsValidCodePage','MultiByteToWideChar','GetStringTypeW','WideCharToMultiByte','OleInitialize','OleUninitialize','QueryPerformanceCounter','QueryPerformanceFrequency','timeGetTime','GetSystemTimeAsFileTime','GetModuleHandleW','RegisterClassExW','CreateWindowExW','CreateEventW','SetEvent','ResetEvent','CloseHandle','WaitForSingleObject','PostMessageW'):
                self.cap.handlers[stub]=('bounded_thread_'+key,getattr(self,key));found.append(key)
        return found
    def FlsAlloc(self):
        callback=self.cap.arg(0)
        if callback and not any(a<=callback<=b and a>=0x180000000 for a,b,p in self.cap.uc.mem_regions()):
            raise ValueError('FLS callback must belong to mapped original image')
        index=next((i for i in range(128)if i not in self.fls),None)
        if index is None:self.cap.last_error=8;return 0xffffffff
        self.fls[index]=0;self.callbacks[index]=callback;self.events.append(('FlsAlloc',index,callback));return index
    def _index(self):
        index=self.cap.arg(0)&0xffffffff
        if index not in self.fls:raise ValueError('original CRT used unallocated FLS index '+hex(index))
        return index
    def FlsGetValue(self):
        index=self._index();self.cap.last_error=0;return self.fls[index]
    def FlsSetValue(self):
        index=self._index();value=self.cap.arg(1)
        self.fls[index]=value;self.events.append(('FlsSetValue',index,value));return 1
    def FlsFree(self):
        index=self._index()
        if self.fls[index] and self.callbacks[index]:raise ValueError('FLS finalizer execution is outside startup contract')
        del self.fls[index];del self.callbacks[index];return 1
    def GetProcessHeap(self):
        self.heap_obtained=True;self.events.append(('GetProcessHeap',self.heap));return self.heap
    def _heap(self):
        if not self.heap_obtained or self.cap.arg(0)!=self.heap:raise ValueError('original CRT heap initializer required')
    def HeapAlloc(self):
        self._heap();flags=self.cap.arg(1)&0xffffffff;size=self.cap.arg(2)
        if flags not in (0,8) or not 0<size<=0x100000:raise ValueError('bounded HeapAlloc flags/size required')
        ptr=self.cap.reserve(size);self.blocks[ptr]=size
        self.events.append(('HeapAlloc',flags,size,ptr));return ptr
    def HeapFree(self):
        self._heap();flags=self.cap.arg(1)&0xffffffff;ptr=self.cap.arg(2)
        if flags!=0 or ptr not in self.blocks:raise ValueError('owned HeapFree block required')
        del self.blocks[ptr];self.events.append(('HeapFree',ptr));return 1
    def HeapReAlloc(self):
        self._heap();flags=self.cap.arg(1)&0xffffffff;ptr=self.cap.arg(2);size=self.cap.arg(3)
        if flags not in (0,8) or ptr not in self.blocks or not 0<size<=0x100000:raise ValueError('owned bounded HeapReAlloc required')
        new=self.cap.reserve(size);self.cap.uc.mem_write(new,bytes(self.cap.uc.mem_read(ptr,min(size,self.blocks[ptr]))))
        del self.blocks[ptr];self.blocks[new]=size;self.events.append(('HeapReAlloc',ptr,new,size));return new

    def InitializeCriticalSectionEx(self):
        if self.cap.arg(1)>0x100000 or self.cap.arg(2)!=0:
            raise ValueError('bounded source-used critical section flags/spin required')
        self.critical_sections.apply('initialize',self.cap.arg(0));return 1
    def InitializeCriticalSectionAndSpinCount(self):
        if self.cap.arg(1)>0x100000:raise ValueError('bounded critical section spin required')
        self.critical_sections.apply('initialize',self.cap.arg(0));return 1

    def _string(self,ptr,wide=False):
        unit=2 if wide else 1
        raw=bytes(self.cap.uc.mem_read(ptr,512))
        end=next((i for i in range(0,512,unit)if raw[i:i+unit]==bytes(unit)),None)
        if end is None:raise ValueError('bounded Win32 symbol required')
        return raw[:end].decode('utf-16-le' if wide else 'ascii')
    def LoadLibraryExW(self):
        name=self._string(self.cap.arg(0),True).lower()
        if self.cap.arg(1)!=0 or self.cap.arg(2)not in(0,0x800):raise ValueError('unexpected virtual module flags')
        if name not in ('kernel32','kernel32.dll','api-ms-win-core-synch-l1-2-0','api-ms-win-core-localization-l1-2-1'):
            raise ValueError('unmodeled virtual Win32 module '+name)
        handle=next((h for h,n in self.modules.items()if n==name),None)
        if handle is None:
            handle=0x75000000+len(self.modules)*0x1000;self.modules[handle]=name
        self.events.append(('LoadLibraryExW',name,handle));return handle
    def GetProcAddress(self):
        handle=self.cap.arg(0)
        if handle not in self.modules:raise ValueError('owned virtual module required')
        name=self._string(self.cap.arg(1))
        allowed=('InitializeCriticalSectionEx','InitializeCriticalSectionAndSpinCount','FlsAlloc','FlsGetValue','FlsSetValue','FlsFree','LCMapStringEx','SleepConditionVariableCS','WakeAllConditionVariable')
        if name not in allowed:raise ValueError('unmodeled dynamic Win32 export '+name)
        if name not in self.exports:
            address=self.dynamic_next;self.dynamic_next+=16
            if self.dynamic_next>0x71010000:raise ValueError('bounded dynamic exports exhausted')
            self.cap.handlers[address]=('bounded_dynamic_'+name,getattr(self,name));self.exports[name]=address
        self.events.append(('GetProcAddress',self.modules[handle],name));return self.exports[name]
    def FreeLibrary(self):
        if self.cap.arg(0)not in self.modules:raise ValueError('owned virtual module required')
        self.events.append(('FreeLibrary',self.cap.arg(0)));return 1

    def RegisterClassExW(self):
        ptr=self.cap.arg(0)
        size=self.cap.rd(ptr)
        if size!=80:raise ValueError('bounded Win64 WNDCLASSEXW record required')
        wndproc=self.cap.rq(ptr+8)
        if not 0x180000000<=wndproc<0x184000000:
            raise ValueError('window class procedure must belong to mapped original image')
        class_name=self.cap.rq(ptr+64)
        if not class_name:raise ValueError('window class name required')
        raw=bytes(self.cap.uc.mem_read(class_name,512))
        end=next((i for i in range(0,len(raw),2) if raw[i:i+2]==b'\0\0'),None)
        if end is None:raise ValueError('bounded window class name required')
        name=raw[:end].decode('utf-16-le')
        if not name:raise ValueError('nonempty window class name required')
        if name not in self.window_classes:
            if self.next_window_class_atom>0xbfff:raise ValueError('bounded window class atom space exhausted')
            self.window_classes[name]=self.next_window_class_atom;self.next_window_class_atom+=1
        atom=self.window_classes[name];self.events.append(('RegisterClassExW',name,hex(wndproc),atom));return atom

    def CreateWindowExW(self):
        class_name=self.cap.arg(1)
        if class_name<=0xffff:
            atom=class_name
            if atom not in self.window_classes.values():raise ValueError('registered window class atom required')
        else:
            raw=bytes(self.cap.uc.mem_read(class_name,512))
            end=next((i for i in range(0,len(raw),2) if raw[i:i+2]==b'\0\0'),None)
            if end is None:raise ValueError('bounded window class name required')
            name=raw[:end].decode('utf-16-le')
            if name not in self.window_classes:raise ValueError('registered window class name required')
            atom=self.window_classes[name]
        handle=self.next_window_handle;self.next_window_handle+=0x100
        if self.next_window_handle>0x77000000:raise ValueError('bounded headless window space exhausted')
        self.windows[handle]=dict(class_atom=atom,parent=self.cap.arg(8),style=self.cap.arg(3)&0xffffffff)
        self.events.append(('CreateWindowExW',hex(handle),atom,self.windows[handle]['style']))
        return handle

    def GetStartupInfoW(self):
        ptr=self.cap.arg(0)
        self.cap.uc.mem_write(ptr,bytes(104));self.cap.wd(ptr,104)
        self.events.append(('GetStartupInfoW','headless_no_standard_handles'));return 0

    def GetStdHandle(self):
        identifier=self.cap.arg(0)&0xffffffff
        if identifier not in(0xfffffff6,0xfffffff5,0xfffffff4):raise ValueError('unknown standard handle')
        self.events.append(('GetStdHandle',identifier,'absent'));return 0

    def _command_line(self,wide):
        key='command_w' if wide else 'command_a'
        if not hasattr(self,key):
            raw=('offline-host.exe\0').encode('utf-16-le'if wide else'ascii')
            ptr=self.cap.reserve(len(raw));self.cap.uc.mem_write(ptr,raw);setattr(self,key,ptr)
        return getattr(self,key)
    def GetCommandLineA(self):return self._command_line(False)
    def GetCommandLineW(self):return self._command_line(True)

    def GetACP(self):return 1252
    def GetOEMCP(self):return 437
    def GetCPInfo(self):
        cp=self.cap.arg(0)&0xffffffff;ptr=self.cap.arg(1)
        if cp not in(0,1252,437):raise ValueError('unmodeled code page '+str(cp))
        self.cap.uc.mem_write(ptr,bytes(20));self.cap.wd(ptr,1);self.cap.uc.mem_write(ptr+4,b'?\0');return 1

    def IsValidCodePage(self):
        cp=self.cap.arg(0)&0xffffffff
        if cp not in(1252,437):raise ValueError('unmodeled code page validation '+str(cp))
        return 1

    def MultiByteToWideChar(self):
        cp=self.cap.arg(0)&0xffffffff;flags=self.cap.arg(1)&0xffffffff
        source=self.cap.arg(2);count=self.cap.arg(3)&0xffffffff;dest=self.cap.arg(4);capacity=self.cap.arg(5)&0xffffffff
        if cp not in(0,1252)or flags not in(0,1,8):raise ValueError('unmodeled multibyte conversion flags/codepage')
        if count==0xffffffff:
            raw=bytes(self.cap.uc.mem_read(source,65536));stop=raw.find(b'\0')
            if stop<0:raise ValueError('bounded null-terminated ANSI text required')
            raw=raw[:stop+1]
        elif 0<count<=65536:raw=bytes(self.cap.uc.mem_read(source,count))
        else:raise ValueError('bounded multibyte count required')
        import json,struct
        table=nls_tables()['cp1252']['mb']
        result=struct.pack('<'+str(len(raw))+'H',*[table[x]for x in raw]);required=len(result)//2
        if capacity==0:return required
        if capacity<required:self.cap.last_error=122;return 0
        self.cap.uc.mem_write(dest,result);return required

    def GetStringTypeW(self):
        import json,struct
        kind=self.cap.arg(0);source=self.cap.arg(1);count=self.cap.arg(2)&0xffffffff;dest=self.cap.arg(3)
        if kind not in(1,2,4)or not 0<count<=65536:raise ValueError('bounded character classification required')
        if not hasattr(self,'ctype'):
            self.ctype=nls_tables()['values']
        codes=struct.unpack('<'+str(count)+'H',self.cap.uc.mem_read(source,count*2))
        output=[self.ctype[ch][kind//2]for ch in codes]
        self.cap.uc.mem_write(dest,struct.pack('<'+str(count)+'H',*output));return 1

    def LCMapStringEx(self):
        import json,struct
        locale,flags,source,count,dest,capacity,version,reserved,handle=[self.cap.arg(i)for i in range(9)]
        flags&=0xffffffff;count&=0xffffffff;capacity&=0xffffffff
        if locale or flags not in(0x100,0x200)or version or reserved or handle or not 0<count<=65536:
            raise ValueError('bounded nonlinguistic BMP case mapping required')
        codes=struct.unpack('<'+str(count)+'H',self.cap.uc.mem_read(source,count*2))
        if any(0xd800<=c<=0xdfff for c in codes):raise ValueError('surrogate case mapping is outside CRT fixture')
        if capacity==0:return count
        if capacity<count:self.cap.last_error=122;return 0
        table=nls_tables()['lower'if flags==0x100 else'upper']
        if not dest:raise ValueError('LCMap null destination with capacity '+repr((count,capacity)))
        self.cap.uc.mem_write(dest,struct.pack('<'+str(count)+'H',*[table[ch]for ch in codes]));return count

    def WideCharToMultiByte(self):
        import struct
        cp,flags,source,count,dest,capacity,default,used=[self.cap.arg(i)for i in range(8)]
        cp&=0xffffffff;flags&=0xffffffff;count&=0xffffffff;capacity&=0xffffffff
        if cp not in(0,1252)or flags not in(0,0x400)or not 0<count<=65536 or default:
            raise ValueError('bounded strict CP1252 conversion required '+repr((cp,flags,count,default)))
        import json
        codes=struct.unpack('<'+str(count)+'H',self.cap.uc.mem_read(source,count*2));result=[];used_default=False
        table=nls_tables()['cp1252']
        for code in codes:
            value=table['wc'][code]
            if ((flags&0x400)or value==table['default'])and table['mb'][value]!=code:
                value=table['default'];used_default=True
            result.append(value)
        if used:self.cap.wd(used,int(used_default))
        if capacity==0:return len(result)
        if capacity<len(result):self.cap.last_error=122;return 0
        self.cap.uc.mem_write(dest,bytes(result));return len(result)

    def OleInitialize(self):
        # Single emulated thread/apartment only. No COM object APIs are admitted.
        if self.cap.arg(0)!=0:raise ValueError('OleInitialize reserved argument must be NULL')
        if self.apartment_mode not in(None,'STA'):
            self.events.append(('OleInitialize','RPC_E_CHANGED_MODE'));return 0x80010106
        result=1 if self.ole_count else 0
        self.apartment_mode='STA';self.ole_count+=1
        self.events.append(('OleInitialize','STA',self.ole_count,result));return result
    def OleUninitialize(self):
        if self.ole_count<=0:raise ValueError('unbalanced OleUninitialize')
        self.ole_count-=1
        if not self.ole_count:self.apartment_mode=None
        self.events.append(('OleUninitialize',self.ole_count));return 0

    def QueryPerformanceCounter(self):
        # Deterministic emulated monotonic clock: ten million ticks/second,
        # one millisecond per observation. It is not wall-clock simulation.
        pointer=self.cap.arg(0)
        if not pointer:raise ValueError('QPC requires output LARGE_INTEGER pointer')
        self.clock_ticks+=10000
        self.cap.wq(pointer,self.clock_ticks)
        self.events.append(('QueryPerformanceCounter',self.clock_ticks));return 1
    def QueryPerformanceFrequency(self):
        pointer=self.cap.arg(0)
        if not pointer:raise ValueError('QPF requires output LARGE_INTEGER pointer')
        self.cap.wq(pointer,self.clock_frequency)
        self.events.append(('QueryPerformanceFrequency',self.clock_frequency));return 1

    def GetSystemTimeAsFileTime(self):
        # Same deterministic emulated clock (100ns ticks) placed after the FILETIME
        # Unix epoch; original callers (CRT time() -> srand) accept any value.
        pointer=self.cap.arg(0)
        if not pointer:raise ValueError('GetSystemTimeAsFileTime requires output FILETIME pointer')
        self.clock_ticks+=10000
        value=116444736000000000+self.clock_ticks
        self.cap.wq(pointer,value)
        self.events.append(('GetSystemTimeAsFileTime',value));return 0
    def timeGetTime(self):
        self.clock_ticks+=10000
        value=(self.clock_ticks*1000//self.clock_frequency)&0xffffffff
        self.events.append(('timeGetTime',value));return value

    def GetModuleHandleW(self):
        # The only module represented by this offline host is the pinned original
        # image. Named module lookups would require loader semantics and reject.
        if self.cap.arg(0)!=0:
            name=self._string(self.cap.arg(0),True).lower()
            if name not in ('kernel32.dll','api-ms-win-core-synch-l1-2-0.dll'):
                raise ValueError('GetModuleHandleW supports only NULL and virtual synch/kernel32 lookups')
            handle=next((h for h,n in self.modules.items()if n==name),None)
            if handle is None:
                handle=0x75000000+len(self.modules)*0x1000;self.modules[handle]=name
            self.events.append(('GetModuleHandleW',name,handle));return handle
        from offline_host import BASE
        self.events.append(('GetModuleHandleW','mapped_original_image',BASE));return BASE

    # Single emulated thread: events are owned handle state only. No thread is ever
    # created, so a wait on an unsignaled event can only time out (emulated clock
    # advances); an infinite wait would deadlock and is refused.
    def CreateEventW(self):
        attributes,manual,initial,name=[self.cap.arg(i)for i in range(4)]
        if attributes or name:raise ValueError('unnamed default-security event required')
        handle=self.next_event_handle;self.next_event_handle+=0x10
        if self.next_event_handle>0x79000000:raise ValueError('bounded event space exhausted')
        self.event_handles[handle]=dict(manual=bool(manual&0xffffffff),signaled=bool(initial&0xffffffff))
        self.events.append(('CreateEventW',hex(handle),self.event_handles[handle]['manual'],self.event_handles[handle]['signaled']));return handle
    def _event(self):
        handle=self.cap.arg(0)
        if handle not in self.event_handles:raise ValueError('owned event handle required')
        return handle,self.event_handles[handle]
    def SetEvent(self):
        handle,event=self._event();event['signaled']=True;self.events.append(('SetEvent',hex(handle)));return 1
    def ResetEvent(self):
        handle,event=self._event();event['signaled']=False;self.events.append(('ResetEvent',hex(handle)));return 1
    def CloseHandle(self):
        handle,event=self._event();del self.event_handles[handle];self.events.append(('CloseHandle',hex(handle)));return 1
    def WaitForSingleObject(self):
        handle,event=self._event();timeout=self.cap.arg(1)&0xffffffff
        if event['signaled']:
            if not event['manual']:event['signaled']=False
            self.events.append(('WaitForSingleObject',hex(handle),'signaled'));return 0
        if timeout==0xffffffff:raise ValueError('infinite wait on unsignaled event would deadlock the single emulated thread')
        self.clock_ticks+=timeout*10000
        self.events.append(('WaitForSingleObject',hex(handle),'timeout',timeout));return 258

    # Condition variables exist as exports, but blocking/waking needs a second thread. The
    # single emulated thread only reaches them on contended thread-safe static initialization.
    def SleepConditionVariableCS(self):raise ValueError('contended static initialization needs a second thread')
    def WakeAllConditionVariable(self):self.events.append(('WakeAllConditionVariable','no_waiters'));return 0  # one thread: nothing can be waiting

    def PostMessageW(self):
        # Headless host: no message pump runs, so a posted message is queued to its owned window and never
        # dispatched (async UI notification; the original sender does not wait on it).
        window,message,wparam,lparam=[self.cap.arg(i)for i in range(4)]
        if window not in self.windows:raise ValueError('owned headless window required for PostMessageW')
        self.posted_messages.append((window,message&0xffffffff,wparam,lparam))
        self.events.append(('PostMessageW',hex(window),hex(message&0xffffffff),'queued_undelivered'));return 1
