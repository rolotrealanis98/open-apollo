"""A deterministic stand-in for GenericCapture (no unicorn) for emulator_worker tests."""


class FakeCapture:
    def __init__(self, profile, auto_start=True):
        self.maxima = tuple(profile['maxima'])
        self.events = [['startup', 0]]
        self.values = list(profile.get('defaults', [0] * len(self.maxima)))
        if profile.get('fail_start'):
            raise ValueError('synthetic constructor failure')

    def start(self):
        self.defaults_values = tuple(self.values)
        self.events.append(['started', len(self.values)])

    def read_values(self):
        return tuple(self.values)

    def set_control(self, index, value):
        if not 0 <= value <= self.maxima[index]:
            raise ValueError('integer original domain required')
        self.values[index] = value
        self.events.append(['write', index, value * 3])
        return value, {}

    def format_parameter(self, index):
        return dict(raw=f' {self.values[index]} ', console_trimmed=str(self.values[index]))


class EnvironmentCapture(FakeCapture):
    """Reads its setting at construction, as the emulator reads OA_UAD_SDK at import."""

    def format_parameter(self, index):
        import os
        return dict(raw='', console_trimmed=os.environ.get('OA_TEST_SETTING', 'unset'))


class CyclicCapture(FakeCapture):
    """Refers to itself, as GenericCapture does through unicorn's hooks; records when freed."""

    def __init__(self, profile, auto_start=True):
        super().__init__(profile, auto_start)
        self.cycle = self

    def __del__(self):
        import os
        with open(os.environ['OA_TEST_FREED'], 'a') as freed:
            freed.write('freed\n')
