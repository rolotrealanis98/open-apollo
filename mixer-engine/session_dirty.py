"""The session's /Dirty flag ("modified since the last open or save").

Ported from the Codex daemon (session_dirty.py), with the property set and
the clear points checked against UA's Mixer Engine (macOS, arm64 slice):

  Set:   CMxBase::ActuallySetProperty (0x100051ea4) calls
         DMixer::MixerTreeHasBeenModified(true) after a property set the
         object's TrackChanges accepts: flag 0x4 in g_PropertyDescriptions,
         with per-class overrides (CMxOutput never tracks Mute, nor
         OutputDestination on one IOType; CMxInput tracks Pad only with a
         preamp).  CMxRoot::MixerTreeHasBeenModified (0x100090d04) acts
         only while the mixer is up and no StSuppressDirtying is held.
  Clear: LoadXmlOnMessageThreadCallback and CreateMixerFromFactoryDefault
         (MixerTreeHasBeenModified(false)); SessionFileManager::Open, ::Save
         after a successful write, and CMxRoot::setSnapshot set the root's
         Dirty property (0xba) to 0.  SaveLastUsedSession does not.
  Client: a SET of /Dirty writes the flag (CMxRoot::SetProperty, 0xba).

Tracked here: the flag-0x4 properties of the input, preamp and output
objects in the device maps whose overrides are known (Codex's RE notes,
analysis/native-dirty-transition.txt): not sends or aux buses, not output
Mute or OutputDestination.  Readback and status pushes are suppressed;
nothing counts before the session is up.  /Dirty is device-session state:
never saved, False at start (the engine loads the last session clean).
"""

import re
from contextlib import contextmanager

DIRTY = "/Dirty/value"

_TRACKED = re.compile(
    r"^/devices/\d+/(?:"
    r"inputs/\d+/(?:Mute|Solo|FaderLevel|FaderLevelTapered|Pan|Pan2|Name|"
    r"StereoName|Stereo|IOType|ChannelHidden|SRConvert|RecordPreEffects|"
    r"DelayCompGroup|OutputDestination|OutputMonitorMixEnable)"
    r"|inputs/\d+/preamps/\d+/(?:Gain|GainTapered|48V|LowCut|Phase)"
    r"|outputs/\d+/(?:Solo|FaderLevel|FaderLevelTapered|Pan|Pan2|Name|"
    r"StereoName|Stereo|IOType|ChannelHidden)"
    r")$")


def canonical(path: str) -> str:
    """/a//b/value -> /a/b (no /value suffix, no empty parts)."""
    p = "/" + "/".join(x for x in path.split("/") if x)
    return p[:-6] if p.endswith("/value") else p


def tracked(path: str) -> bool:
    return bool(_TRACKED.match(canonical(path)))


class SessionDirty:
    OBSERVER = "openapollo.internal.session_dirty"

    def __init__(self, state, *, ready, helper=None):
        """`ready()` says whether the session is up (the daemon's
        initialized transition)."""
        self.state, self.ready, self.helper = state, ready, helper
        self.suppression = 0
        prop = state.get_prop_dict("/Dirty")
        if prop is not None:
            prop["value"] = False
        state._dirty.pop(DIRTY, None)
        state._dirty.pop("/Dirty", None)
        state.register_change_observer(self.OBSERVER, self._changed)

    @contextmanager
    def suppress(self):
        """Changes inside are not session edits (readback, status)."""
        self.suppression += 1
        try:
            yield
        finally:
            self.suppression -= 1

    def _changed(self, path: str, value):
        if canonical(path) == "/Dirty":
            # whoever set it (a client, or mark/clear): never saved
            self.state._dirty.pop(DIRTY, None)
            self.state._dirty.pop("/Dirty", None)
            return
        if not self.suppression and self.ready() and tracked(path):
            self.mark()

    def mark(self):
        """An accepted session edit."""
        if self.ready() and not self.suppression:
            self._set(True)

    def clear(self):
        """A session was opened, created or saved."""
        self._set(False)

    def _set(self, flag: bool):
        prop = self.state.get_prop_dict("/Dirty")
        if prop is None or prop.get("value") is flag:
            return
        # Server-owned: hardware=False, the router never sees it
        self.state.set(DIRTY, flag, hardware=False)
        if self.helper is not None:
            meta = (self.helper.root.get("properties") or {}).get("Dirty")
            if isinstance(meta, dict):
                meta["value"] = flag
                self.helper._notify(DIRTY, flag)
