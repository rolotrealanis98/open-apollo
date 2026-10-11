"""Injected attestations test ownership/rollback, never emulate hardware."""

import copy
import unittest
from threading import Thread
from test_plugin_transactions import Backend
import test_plugin_publication as fixtures
from plugin_publication import ControlReadback
from plugin_transactions import (
    PluginTransactions,
    PreparedControl,
    PrepareControlOutcome,
    ApplyControlOutcome,
    RestoreControlOutcome,
    Outcome,
)
from plugin_lifecycle import PluginLifecycle


class ControlBackend(Backend):
    def prepare_control(self, slot, lease, field, value):
        self.calls.append("prepare_control")
        self.prior = self.p.snapshot_control(slot, lease, field)
        self.request = value
        return PrepareControlOutcome(
            Outcome.CONFIRMED,
            PreparedControl(
                object(), lease, field, copy.deepcopy(self.prior.values)
            ),
        )

    def apply_control(self, prepared):
        self.calls.append("apply_control")
        if self.hook:
            self.hook()
        values = copy.deepcopy(prepared.current_values)
        if prepared.field == "Power":
            values["Power"] = self.request
        else:
            values.update(
                NormalizedValue=0.75, StringValue="confirmed", StepValue=0
            )
        rb = ControlReadback(prepared.handle, prepared.field, values)
        return ApplyControlOutcome(self.apply_status, rb)

    def restore_control(self, prepared):
        self.calls.append("restore_control")
        return RestoreControlOutcome(self.restore_status, self.prior)


class Controls(unittest.TestCase):
    def model(self, load=True):
        f = fixtures.PublicationTests()
        s, h, p = f.model()
        b = ControlBackend(p, f.result())
        b.hook = None
        b.apply_status = b.restore_status = Outcome.CONFIRMED
        c = PluginTransactions(p, b)
        if load:
            self.assertTrue(c.load(f.slot, "fixture").committed)
        b.calls = []
        return f, s, h, p, b, c

    def test_confirmed_power_and_parameter_use_readback_delta(self):
        f, s, h, p, b, c = self.model()
        for field, value in [
            ("Power", False),
            ("parameters/0/NormalizedValue", 0.8),
        ]:
            r = c.control(f.slot, field, value)
            self.assertTrue(r.committed)
            self.assertIsNone(r.error)
            self.assertTrue(
                all("/EffectInstance" not in event[0] for event in r.events)
            )
        self.assertEqual(
            s.get_value(f.slot + "/parameters/0/NormalizedValue"), 0.75
        )
        self.assertEqual(
            h.get_value(f.slot + "/parameters/0/StringValue"), "confirmed"
        )
        self.assertEqual(s.get_value(f.slot + "/EffectInstance"), 42)

    def test_capture_not_authority_and_invalid_fields_no_backend(self):
        f, s, h, p, b, c = self.model(False)
        p.stage(f.slot, f.result()).commit()
        self.assertFalse(c.control(f.slot, "Power", False).committed)
        self.assertEqual(b.calls, [])
        f, s, h, p, b, c = self.model()
        for field, value in [
            ("parameters/9/NormalizedValue", 0.2),
            ("Power", 1),
            ("parameters/0/NormalizedValue", float("nan")),
            ("Latency", 2),
            ("parameters/0/StepValue", "1"),
            ("parameters/0/StepValue", 2),
        ]:
            self.assertFalse(c.control(f.slot, field, value).committed)
        self.assertEqual(b.calls, [])

    def test_apply_and_publication_failure_restore_no_tree_mutation(self):
        for failure in ("apply", "publication"):
            f, s, h, p, b, c = self.model()
            before = copy.deepcopy((s.root, h.root))
            if failure == "apply":
                b.apply_status = Outcome.INDETERMINATE
            else:

                def fault(phase):
                    if phase == "both_swapped":
                        raise RuntimeError("injected")

                c.publication_fault = fault
            r = c.control(f.slot, "Power", False)
            self.assertFalse(r.committed)
            self.assertFalse(r.quarantined)
            self.assertIn("restore_control", b.calls)
            self.assertEqual(before, (s.root, h.root))

    def test_unknown_restore_quarantines_and_blocks_retry(self):
        f, s, h, p, b, c = self.model()
        b.apply_status = b.restore_status = Outcome.INDETERMINATE
        self.assertTrue(c.control(f.slot, "Power", False).quarantined)
        calls = b.calls[:]
        self.assertTrue(c.control(f.slot, "Power", True).quarantined)
        self.assertEqual(b.calls, calls)
        self.assertEqual(len(c.unresolved), 1)

    def test_disconnect_wrongthread_and_reentrancy(self):
        f, s, h, p, b, c = self.model()
        out = []
        t = Thread(
            target=lambda: out.append(c.control(f.slot, "Power", False))
        )
        t.start()
        t.join()
        self.assertEqual(
            out[0].error, "openapollo.plugin_publication_owner_mismatch"
        )
        self.assertEqual(b.calls, [])
        nested = []
        b.hook = lambda: nested.append(c.control(f.slot, "Power", False))
        self.assertTrue(c.control(f.slot, "Power", False).committed)
        self.assertEqual(
            nested[0].error, "openapollo.plugin_transaction_reentrant"
        )
        b.hook = p.disconnect
        self.assertTrue(c.control(f.slot, "Power", True).quarantined)

    def test_prepare_and_apply_view_mutation_not_adopted(self):
        f, s, h, p, b, c = self.model()
        original = b.prepare_control

        def prepare(*args):
            result = original(*args)
            s.add_runtime_property("/mixer/late", {"type": "int", "value": 1})
            return result

        b.prepare_control = prepare
        self.assertFalse(c.control(f.slot, "Power", False).committed)
        self.assertEqual(b.calls, ["prepare_control"])
        f, s, h, p, b, c = self.model()
        b.hook = lambda: s.add_runtime_property(
            "/mixer/late", {"type": "int", "value": 1}
        )
        result = c.control(f.slot, "Power", False)
        self.assertTrue(result.quarantined)
        self.assertIn("restore_control", b.calls)
        self.assertTrue(s.get_value(f.slot + "/Power"))

    def test_readonly_control_rejected_before_backend(self):
        f, s, h, p, b, c = self.model()
        node, _ = s._resolve_path(f.slot + "/Power")
        node["read_only"] = True
        # Both views must agree before request admission.
        target = h.root
        for part in f.slot.strip("/").split("/"):
            target = next(
                child for child in target["children"] if child["path"] == part
            )
        target["properties"]["Power"]["read_only"] = True
        result = c.control(f.slot, "Power", False)
        self.assertEqual(result.error, "openapollo.plugin_read_only")
        self.assertEqual(b.calls, [])

    def test_lifecycle_uses_transaction_controls(self):
        f, s, h, p, b, c = self.model()
        l = PluginLifecycle(s, helper_tree=h, transactions=c)
        self.assertTrue(l.handle(f.slot + "/Power/value", False).committed)
        self.assertEqual(b.calls, ["prepare_control", "apply_control"])


if __name__ == "__main__":
    unittest.main()
