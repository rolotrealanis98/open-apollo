"""Offline executor for the UAD `UAD2AGain` / `ParameterHandler` / `IUADPlugRelab` template (sub-family C: UAD Lexicon 480L).

Object layout is sub-family A's (VSTPluginMain news an `AudioEffectX` whose AEffect sits at +0x30 and whose CUADPlugCallbackHandler is the base
sub-object at +0xf0), the runtime is sub-family B's (older static CRT started through the original DllMain, TLS, pinned allocator/editor
intercepts) and the parameter -> DSP function is the plug-in's own `ParameterHandler::setParameter` reached through `AEffect.setParameter`.
Reuses Vst2BxCapture for the shared runtime; see catalog-discovery/families-vst2-iuadplug.md section 7.
"""
from vst2_bx_host import Vst2BxCapture, OPCODES
from generic_capture import integer
import struct


class Vst2RelabCapture(Vst2BxCapture):
    def build_processor(self):
        spec = self.vst2
        self.prepare_runtime()
        master = self.stub('audioMaster', self.audio_master)
        self.aeffect = self.call(integer(spec['entry_va']), master)
        if not self.aeffect or self.rd(self.aeffect) != 0x56737450:
            raise ValueError('VSTPluginMain returned no AEffect')
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
        self.cells = []
        self.inactive_requests = set()

    def start(self):
        """effMainsChanged(1) is the SDK's Resume notification (IUADPlug +0x160, no DSP content); the plug-in's own DSP image is pushed
        by CUADPlugCallbackHandler slot 17 (the load-time call the SDK makes after allocation), exactly as in the DSPAudioProcessor family."""
        self.phase = 'original_startup'
        self.dispatch(OPCODES['mains_changed'], value=1)
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

    def inactive(self, index):
        """The original's own predicate (the one ParameterHandler::setParameter consults before it refuses with 'Inactive Parameter')."""
        spec = self.vst2['inactive']
        return bool(self.call(integer(spec['va']), self.rq(self.this + integer(spec['receiver_offset'])), index) & 0xff)

    def set_control(self, index, value):
        if not self.inactive(index):
            return super().set_control(index, value)
        if type(value) is not int or not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.phase = 'control_' + str(index)
        before = self.values
        self.set_parameter(index, value / self.maxima[index])   # the original refuses the value; it only runs its atomic sequence bracket
        self.values = tuple(self.grid(i, self.get_parameter(i)) for i in range(len(self.maxima)))
        if self.values != before:
            raise ValueError('original changed parameter state on a parameter it reports inactive')
        self._verify_replay()
        self.inactive_requests.add(index)
        return self.values[index], dict(self.shadow)


def extract_descriptor(cap, profile, decode_descriptor):
    """Original handler slot 3 on the constructed effect; same decoder and checks as sub-family A."""
    import vst2_host
    data = vst2_host.extract_descriptor(cap, profile, decode_descriptor)
    data['receiver_evidence']['family'] = 'UAD2AGain / ParameterHandler / IUADPlugRelab template'
    return data
