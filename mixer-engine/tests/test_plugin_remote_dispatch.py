"""Actual daemon GET frames backed by coordinator-owned readback; no sockets/DSP."""

import copy
import unittest
from threading import Thread
from plugin_remote import RemoteReadback
from protocol import Command
import test_plugin_transactions as transactions
import test_plugin_lifecycle as lifecycle
from ua_mixer_daemon import MixerDaemon, MixerClient
from plugin_remote_codec import decode_scalar_response, decode_array_response


class RemoteDispatch(unittest.TestCase):
    def model(self, binary=False, helper=False):
        f, s, h, p, b, c = transactions.Transactions().model()
        self.assertTrue(c.load(f.slot, "fixture").committed)

        class ReadBackend:
            hook = None

            def read(self, slot, handle, query):
                if self.hook:
                    self.hook()
                return RemoteReadback(handle, query, (0x80000000, 0x7FC00001))

        read = ReadBackend()
        daemon = MixerDaemon(
            s,
            None,
            helper_tree=h,
            plugin_transactions=c,
            plugin_remote_backend=read,
        )
        writer = lifecycle.Writer()
        client = MixerClient(None, writer, ubjson=binary)
        client.ubjson = binary
        client.helper = helper
        return f, s, h, p, b, c, read, daemon, client, writer

    def test_owned_scalar_and_array_frames_all_transports(self):
        for binary in (False, True):
            for helper in (False, True):
                for params in (
                    {"floats": "1,2"},
                    {"ints": "1,2"},
                    {"opcode": "519", "p0": "12", "p1": "2", "p2": "1"},
                    {"opcode": "518", "p0": "12", "p1": "2", "p2": "1"},
                ):
                    with self.subTest(
                        binary=binary, helper=helper, params=params
                    ):
                        f, s, h, p, b, c, r, d, client, w = self.model(
                            binary, helper
                        )
                        path = f.slot + "/VendorSpecific/value"
                        before = copy.deepcopy((s.root, h.root))
                        d._handle_get(
                            client,
                            Command(
                                "get",
                                path,
                                raw_params=dict(params, func_id="77"),
                            ),
                        )
                        msg = lifecycle.PluginTests().decode(w, binary)
                        self.assertNotIn("error", msg)
                        self.assertEqual(msg["parameters"]["func_id"], 77)
                        decode = (
                            decode_array_response
                            if "opcode" in params
                            else decode_scalar_response
                        )
                        decoded = decode(
                            path,
                            msg["data"],
                            msg["parameters"],
                            expected_path=path,
                        )
                        self.assertEqual(
                            decoded.words, (0x80000000, 0x7FC00001)
                        )
                        self.assertEqual(before, (s.root, h.root))

    def test_disconnect_during_read_suppresses_data(self):
        f, s, h, p, b, c, r, d, client, w = self.model()
        r.hook = p.disconnect
        d._handle_get(
            client,
            Command(
                "get",
                f.slot + "/VendorSpecific/value",
                raw_params={"ints": "1,2"},
            ),
        )
        msg = lifecycle.PluginTests().decode(w, False)
        self.assertIn("error", msg)
        self.assertNotIn("data", msg)

    def test_publication_revision_change_suppresses_same_handle_reply(self):
        f, s, h, p, b, c, r, d, client, w = self.model()
        r.hook = lambda: p.stage(f.slot, f.result(43)).commit()
        d._handle_get(
            client,
            Command(
                "get",
                f.slot + "/VendorSpecific/value",
                raw_params={"ints": "1,2"},
            ),
        )
        msg = lifecycle.PluginTests().decode(w, False)
        self.assertIn("error", msg)
        self.assertNotIn("data", msg)

    def test_captured_publication_does_not_establish_owned_lease(self):
        f, s, h, p, b, c = transactions.Transactions().model(old=True)
        self.assertIsNone(c.current_remote_handle(f.slot))
        self.assertTrue(c.load(f.slot, "fixture").committed)
        self.assertIsNotNone(c.current_remote_handle(f.slot))
        replies = []
        t = Thread(
            target=lambda: replies.append(c.current_remote_handle(f.slot))
        )
        t.start()
        t.join()
        self.assertEqual(replies, [None])
        c.quarantined = True
        self.assertIsNone(c.current_remote_handle(f.slot))


if __name__ == "__main__":
    unittest.main()
