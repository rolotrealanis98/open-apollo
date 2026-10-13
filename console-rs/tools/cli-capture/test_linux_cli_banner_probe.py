import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('banner', Path(__file__).with_name('linux_cli_banner_probe.py'))
banner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(banner)


class MockRegisters:
    def __init__(self, response=b'', fail=None):
        self.words = dict.fromkeys(banner.CONTROL, 0)
        self.response, self.fail = response, fail
        self.writes = []

    def read(self, offset):
        if offset >= banner.RESPONSE_BUFFER:
            start = offset - banner.RESPONSE_BUFFER
            return struct.unpack('<I', self.response[start:start + 4].ljust(4, b'\0'))[0]
        return self.words[offset]

    def write(self, offset, value):
        self.writes.append((offset, value))
        self.words[offset] = value
        if self.fail == (offset, value):
            self.fail = None
            raise OSError('injected uncertainty after write')
        if offset == banner.RESPONSE_LENGTH and self.words[banner.ENABLE] == 1:
            self.words[offset] = len(self.response)


class Clock:
    def __init__(self): self.value = 0
    def now(self): return self.value
    def sleep(self, duration): self.value += duration


class ProbeTests(unittest.TestCase):
    def run_probe(self, registers):
        clock = Clock()
        return banner.probe(registers, now=clock.now, sleep=clock.sleep)

    def test_raw_register_allowlist_rejects_commands_and_unaligned_reads(self):
        registers = object.__new__(banner.Registers)
        for offset in (0xC400, banner.RESPONSE_BUFFER + 1, banner.RESPONSE_BUFFER + 300):
            with self.assertRaises(ValueError): registers.read(offset)
        for offset, value in ((0xC400, 1), (banner.COMMAND_LENGTH, 1),
                              (banner.ENABLE, 2), (banner.RESPONSE_LENGTH, 1)):
            with self.assertRaises(ValueError): registers.write(offset, value)

    def test_idle_enable_poll_restore_without_command_publication(self):
        mock = MockRegisters()
        result = self.run_probe(mock)
        self.assertTrue(result['restored'])
        self.assertEqual(result['response_length'], 0)
        self.assertEqual(mock.writes, [(banner.ENABLE, 1), (banner.COMMAND_LENGTH, 0),
            (banner.RESPONSE_LENGTH, 0), (banner.ENABLE, 0),
            (banner.COMMAND_LENGTH, 0), (banner.RESPONSE_LENGTH, 0)])

    def test_unsolicited_bytes_captured_without_command_write(self):
        result = self.run_probe(MockRegisters(b'fixture\r\n'))
        self.assertEqual(bytes.fromhex(result['response_hex']), b'fixture\r\n')
        self.assertTrue(result['restored'])
        self.assertFalse(result['sent_command'])

    def test_uncertain_write_still_restores_disabled_state(self):
        mock = MockRegisters(fail=(banner.ENABLE, 1))
        result = self.run_probe(mock)
        self.assertIn('error', result)
        self.assertTrue(result['restored'])

    def test_nonidle_mailbox_refused_before_any_write(self):
        for offset in banner.CONTROL:
            mock = MockRegisters()
            mock.words[offset] = 1
            with self.assertRaises(RuntimeError): self.run_probe(mock)
            self.assertEqual(mock.writes, [])

    def test_oversized_response_refused_and_restored(self):
        result = self.run_probe(MockRegisters(b'x' * 300))
        self.assertIn('oversized', result['error'])
        self.assertTrue(result['restored'])

    def test_inaccessible_preflight_does_not_write(self):
        class Gone(MockRegisters):
            def read(self, offset): raise OSError('device inaccessible')
        mock = Gone()
        with self.assertRaises(OSError): self.run_probe(mock)
        self.assertEqual(mock.writes, [])

    def test_lost_readback_never_claims_restored(self):
        class GoneAfterCleanup(MockRegisters):
            def read(self, offset):
                if len(self.writes) >= 6: raise OSError('device inaccessible')
                return super().read(offset)
        result = self.run_probe(GoneAfterCleanup())
        self.assertFalse(result['restored'])
        self.assertIn('restore_read_error', result)


if __name__ == '__main__': unittest.main()
