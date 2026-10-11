"""Injected ownership attestations exercise coordination; no DSP is emulated."""

import copy
import unittest
from threading import Thread
import test_plugin_publication as fixtures
from plugin_transactions import (
    PluginTransactions,
    PreparedPlugin,
    PrepareOutcome,
    ActivateOutcome,
    RollbackOutcome,
    FinalizeOutcome,
    Outcome,
)
from plugin_publication import OwnedHandle


class Backend:
    def __init__(self, publisher, candidate, previous=None):
        self.p = publisher
        self.candidate = candidate
        self.previous = previous
        self.calls = []
        self.prepare_status = Outcome.CONFIRMED
        self.activate_status = Outcome.CONFIRMED
        self.rollback_status = Outcome.CONFIRMED
        self.finalize_status = Outcome.CONFIRMED
        self.activate_hook = None

    def prepare(self, slot, name):
        self.calls.append("prepare")
        self.slot = slot
        self.before = copy.deepcopy((self.p.state.root, self.p.helper.root))
        prepared = PreparedPlugin(object(), self.candidate, self.previous)
        return PrepareOutcome(self.prepare_status, prepared)

    def activate(self, prepared):
        self.calls.append("activate")
        assert self.before == (self.p.state.root, self.p.helper.root)
        if self.activate_hook:
            self.activate_hook()
        return ActivateOutcome(self.activate_status, True)

    def rollback(self, prepared):
        self.calls.append("rollback")
        return RollbackOutcome(self.rollback_status, True, True)

    def finalize(self, prepared):
        self.calls.append("finalize")
        assert (
            self.p.state.get_value(self.slot + "/EffectInstance/value")
            == prepared.loaded.handle.value
        )
        return FinalizeOutcome(self.finalize_status)


class Transactions(unittest.TestCase):
    def model(self, old=False):
        f = fixtures.PublicationTests()
        s, h, p = f.model()
        if old:
            p.stage(f.slot, f.result()).commit()
        previous = OwnedHandle(42, "test", 0) if old else None
        b = Backend(p, f.result(43), previous)
        return f, s, h, p, b, PluginTransactions(p, b)

    def test_confirmed_load_and_replacement(self):
        for old in (False, True):
            f, s, h, p, b, c = self.model(old)
            result = c.load(f.slot, "fixture")
            self.assertIsNone(result.error)
            self.assertTrue(result.committed)
            self.assertEqual(b.calls, ["prepare", "activate", "finalize"])
            self.assertEqual(s.get_value(f.slot + "/EffectInstance/value"), 43)
            self.assertEqual(
                h.get_value(f.slot + "/parameters/0/NormalizedValue"), 0.5
            )
            self.assertFalse(s.get_value("/plugins/0/Authorized/value"))
            self.assertEqual(
                result.events[-1], (f.slot + "/EffectInstance/value", 43)
            )

    def test_validation_and_activation_failures_retire_candidate(self):
        for fault in ("schema", "ownership", "activation"):
            f, s, h, p, b, c = self.model(True)
            before = copy.deepcopy((s.root, h.root))
            if fault == "schema":
                b.candidate = f.result(value=float("nan"))
            if fault == "ownership":
                b.previous = None
            if fault == "activation":
                b.activate_status = Outcome.FAILED
            result = c.load(f.slot, "fixture")
            self.assertFalse(result.committed)
            self.assertFalse(result.quarantined)
            self.assertEqual(b.calls[-1], "rollback")
            self.assertNotIn("finalize", b.calls)
            self.assertEqual(before, (s.root, h.root))

    def test_wrong_thread_and_reentrant_load_do_not_call_backend(self):
        f, s, h, p, b, c = self.model()
        replies = []
        thread = Thread(
            target=lambda: replies.append(c.load(f.slot, "fixture"))
        )
        thread.start()
        thread.join()
        self.assertEqual(
            replies[0].error, "openapollo.plugin_publication_owner_mismatch"
        )
        self.assertEqual(b.calls, [])
        nested = []
        b.activate_hook = lambda: nested.append(c.load(f.slot, "fixture"))
        self.assertTrue(c.load(f.slot, "fixture").committed)
        self.assertEqual(
            nested[0].error, "openapollo.plugin_transaction_reentrant"
        )
        self.assertEqual(b.calls, ["prepare", "activate", "finalize"])

    def test_false_helper_empty_handle_rejected(self):
        f, s, h, p, b, c = self.model()
        p.stage(f.slot).commit()
        node = h.root
        for part in f.slot.strip("/").split("/"):
            node = next(
                child for child in node["children"] if child["path"] == part
            )
        node["properties"]["EffectInstance"]["value"] = False
        result = c.load(f.slot, "fixture")
        self.assertFalse(result.committed)
        self.assertEqual(b.calls, ["prepare", "rollback"])

    def test_parameter_mutation_during_activation_rejected(self):
        f, s, h, p, b, c = self.model(True)
        before = copy.deepcopy((s.root, h.root))
        b.activate_hook = lambda: b.candidate.parameters[0]["properties"][
            "NormalizedValue"
        ].update(value=0.9)
        result = c.load(f.slot, "fixture")
        self.assertFalse(result.committed)
        self.assertEqual(b.calls[-1], "rollback")
        self.assertEqual(before, (s.root, h.root))

    def test_publication_failure_rolls_back_before_release(self):
        f, s, h, p, b, c = self.model(True)
        before = copy.deepcopy((s.root, h.root))

        def fault(phase):
            if phase == "both_swapped":
                raise RuntimeError("injected")

        c.publication_fault = fault
        result = c.load(f.slot, "fixture")
        self.assertFalse(result.committed)
        self.assertEqual(before, (s.root, h.root))
        self.assertEqual(b.calls, ["prepare", "activate", "rollback"])

    def test_unknown_rollback_retains_token_and_blocks_retry(self):
        f, s, h, p, b, c = self.model(True)
        b.activate_status = Outcome.INDETERMINATE
        b.rollback_status = Outcome.INDETERMINATE
        result = c.load(f.slot, "fixture")
        self.assertTrue(result.quarantined)
        self.assertFalse(result.committed)
        self.assertEqual(len(c.unresolved), 1)
        calls = b.calls[:]
        self.assertTrue(c.load(f.slot, "fixture").quarantined)
        self.assertEqual(b.calls, calls)

    def test_retirement_failure_keeps_published_candidate_and_events(self):
        f, s, h, p, b, c = self.model(True)
        b.finalize_status = Outcome.INDETERMINATE
        result = c.load(f.slot, "fixture")
        self.assertTrue(result.committed)
        self.assertTrue(result.quarantined)
        self.assertTrue(result.events)
        self.assertEqual(len(c.unresolved), 1)
        self.assertEqual(s.get_value(f.slot + "/EffectInstance/value"), 43)
        self.assertEqual(h.get_value(f.slot + "/EffectInstance"), 43)
        self.assertNotIn("rollback", b.calls)

    def test_disconnect_and_intervening_mixer_edit(self):
        for disconnect in (False, True):
            f, s, h, p, b, c = self.model(True)
            b.activate_hook = (
                p.disconnect
                if disconnect
                else lambda: s.add_runtime_property(
                    "/mixer/Level", {"type": "float", "value": 0.4}
                )
            )
            result = c.load(f.slot, "fixture")
            self.assertFalse(result.committed)
            self.assertEqual(b.calls[-1], "rollback")
            self.assertEqual(s.get_value(f.slot + "/EffectInstance/value"), 42)
            if not disconnect:
                self.assertEqual(s.get_value("/mixer/Level/value"), 0.4)

    def test_session_revoked_after_publication_never_reports_success(self):
        for phase in ("commit", "finalize", "raise"):
            with self.subTest(phase=phase):
                f, s, h, p, b, c = self.model(True)
                if phase == "commit":
                    c.publication_fault = lambda stage: (
                        p.disconnect() if stage == "both_swapped" else None
                    )
                else:

                    def finalize(prepared):
                        b.calls.append("finalize")
                        p.disconnect()
                        if phase == "raise":
                            raise OSError("device removed")
                        return FinalizeOutcome(Outcome.CONFIRMED)

                    b.finalize = finalize
                result = c.load(f.slot, "fixture")
                self.assertTrue(
                    result.committed
                )  # Historical tree swap, not device success.
                self.assertTrue(result.quarantined)
                self.assertIsNotNone(result.error)
                self.assertEqual(result.events, ())
                self.assertEqual(len(c.unresolved), 1)
                self.assertEqual(c.unresolved[0].loaded.handle.generation, 0)
                self.assertEqual(p.generation, 1)
                self.assertNotIn("rollback", b.calls)
                if phase == "commit":
                    self.assertNotIn("finalize", b.calls)
                calls = b.calls[:]
                self.assertTrue(c.load(f.slot, "fixture").quarantined)
                self.assertEqual(b.calls, calls)

    def test_invalid_slot_no_backend_and_preparation_exception_quarantines(
        self,
    ):
        f, s, h, p, b, c = self.model()
        self.assertEqual(
            c.load("/not/effect/slot", "fixture").error,
            "openapollo.invalid_plugin_slot",
        )
        self.assertEqual(b.calls, [])

        def fail(*args):
            raise OSError("disconnected after allocation")

        b.prepare = fail
        self.assertTrue(c.load(f.slot, "fixture").quarantined)
        self.assertEqual(c.unresolved, [])  # No invented token to release.


if __name__ == "__main__":
    unittest.main()
