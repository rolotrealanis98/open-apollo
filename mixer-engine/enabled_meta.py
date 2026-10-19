"""`enabled` metadata the Mixer Engine publishes on a property (x4 engine dump:
IOType, Stereo, preamp 48V and Pad), kept current from the values it
depends on.  console-rs reads it from the tree (ua-engine topology) and
re-reads a node when IOType, Stereo or HiZ change.

Rules (engine dump + Apollo x4 manual):
  - 48V and Pad: only in Mic mode, and not with a Hi-Z jack in (Hi-Z
    overrides Mic/Line);
  - IOType: not selectable while a Hi-Z jack is in;
  - Stereo (link): a Hi-Z input cannot be linked, so neither leg of its
    pair can start a link (an existing link can still be broken);
  - SRConvert: unavailable when the clock source is S/PDIF.
The tree's own `enabled` stays True wherever no rule applies.
"""

import re

_INPUT = re.compile(r"^(/devices/\d+/inputs/(\d+))(?:/.*)?$")
_CLOCK = "/ClockSource"
OBSERVER = "openapollo.enabled_meta"


def install(state):
    """Publish `enabled` on `state` now and keep it current."""
    paths = tuple(state._props)
    inputs = {m.group(1): int(m.group(2)) for p in paths
              if (m := _INPUT.match(p))}
    by_index = {(base.rsplit("/inputs/", 1)[0], i): base
                for base, i in inputs.items()}

    def flag(path, enabled):
        prop = state.get_prop_dict(path)
        if prop is not None:
            prop["enabled"] = enabled

    def hiz(base):
        return any(state.get_value(f"{p}/value") is True
                   for p in paths if p.startswith(base + "/preamps/")
                   and p.endswith("/HiZ"))

    def refresh_input(base):
        device, index = base.rsplit("/inputs/", 1)
        partner = by_index.get((device, int(index) ^ 1))
        jack = hiz(base)
        mic = state.get_value(f"{base}/IOType/value") == "Mic"
        flag(f"{base}/IOType", not jack)
        flag(f"{base}/Stereo", state.get_value(f"{base}/Stereo/value") is True
             or not (jack or (partner and hiz(partner))))
        for p in paths:
            if p.startswith(base + "/preamps/") and \
                    p.endswith(("/48V", "/Pad")):
                flag(p, mic and not jack)

    def refresh_clock():
        spdif = state.get_value(_CLOCK + "/value") == "S/PDIF"
        for p in paths:
            if p.endswith("/SRConvert") and _INPUT.match(p):
                flag(p, not spdif)

    def refresh_all():
        for base in inputs:
            refresh_input(base)
        refresh_clock()

    def changed(path, value):
        path = path.removesuffix("/value")
        if path == _CLOCK:
            refresh_clock()
            return
        m = _INPUT.match(path)
        if m is None:
            return
        base = m.group(1)
        if path.rsplit("/", 1)[1] in ("IOType", "Stereo", "HiZ"):
            refresh_input(base)
            # a Hi-Z change also moves its partner's link
            device, index = base.rsplit("/inputs/", 1)
            if (partner := by_index.get((device, int(index) ^ 1))):
                refresh_input(partner)

    refresh_all()
    state.register_change_observer(OBSERVER, changed)
