import unittest
from dataclasses import replace
from plugin_remote import PluginRemote, RemoteReadback
from plugin_publication import OwnedHandle
from plugin_remote_codec import decode_scalar_response, decode_array_response

SLOT = "/devices/0/inputs/0/effects/0"
PATH = SLOT + "/VendorSpecific/value"


class State:
    value = 12

    def path_exists(self, p):
        return p == SLOT

    def get_value(self, p):
        assert p == SLOT + "/EffectInstance"
        return self.value


class Provider:
    remote_revision = 0
    handle = OwnedHandle(12, "native", 3)

    def current_remote_handle(self, slot):
        return self.handle


class Backend:
    def __init__(self):
        self.calls = 0
        self.callback = None

    def read(self, slot, handle, query):
        self.calls += 1
        return (
            self.callback(handle, query)
            if self.callback
            else RemoteReadback(handle, query, (0x80000000, 0x7FC01234))
        )


class RemoteBackendTests(unittest.TestCase):
    def setUp(self):
        self.state, self.provider, self.backend = (
            State(),
            Provider(),
            Backend(),
        )
        self.remote = PluginRemote(self.state, self.backend, self.provider)

    def failed(self, result):
        self.assertEqual(
            result.error, "openapollo.plugin_remote_readback_unavailable"
        )
        self.assertFalse(hasattr(result, "data"))

    def test_scalar_bits_and_metadata(self):
        params = {"floats": "1,-2", "message_id": "77"}
        r = self.remote.handle(PATH, params)
        self.assertIsNone(r.error)
        self.assertEqual(r.parameters, params)
        self.assertIsNot(r.parameters, params)
        self.assertEqual(
            decode_scalar_response(
                PATH, r.data, r.parameters, expected_path=PATH
            ).words,
            (0x80000000, 0x7FC01234),
        )
        self.assertEqual(
            self.remote.handle(PATH, {"ints": "1,2"}).data,
            "-2147483648,2143294004",
        )

    def test_array_headers_and_bits(self):
        for op in (518, 519):
            r = self.remote.handle(
                PATH,
                dict(opcode=str(op), p0="4", p1="2", p2="9", message_id="8"),
            )
            self.assertIsNone(r.error)
            self.assertEqual(
                r.parameters, dict(opcode=op, p0=4, p1=2, p2=9, message_id="8")
            )
            self.assertEqual(
                decode_array_response(
                    PATH, r.data, r.parameters, expected_path=PATH
                ).words,
                (0x80000000, 0x7FC01234),
            )

    def test_reused_handle_with_new_generation(self):
        def change(h, q):
            self.provider.handle = OwnedHandle(12, "native", 4)
            return RemoteReadback(h, q, (1, 2))

        self.backend.callback = change
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))

    def test_same_handle_republication_aba_rejected(self):
        def republish(h, q):
            self.provider.remote_revision += 1
            return RemoteReadback(h, q, (1, 2))

        self.backend.callback = republish
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))

    def test_invalid_revision_rejected_before_read(self):
        for revision in (True, -1, None):
            self.provider.remote_revision = revision
            self.failed(self.remote.handle(PATH, {"ints": "1,2"}))
        self.assertEqual(self.backend.calls, 0)

    def test_state_identity_and_provider_required(self):
        self.state.value = 99
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))
        self.failed(
            PluginRemote(self.state, self.backend).handle(
                PATH, {"ints": "1,2"}
            )
        )
        self.assertEqual(self.backend.calls, 0)

    def test_disconnected_or_wrong_reply_owner(self):
        self.provider.handle = None
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))
        self.provider.handle = OwnedHandle(12, "native", 3)
        self.backend.callback = lambda h, q: RemoteReadback(
            OwnedHandle(12, "other", 3), q, (1, 2)
        )
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))

    def test_bad_vectors_wrong_query_exception(self):
        callbacks = [
            lambda h, q: RemoteReadback(h, q, (1,)),
            lambda h, q: RemoteReadback(h, q, (True, 2)),
            lambda h, q: RemoteReadback(h, q, (1, 0x100000000)),
            lambda h, q: RemoteReadback(h, replace(q, ids=(3, 4)), (1, 2)),
            lambda h, q: (_ for _ in ()).throw(RuntimeError("offline")),
        ]
        for cb in callbacks:
            self.backend.callback = cb
            self.failed(self.remote.handle(PATH, {"ints": "1,2"}))

    def test_boolean_identity_and_query_fields_rejected(self):
        self.provider.handle = OwnedHandle(12, "native", 0)
        self.backend.callback = lambda h, q: RemoteReadback(
            OwnedHandle(12, "native", False), q, (1, 2)
        )
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))
        self.backend.callback = lambda h, q: RemoteReadback(
            h, replace(q, ids=(True, 2)), (1, 2)
        )
        self.failed(self.remote.handle(PATH, {"ints": "1,2"}))
        self.backend.callback = lambda h, q: RemoteReadback(
            h, replace(q, header_word=True), (1, 2)
        )
        self.failed(
            self.remote.handle(
                PATH, dict(opcode="519", p0="4", p1="2", p2="1")
            )
        )

    def test_unsupported_never_reads(self):
        for params in (
            {"ints": "1,1"},
            {"ints": "4294967295"},
            dict(opcode="516", p0="4", p1="2", p2="9"),
        ):
            self.failed(self.remote.handle(PATH, params))
        self.assertEqual(self.backend.calls, 0)

    def test_no_backend_preserved(self):
        self.assertEqual(
            PluginRemote(self.state).handle(PATH, {"ints": "1"}).error,
            "openapollo.plugin_remote_backend_unavailable",
        )


if __name__ == "__main__":
    unittest.main()
