"""Actual wire publication with injected ownership attestations, no DSP emulation."""

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
from protocol import Command
from plugin_lifecycle import PluginLifecycle, PluginResult, is_plugin_slot_path
from plugin_transactions import Outcome
from plugin_publication import OwnedHandle
import test_plugin_transactions as transaction_fixtures
from plugin_test_support import MemoryWriter
from ua_mixer_daemon import MixerDaemon, MixerClient
from ubjson_codec import decode_frame


class PluginTransactionProtocol(unittest.TestCase):
    def model(self, binary=False):
        f, s, h, p, backend, coordinator = (
            transaction_fixtures.Transactions().model()
        )
        s.add_runtime_property("/Dirty", {"type": "bool", "value": False})
        h.root.setdefault("properties", {})["Dirty"] = {
            "type": "bool",
            "value": False,
        }
        daemon = MixerDaemon(
            s, None, helper_tree=h, plugin_transactions=coordinator
        )
        daemon._session_ready = True
        writer = MemoryWriter()
        client = MixerClient(None, writer, ubjson=binary)
        client.ubjson = binary
        client.helper = binary
        s.on_set = Mock()
        s._schedule_save = Mock()
        seen = []

        def observe(path, value):
            # Every event observes the complete two-view identity/parameter tree.
            self.assertEqual(s.get_value(f.slot + "/EffectInstance"), 43)
            self.assertEqual(h.get_value(f.slot + "/EffectInstance"), 43)
            self.assertEqual(
                s.get_value(f.slot + "/parameters/0/NormalizedValue"), 0.5
            )
            seen.append((path, value))

        s.register_callback("observer", observe)
        for field in (
            "EffectName",
            "Power",
            "Latency",
            "Status",
            "EffectInstance",
        ):
            s.subscribe("observer", f.slot + "/" + field + "/value")
        return f, s, h, backend, coordinator, daemon, writer, client, seen

    def messages(self, writer, binary):
        return [
            decode_frame(frame)[0]
            if binary
            else json.loads(frame.rstrip(b"\0"))
            for frame in writer.frames
        ]

    def test_confirmed_commit_publishes_both_views_and_dirty(self):
        for binary in (False, True):
            f, s, h, b, c, d, w, client, seen = self.model(binary)
            d._handle_set(
                client,
                Command(
                    "set",
                    f.slot + "/EffectName/value",
                    "fixture",
                    raw_params={"func_id": "7"},
                ),
            )
            self.assertEqual(b.calls, ["prepare", "activate", "finalize"])
            self.assertEqual(seen[-1], (f.slot + "/EffectInstance/value", 43))
            self.assertTrue(s.get_value("/Dirty"))
            self.assertTrue(h.get_value("/Dirty"))
            self.assertFalse(s.get_value("/plugins/0/Authorized"))
            s.on_set.assert_not_called()
            self.assertEqual(self.messages(w, binary)[-1]["data"], "fixture")

    def test_failed_activation_publishes_nothing(self):
        f, s, h, b, c, d, w, client, seen = self.model()
        b.activate_status = Outcome.FAILED
        before = copy.deepcopy((s.root, h.root, s._dirty))
        d._handle_set(
            client, Command("set", f.slot + "/EffectName", "fixture")
        )
        self.assertEqual((s.root, h.root, s._dirty), before)
        self.assertEqual(seen, [])
        s._schedule_save.assert_not_called()
        self.assertIn("error", self.messages(w, False)[-1])

    def test_failed_helper_subscriber_does_not_interrupt_committed_publication(
        self,
    ):
        f, s, h, b, c, d, w, client, seen = self.model()
        delivered = []

        def fail(path, value):
            raise RuntimeError("subscriber disconnected")

        h.register_callback("broken", fail)
        h.register_callback(
            "healthy", lambda path, value: delivered.append((path, value))
        )
        h.subscribe("broken", f.slot)
        h.subscribe("healthy", f.slot)
        with self.assertLogs("helper_tree", level="ERROR"):
            d._handle_set(
                client,
                Command(
                    "set",
                    f.slot + "/EffectName",
                    "fixture",
                    raw_params={"func_id": "7"},
                ),
            )
        self.assertEqual(delivered[-1], (f.slot + "/EffectInstance/value", 43))
        self.assertTrue(s.get_value("/Dirty"))
        self.assertEqual(self.messages(w, False)[-1]["data"], "fixture")

    def test_retirement_unknown_still_publishes_committed_state_and_blocks_more_edits(
        self,
    ):
        f, s, h, b, c, d, w, client, seen = self.model()
        b.finalize_status = Outcome.INDETERMINATE
        d._handle_set(
            client, Command("set", f.slot + "/EffectName", "fixture")
        )
        self.assertTrue(c.quarantined)
        self.assertEqual(seen[-1], (f.slot + "/EffectInstance/value", 43))
        self.assertTrue(s.get_value("/Dirty"))
        self.assertEqual(
            self.messages(w, False)[-1]["error"],
            "openapollo.plugin_transaction_indeterminate",
        )
        calls = list(b.calls)
        d._handle_set(client, Command("set", f.slot + "/EffectName", ""))
        self.assertEqual(b.calls, calls)
        self.assertEqual(s.get_value(f.slot + "/EffectInstance"), 43)

    def test_reentrant_plugin_edits_rejected_during_both_view_notifications(
        self,
    ):
        for binary in (False, True):
            for via_helper in (False, True):
                f, s, h, b, c, d, w, client, seen = self.model(binary)
                nested = []

                def reenter(path, value):
                    if path != f.slot + "/EffectName/value":
                        return
                    # A genuine second injected replacement could otherwise
                    # commit before the outer EffectInstance event is sent.
                    b.candidate = f.result(44)
                    b.previous = OwnedHandle(43, "test", 0)
                    d._handle_set(
                        client,
                        Command(
                            "set",
                            f.slot + "/EffectName",
                            "fixture",
                            raw_params={"func_id": "99"},
                        ),
                    )
                    nested.append(True)

                if via_helper:
                    h.register_callback("reenter", reenter)
                    h.subscribe("reenter", f.slot + "/EffectName/value")
                else:
                    s.register_callback("reenter", reenter)
                    s.subscribe("reenter", f.slot + "/EffectName/value")
                d._handle_set(
                    client,
                    Command(
                        "set",
                        f.slot + "/EffectName",
                        "fixture",
                        raw_params={"func_id": "7"},
                    ),
                )
                self.assertEqual(nested, [True])
                self.assertEqual(b.calls, ["prepare", "activate", "finalize"])
                self.assertEqual(s.get_value(f.slot + "/EffectInstance"), 43)
                self.assertEqual(h.get_value(f.slot + "/EffectInstance"), 43)
                errors = [
                    message
                    for message in self.messages(w, binary)
                    if "error" in message
                ]
                self.assertEqual(
                    errors[0]["error"],
                    "openapollo.plugin_transaction_reentrant",
                )
                self.assertEqual(str(errors[0]["parameters"]["func_id"]), "99")
                self.assertEqual(
                    seen[-1], (f.slot + "/EffectInstance/value", 43)
                )
                self.assertFalse(d._plugin_publication_active)

    def test_publication_exception_releases_guard_without_fake_backend_rollback(
        self,
    ):
        f, s, h, b, c, d, w, client, seen = self.model()
        d._publish_plugin_commit_inner = Mock(
            side_effect=RuntimeError("injected dispatcher failure")
        )
        with self.assertRaises(RuntimeError):
            d._handle_set(
                client, Command("set", f.slot + "/EffectName", "fixture")
            )
        self.assertFalse(d._plugin_publication_active)
        self.assertEqual(b.calls, ["prepare", "activate", "finalize"])
        self.assertEqual(s.get_value(f.slot + "/EffectInstance"), 43)
        self.assertEqual(h.get_value(f.slot + "/EffectInstance"), 43)
        self.assertNotIn("rollback", b.calls)
        # A later ordinary plugin request reaches lifecycle rather than the guard.
        d._handle_set(client, Command("set", f.slot + "/Power/value", False))
        self.assertEqual(
            self.messages(w, False)[-1]["error"],
            "openapollo.plugin_backend_unavailable",
        )

    def test_saved_intent_drops_stale_parameters_and_runtime_handles(self):
        f, s, h, b, c, d, w, client, seen = self.model()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.json"
            path.write_text(
                json.dumps(
                    {
                        f.slot + "/EffectInstance/value": 999,
                        f.slot + "/parameters/91/NormalizedValue/value": 0.9,
                    }
                )
            )
            s.enable_persistence(
                path, restore_filter=lambda key: not is_plugin_slot_path(key)
            )
            d._handle_set(
                client, Command("set", f.slot + "/EffectName", "fixture")
            )
            s.save_now()
            saved = json.loads(path.read_text())
            self.assertNotIn(f.slot + "/EffectInstance/value", saved)
            self.assertNotIn(
                f.slot + "/parameters/91/NormalizedValue/value", saved
            )
            self.assertEqual(saved[f.slot + "/EffectName/value"], "fixture")
            self.assertEqual(
                saved[f.slot + "/parameters/0/NormalizedValue/value"], 0.5
            )

    def test_diagnostic_adapter_cannot_forge_committed_events(self):
        f, s, h, b, c, d, w, client, seen = self.model()

        class Diagnostic:
            diagnostic_only = True

            def load(self, *args):
                return PluginResult(
                    "local.test",
                    committed=True,
                    events=((f.slot + "/EffectInstance/value", 999),),
                )

        result = PluginLifecycle(s, Diagnostic(), helper_tree=h).handle(
            f.slot + "/EffectName", "fixture"
        )
        self.assertFalse(result.committed)
        self.assertEqual(result.events, ())


if __name__ == "__main__":
    unittest.main()
