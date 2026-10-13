#!/usr/bin/env python3
"""Update the external, hand-kept UI catalogue without overwriting unrelated work.

This records source/mock/software verification, never hardware acceptance or new
pixel alignment. Run build_ledger.py afterwards using its existing report inputs.
"""
import argparse
from datetime import datetime, timezone
from pathlib import Path
import shutil
import tempfile

GAPS = {
    "P0-scenes": ("P0", "Scene command support", "software verified", "SceneOperation keys and rename are covered by engine-model/tests/wiring.rs and daemon P0 acceptance; native DSP restoration remains unverified."),
    "P0-session-folders": ("P0", "Session folder operations", "software verified", "Console payloads and daemon create/rename/delete paths are covered by sessions.rs and daemon_p0.rs, including current-path updates."),
    "P0-session-dialog-guard": ("P0", "Session dialog guard", "software verified", "Client-side name/path guards and save-changes flow are implemented; no Mac Console or engine-host dialogs were driven."),
    "P0-insert-copy-paste": ("P0", "Insert clipboard and structural undo", "partial", "CopyInsert/PasteInsert/MoveInsert now preserve synthetic parameter subtrees and structural undo in the no-hardware daemon. Production DSP loading remains fail-closed/unproved."),
    "P1-context-type": ("P1", "Native context_type coverage", "partial", "Mute/Pan have 1.3.1 provenance; tapered faders have retained 1.2 evidence. Solo, sends and direct-dB setters still need native traffic confirmation."),
    "P1-reset-default": ("P1", "Numeric-control remote-default reset", "software verified", "Pan reset emits the literal default string; wiring.rs and native trace fixtures verify it. Insert SET DEFAULT is a separate unresolved operation."),
    "P1-assign-bracket": ("P1", "Insert assignment wire form", "software verified", "Assign is unbracketed and without a query; remove remains bracketed. engine-model/tests/inserts.rs and retained 1.2 traces cover this."),
    "P1-settings-posts": ("P1", "Settings post writer gate", "software verified", "Settings posts use Store.post, with suppression diagnostics and a closed-by-default writer. wiring.rs covers gated dispatch."),
    "G-plugin-toolbar": ("P1", "Plugin toolbar and native DSP acceptance", "partial", "Preset navigation/save/browser and PluginBar copy/paste are routed; synthetic daemon clipboard is verified. A/B only switches chrome state and its engine action remains unimplemented. Sidechain/IN-OUT/multi-out, typed-entry acceptance, HTML chrome ownership, native preset state, DSP execution and real demo activation remain open."),
    "G-preset-assign": ("P1", "Preset routing and file compatibility", "partial", "Browser activation, explicit/selected-channel targets and drag assignment are wired. Daemon supports its own oapreset files and scalar-only channel XML; opaque native FXP/plugin-state restore is refused, not verified."),
    "G-drag-drop": ("P1", "Drag/drop acceptance", "partial", "Desktop browser/preset/insert routing exists and software daemon moves are verified. Native pixel/runtime parity and iPad long-press forwarding remain open."),
    "G-correction-curve": ("P2", "Correction SVG retrieval and presentation", "partial", "Main monitor renders standard-base64 SVG, refreshes on count pushes, and passes synthetic pixels plus GPU refresh/reuse tests. Headphone images are cached; headphone graph placement and native calibrated-profile pixel parity remain open."),
    "G-tap-tempo": ("P2", "Tap tempo and incoming MIDI", "partial", "Four-tap tempo and configured NOTE/nonzero-controller MIDI input are implemented. Native event time units are unresolved; receipt timing is conservative."),
    "G-midi-prefs": ("P2", "MIDI actions and native mapping parity", "partial", "Settings remain explicit client preferences. Bounded native MidiBuffer decoding and polling are loopback-tested; Program Change uses existing zero-based scene indices as a Rust policy. NOTE-to-scene mapping remains unresolved."),
    "G-multi-device": ("P2", "Multiple Apollo UI", "partial", "Sparse unit identities, cascade input grouping, per-unit subscriptions/writes and offline filtering are mock-tested. Native tabs and shared control-room ownership remain unverified."),
    "G-popup-windows": ("P1", "Separate-window host parity", "partial", "Rust FFI and desktop layer/window placement exist and windows_check passes. Apple host integration and HTML plugin chrome ownership remain open."),
}

ELEMENTS = {
    "dd.preset": ("rendered", "Desktop drag routing exists; native visual parity is unverified.", "engine-model/tests/presets.rs; rack drag routing; INTEGRATION.md P0/P1"),
    "dd.insertmove": ("rendered", "Desktop move/copy routing exists; native drag visual parity is unverified.", "engine-model/tests/inserts.rs; engine-model/tests/daemon_p0.rs"),
    "x.devicetabs": ("missing", "Cascade input grouping is built; native device tabs are still absent.", "engine-model/tests/multidevice.rs verifies grouping/offline identity, not tab pixels"),
    "set.midi": (None, None, "engine-model/src/midi.rs synthetic codec/actions and loopback RPC tests; native timestamp and NOTE-scene mapping remain open"),
    "mon.correction": (None, None, "engine-model/src/correction.rs + houston SVG pixels + gfx explicit headless correction refresh test; native calibrated-profile parity not claimed"),
    "br.presets": (None, None, "rack/tests/browser_selection.rs + engine-model/tests/presets.rs; native opaque preset compatibility remains open"),
    "br.sessions": (None, None, "engine-model/tests/sessions.rs + daemon_p0.rs; no supervised native session test"),
}

BEGIN = "# BEGIN CODEX CONTINUATION SCORECARD REFRESH\n"
END = "# END CODEX CONTINUATION SCORECARD REFRESH\n"

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("catalog", type=Path)
    args = parser.parse_args()
    old = args.catalog.read_text()
    if BEGIN in old:
        left, rest = old.split(BEGIN, 1)
        if END not in rest:
            raise SystemExit("existing managed refresh block has no end marker")
        _, right = rest.split(END, 1)
        old = left + right
    block = BEGIN + "GAPS.update(" + repr(GAPS) + ")\n"
    block += "_continuation_elements = " + repr(ELEMENTS) + "\n"
    block += "for _e in ELEMENTS:\n    if _e['id'] in _continuation_elements:\n        _s, _v, _t = _continuation_elements[_e['id']]\n        if _s is not None:\n            _e['vis'] = {'status': _s, 'evidence': _v}\n        _e['verify'] = _e.get('verify', '') + '; ' + _t\n"
    block += END
    updated = old.rstrip() + "\n\n" + block
    if updated == args.catalog.read_text():
        print("Catalogue already refreshed")
        return
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%f")
    backup = Path(tempfile.gettempdir()) / f"apollo-ledger-catalog-{stamp}.py"
    shutil.copy2(args.catalog, backup)
    args.catalog.write_text(updated)
    print(f"Refreshed {args.catalog}; preserved original at {backup}")

if __name__ == "__main__":
    main()
