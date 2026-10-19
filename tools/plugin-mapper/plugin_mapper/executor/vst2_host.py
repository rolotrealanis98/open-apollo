"""Offline executor for VST2 `AudioEffect` + `IUADPlug` plug-ins (UAD AMS RMX16 family and kin).

The original DLL runs under Unicorn exactly as a VST2 host would drive it:

  * `VSTPluginMain(audioMaster)` -> original constructor; the AEffect inside the object (+aeffect_offset) is
    the only thing the host keeps. open / setSampleRate / setBlockSize / mainsChanged go through
    `AEffect.dispatcher`, parameters through `AEffect.setParameter/getParameter` (float 0..1);
  * the plug-in calls `UAD_CreateIUADPlug(audioMaster, AEffect*, CUADPlugCallbackHandler*, version)`;
    the handler is a base sub-object of the effect (this + handler_offset). The IUADPlug it gets back is the
    same cache interface DspApCapture / GenericCapture already model (set_int/set_float/arrays/atomics/timing),
    so every DSP write lands in the coefficient shadow and in the event list the bundle schema replays;
  * the handler's original vtable slot 3 fills the AllocInfo descriptor, slot 15 formats Console text.

Statically linked CRT/heap code that needs an OS (operator new, sprintf) and the GUI editor constructor are
replaced by address-pinned intercepts (the profile records each function's prologue hash; mismatch refuses).
Hardware is never touched. See catalog-discovery/families-vst2-iuadplug.md.
"""
import hashlib, struct
from dspap_capture import DspApCapture, SCHEMA as DSPAP_SCHEMA, F32, F32BITS
from generic_capture import integer, STUBS, BASE
from probe_transactions import Replay
from unicorn.x86_const import UC_X86_REG_XMM0, UC_X86_REG_XMM2

SCHEMA = 'openapollo-vst2-iuadplug-profile/1'
FRAMEWORK = 'VST2IUADPlug'
INTERCEPTS = STUBS + 0xf000          # 16-byte slots above the import stubs (OfflineHost stops at +0xf000)
OPCODES = dict(open=0, set_sample_rate=10, set_block_size=11, mains_changed=12)


class Vst2Capture(DspApCapture):
    """Same host API as GenericCapture (what run_profile.py and the carrier provider call)."""

    def __init__(self, profile, auto_start=True):
        spec = profile.get('vst2')
        if profile.get('schema') != SCHEMA or profile.get('framework') != FRAMEWORK or not isinstance(spec, dict):
            raise ValueError('VST2 IUADPlug profile required')
        self.vst2 = spec
        self.next_intercept = INTERCEPTS
        derived = dict(profile, schema=DSPAP_SCHEMA, dspap=spec)  # DspApCapture host fixtures (cache interface, CRT subset)
        super().__init__(derived, auto_start=False)
        self.profile = profile
        self.phase = 'constructed'
        if auto_start:
            self.start()

    # --- host fixtures ---------------------------------------------------------------------------------
    def stub(self, name, fn):
        address = self.next_intercept
        if address >= STUBS + 0x10000:
            raise ValueError('intercept stub overflow')
        self.next_intercept += 16
        self.handlers[address] = (name, fn)
        return address

    def intercept(self, item):
        """Replace one original function by a host handler; refuses unless the original prologue is the pinned one."""
        va, size = integer(item['va']), len(bytes.fromhex(item['prologue']))
        if bytes(self.uc.mem_read(va, size)).hex() != item['prologue']:
            raise ValueError('intercepted function differs from the pinned original ' + item['va'])
        target = self.stub('intercept ' + item['kind'] + ' ' + item['va'], getattr(self, 'intercept_' + item['kind']))
        if size < 12:
            raise ValueError('prologue too short to redirect')
        self.uc.mem_write(va, b'\x48\xb8' + struct.pack('<Q', target) + b'\xff\xe0')  # mov rax, stub; jmp rax

    def intercept_new(self):
        return self.reserve(max(16, self.arg(0)))

    def intercept_gui_constructor(self):
        return self.arg(0)  # the zeroed object the original `new` produced; no bitmaps, no windows

    def cstring(self, pointer, limit=4096):
        raw = bytes(self.uc.mem_read(pointer, limit))
        return raw[:raw.index(b'\0')] if b'\0' in raw else raw

    def intercept_sprintf(self):
        """C sprintf for the subset the original formatters use (%d %u %x %c %s %f with width/precision/flags)."""
        buf, fmt, position = self.arg(0), self.cstring(self.arg(1)).decode('ascii'), 2
        out, i = [], 0
        while i < len(fmt):
            if fmt[i] != '%':
                out.append(fmt[i]); i += 1; continue
            j = i + 1
            while fmt[j] in '-+ #0123456789.':
                j += 1
            flags, long = fmt[i:j], False
            while fmt[j] in 'lhI64wz':
                long = long or fmt[j] in 'lI'; j += 1
            conv = fmt[j]
            if conv == '%':
                out.append('%')
            else:
                raw = self.arg(position); position += 1
                if conv in 'di':
                    raw &= 0xffffffffffffffff if long else 0xffffffff
                    bits = 64 if long else 32
                    out.append((flags + 'd') % (raw - (1 << bits) if raw >> (bits - 1) else raw))
                elif conv in 'uxX':
                    out.append((flags + conv) % (raw & (0xffffffffffffffff if long else 0xffffffff)))
                elif conv == 'c':
                    out.append(chr(raw & 0xff))
                elif conv == 's':
                    out.append((flags + 's') % self.cstring(raw).decode('ascii'))
                elif conv in 'fgeE':
                    out.append((flags + conv) % struct.unpack('<d', struct.pack('<Q', raw & 0xffffffffffffffff))[0])
                else:
                    raise ValueError('unmodeled sprintf conversion ' + repr(fmt[i:j + 1]))
            i = j + 1
        text = ''.join(out).encode('latin-1') + b'\0'
        self.uc.mem_write(buf, text)
        return len(text) - 1

    def audio_master(self):
        """audioMaster(effect, opcode, index, value, ptr, opt): only the version handshake is answered; the rest is 0."""
        opcode = self.arg(1) & 0xffffffff
        self.audio_master_calls.append(opcode)
        return 2400 if opcode == 1 else 0

    def create_iuadplug(self):
        """UAD2SDK!UAD_CreateIUADPlug(audioMaster, AEffect*, CUADPlugCallbackHandler*, UADInterfaceVersion)."""
        master, aeffect, handler, version = (self.arg(i) for i in range(4))
        self.created = (master, aeffect, handler, version & 0xffffffff)
        if self.created[3] != integer(self.vst2['interface_version']):
            raise ValueError('UADInterfaceVersion differs from the pinned one ' + hex(self.created[3]))
        return self.iface

    # --- the VST2 object ---------------------------------------------------------------------------------
    def build_processor(self):
        spec = self.vst2
        for slot, answer in spec['iface_answers'].items():  # IUADPlug queries the constructor and descriptor getter make
            self.bind(integer(slot), 'iface_answer', lambda answer=answer: answer)
        for item in spec['intercepts']:
            self.intercept(item)
        by_name = {(name.decode() if name else ''): stub for name, label, stub in self.imports}
        creator = next(stub for name, stub in by_name.items() if name.startswith('?UAD_CreateIUADPlug@'))
        self.handlers[creator] = ('UAD_CreateIUADPlug', self.create_iuadplug)
        self.audio_master_calls, self.created = [], None
        master = self.stub('audioMaster', self.audio_master)
        self.aeffect = self.call(integer(spec['entry_va']), master)  # VSTPluginMain
        if not self.aeffect:
            raise ValueError('VSTPluginMain returned no effect')
        self.this = self.parent = self.aeffect - integer(spec['aeffect_offset'])
        self.handler = self.this + integer(spec['handler']['offset'])
        for off, va in ((0, spec['processor_vtable']), (integer(spec['handler']['offset']), spec['handler']['vtable'])):
            if self.rq(self.this + off) != integer(va):
                raise ValueError('constructed object vtable differs from the pinned one at +' + hex(off))
        for slot, va in spec['handler']['slots'].items():
            if self.rq(integer(spec['handler']['vtable']) + integer(slot)) != integer(va):
                raise ValueError('handler vtable slot differs from the original image ' + slot)
        if self.created != (master, self.aeffect, self.handler, integer(spec['interface_version'])):
            raise ValueError('UAD_CreateIUADPlug arguments differ: ' + repr(self.created))
        if self.rq(self.this + integer(spec['iface_offset'])) != self.iface:
            raise ValueError('effect did not store the IUADPlug it was given')
        # The GUI editor object is never opened offline; the original constructor attached a zeroed one.
        self.wq(self.this + integer(spec['editor_offset']), 0)
        magic = self.rd(self.aeffect)
        if magic != 0x56737450:
            raise ValueError('AEffect magic differs')
        self.dispatcher_fn = self.rq(self.aeffect + 8)
        self.set_parameter_fn = self.rq(self.aeffect + 0x18)
        self.get_parameter_fn = self.rq(self.aeffect + 0x20)
        if self.rd(self.aeffect + 0x2c) < len(spec['parameters']):
            raise ValueError('AEffect numParams smaller than the profile parameter count')
        # Host configuration, in the order a VST2 host applies it right after creation.
        host = spec['host']
        self.dispatch(OPCODES['open'])
        self.dispatch(OPCODES['set_sample_rate'], opt=float(host['sample_rate']))
        self.dispatch(OPCODES['set_block_size'], value=int(host['block_size']))
        self.uc.mem_write(self.fs, struct.pack('<f', float(host['sample_rate'])))
        self.cells = []

    def dispatch(self, opcode, index=0, value=0, pointer=0, opt=0.0):
        return self.call(self.dispatcher_fn, self.aeffect, opcode, index, value, pointer, F32BITS(F32(opt)))

    def get_parameter(self, index):
        self.call(self.get_parameter_fn, self.aeffect, index)
        return struct.unpack('<f', struct.pack('<I', self.uc.reg_read(UC_X86_REG_XMM0) & 0xffffffff))[0]

    def set_parameter(self, index, norm):
        self.uc.reg_write(UC_X86_REG_XMM2, F32BITS(F32(norm)))
        self.call(self.set_parameter_fn, self.aeffect, index, 0)

    def grid(self, index, norm):
        """Integer on the profile grid for a plug-in float, or ValueError when it is off the grid."""
        top = self.maxima[index]
        value = round(norm * top)
        tolerance = 1e-6 if self.vst2['parameters'][index].get('exact') else 0.5 / top  # stepped: the original keeps only the step
        if abs(F32(value / top) - norm) > tolerance:
            raise ValueError('plug-in parameter %d value %r is off the profile grid 1/%d' % (index, norm, top))
        return value

    def apply_raw_values(self):
        """Profile defaults (captured Console state) must equal the original constructor's state."""
        actual = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if actual != tuple(self.values):
            raise ValueError('captured defaults differ from the original constructor ' + repr((actual, tuple(self.values))))

    # --- host API used by run_profile.py / the carrier provider -------------------------------------------
    def start(self):
        self.phase = 'original_startup'
        self.dispatch(OPCODES['mains_changed'], value=1)  # resume(): sample rate, timing header, full parameter push
        self.defaults_values = tuple(self.values)
        self.defaults = dict(self.shadow)
        self.startup_events = list(self.events)
        self.startup_formatted = [self.format_parameter(i) for i in range(len(self.maxima))]
        self._verify_replay()
        self.captured_properties = None
        shown = self.vst2.get('captured_text')
        if shown is not None:
            for i, text in enumerate(self.startup_formatted):
                if text['console_trimmed'] != shown[i]:
                    raise ValueError('captured original formatter mismatch ' + repr((i, text, shown[i])))
        self.phase = 'ready'

    def set_control(self, index, value):
        if type(index) is not int or not 0 <= index < len(self.maxima) or type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        self.set_parameter(index, value / self.maxima[index])
        self.values = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))  # coupled parameters included
        if self.values[index] != value:
            raise ValueError('original setter did not keep the requested value')
        self._verify_replay()
        return value, dict(self.shadow)


def extract_descriptor(cap, profile, decode_descriptor):
    """Original handler slot 3 (GetAllocInfo) on the constructed effect; decoded by the shared decoder."""
    from pathlib import Path
    spec = profile['vst2']['descriptor']
    fn = integer(spec['getter_va'])
    if cap.rq(cap.rq(cap.handler) + 0x18) != fn:
        raise ValueError('descriptor getter is not the original handler slot 3')
    before = dict(cap.shadow)
    out = cap.reserve(0x1000)
    cap.call(fn, cap.handler, out)
    if any(cap.shadow.get(k) != v for k, v in before.items()) or cap.shadow.keys() - before.keys():
        raise ValueError('descriptor call touched DSP memory')
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
    data['receiver_evidence'] = dict(status='original_handler_slot3_descriptor', framework=FRAMEWORK,
                                     handler_offset=profile['vst2']['handler']['offset'], vtable=profile['vst2']['handler']['vtable'],
                                     getter_va=spec['getter_va'])
    return data
