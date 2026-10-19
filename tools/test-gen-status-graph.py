#!/usr/bin/env python3
"""Synthetic-notes tests for gen-status-graph.py (no RE notes needed):
python3 tools/test-gen-status-graph.py"""
import importlib.util
import tempfile
import xml.dom.minidom
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "gen", Path(__file__).with_name("gen-status-graph.py"))
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

PARITY = """# 03 hardware interface

| Status | Meaning |
|---|---|
| DONE | legend rows are not items |

| ID | Offset | Meaning | Status | Linux detail |
|---|---|---|---|---|
| A01 | W+0x00 | DSP status word | DIFFERENT | read only for the info ioctl |
| A02 | 0x2264 | ISR_HI | **DONE** | `ua_core.c`, seen on titan |
| A03 | 0x2268 | spare | MISSING | meaning unknown [L] |
| A04 | 0x226c | mask | MISSING | nothing sends it |
| A05 | 0x2270 | unused | N/A | |
| A06 | 0x2274 | demo block | OUT-OF-SCOPE | named only |
| A07 | 0x2278 | write \\| read | DONE | untestable on titan |
| A08 | 0x227c | sends | WRONG | wrong target |
| A09 | 0x2280 | echo | CACHE-ONLY | stored only |
| A10 | 0x2284 | slot | **PLUGIN-HOST** (triage) — MISSING | allocator |
| A11 | 0x2288 | peer | **BLOCKED-HW** (triage) — MISSING | second unit |
| A12 | 0x228c | msg | BLOCKED-HW | payload undecoded |
"""

PLAN = """# plan

## A. Daemon correctness

| ID | Item | Source | Size | Needs | State |
|---|---|---|---|---|---|
| A1 | Bus table | 04 X1 | S | code + hw | done (titan 2026-10-11): checked |
| A2 | CUE map | 04 X2 | S | code | done de418d0 |
| A3 | Old row | 04 X3 | S | code | split into A4-A5 |
| A4 | Strips | x | L | RE + titan | in progress: bisect |
| A5 | Second unit | x | S | second Apollo | todo |
| A6 | Param images | x | M | RE | todo |
| A7 | Wire it | x | S | code | todo |
| A8 | Plugin license check | x | S | code | blocked: decision |
| A9 | Unison lanes | x | M | strip engine | not now (strip engine deferred) |

## P. Plugin hosting

| ID | Item | Source | Size | Needs | State |
|---|---|---|---|---|---|
| P1 | Hosting | x | L | code | todo |

## Waves

| W1 | not a plan row |
"""


def main():
    with tempfile.TemporaryDirectory() as t:
        t = Path(t)
        (t / "parity").mkdir()
        (t / "parity" / "03-hw-interface.md").write_text(PARITY)
        (t / "master-implementation-plan.md").write_text(PLAN)
        data = gen.export(t)

        par = data["parity"]
        assert [g["name"] for g in par] == ["HW interface"], par
        got = {it[0]: it[2] for it in par[0]["items"]}
        # legend table skipped, N/A and out-of-scope not counted
        assert got == {"A01": "partial", "A02": "verified", "A03": "re",
                       "A04": "todo", "A07": "done", "A08": "partial",
                       "A09": "todo", "A10": "plugin", "A11": "blocked",
                       "A12": "re"}, got
        labels = {it[0]: it[1] for it in par[0]["items"]}
        assert labels["A07"] == "write | read", labels

        plan = data["plan"]
        assert [g["name"] for g in plan] == ["Daemon"], plan   # P skipped
        got = {it[0]: it[2] for it in plan[0]["items"]}
        assert got == {"A1": "verified", "A2": "done", "A4": "partial",
                       "A5": "blocked", "A6": "re", "A7": "todo",
                       "A8": "blocked", "A9": "plugin"}, got
        labels = {it[0]: it[1] for it in plan[0]["items"]}
        assert labels["A8"] == "(not listed)", labels

        svg = gen.render(data, "dark")
        doc = xml.dom.minidom.parseString(svg)      # well-formed
        cells = [r for r in doc.getElementsByTagName("rect")
                 if r.getAttribute("class") in gen.ORDER and
                 r.getElementsByTagName("title")]
        n_models = sum(len(g["items"]) for g in gen.load_models())
        n_library = sum(len(g["items"]) for g in gen.load_library())
        assert len(cells) == 10 + 8 + n_models + n_library, len(cells)
        assert gen.render(data, "dark") == svg       # deterministic
        for name, text in gen.outputs(data).items():
            if name.endswith(".svg"):
                xml.dom.minidom.parseString(text)
    print("ok")


if __name__ == "__main__":
    main()
