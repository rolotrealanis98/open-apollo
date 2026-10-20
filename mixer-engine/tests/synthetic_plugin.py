"""A made-up plug-in with the emulator's capture surface (words and texts invented, nothing from
a UA plug-in), for the emulator-free tables tests."""
import struct


def f32(x):
    return struct.unpack('<I', struct.pack('<f', x))[0]


class Synthetic:
    """A preamp-like plug-in: Select (Line/Mic) picks which of two continuous gains feeds the gain
    block, Phase flips its sign, Output feeds an output block that Power gates; the plug-in's copy
    of Power is off at load until Power is set once (even to the value it has)."""
    names = ('Select', 'Mic', 'Line', 'Phase', 'Output', 'Power')
    maxima = (1, 300, 300, 1, 300, 1)
    coupled = False         # True: Mic moves Line
    two_gains = False       # True: the gain block uses both gains at once

    def __init__(self, profile=None, auto_start=True):
        self.values = [1, 150, 0, 0, 200, 1]
        self.power_copy = 0
        self.count = 7
        self.events = [['begin', 0x10, 0x20, 6], ['end', 0x20, 6]]   # startup
        self.defaults_values = tuple(self.values)

    def start(self):
        pass

    def read_values(self):
        return tuple(self.values)

    def _tx(self, writes):
        self.events.append(['begin', 0x10, 0x20, self.count])
        self.events.extend(['write', ref, words] for ref, words in writes)
        self.events.append(['end', 0x20, self.count])
        self.count += 1

    def _gain(self):
        sel, mic, line, phase = self.values[:4]
        gain = (mic + line) / 30 if self.two_gains else (mic if sel else line) / 30
        sign = -1 if phase else 1
        return [f32(sign * gain), f32(sign * gain / 2), sel]

    def _out(self):
        on = self.power_copy
        return [f32(self.values[4] / 100) if on else 0, 0 if on else f32(1.0)]

    def set_control(self, index, value):
        if not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.values[index] = min(value, 290) if index == 4 else value   # Output clamps at 290
        if self.coupled and index == 1:
            self.values[2] = value
        if index in (0, 1, 2, 3):
            self._tx([(0x100, self._gain())])
        else:
            if index == 5:
                self.power_copy = value
            self._tx([(0x200, self._out()[:1]), (0x201, self._out()[1:])])
        self.events.append(['write', 0x300 + index, [self.values[index]]])   # a plain write, no bracket

    def format_parameter(self, index):
        v = self.values[index]
        text = {0: ('Line', 'Mic'), 3: ('Normal', 'Inverted'), 5: ('Off', 'On')}.get(index)
        text = text[v] if text else f'{v // 10 - 15:+d} dB '
        return dict(raw=text, console_trimmed=text.strip())
