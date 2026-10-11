"""Socket-free writer for plugin protocol regressions."""


class MemoryWriter:
    def __init__(self):
        self.frames = []

    def get_extra_info(self, name):
        return ("127.0.0.1", 4720)

    def write(self, data):
        self.frames.append(data)
