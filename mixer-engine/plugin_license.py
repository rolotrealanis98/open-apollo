"""
Plug-in licence status for /plugins/N, from the Apollo's own answer.

UA's Mixer Engine shows, for every plug-in of its catalogue, whether the
user may run it: /plugins/N/Authorized, Status, Demoing and
DemoDaysRemaining.  The answer is the device's, not the host's: the Apollo
keeps a board table of 768 words, one per plug-in render id 0x800..0xAFF,
and returns it to a board query.  With the driver loaded with
plugin_host=1 (UA_FEAT2_PLUGIN_HOST) the daemon reads it through
UA_IOCTL_BOARD_QUERY (hardware.HardwareBackend.board_query) and this module
turns it into those four properties.

Nothing here decides anything for the device or the driver:
  - the words are decoded exactly as UA's SDK decodes them (state_of), the
    same decision the driver makes before it hosts a plug-in;
  - the strings are the ones the Mac Mixer Engine reported (status_for);
    a state with no observed string, a plug-in without a render id and a
    missing answer all publish the empty status, never an invented one;
  - no answer is kept: a failed query publishes the unknown status for
    every plug-in, and the daemon withdraws an answer (invalidate) when
    the device reconnects or changes rate, before it asks again;
  - nothing starts a demo and nothing is written to the device: the board
    query is read-only.  Demos are started in UA's own software;
  - the table is never logged, only the number of plug-ins per decision.

Render ids come from the plug-in DSP manifest that `oa-assets dsp` writes
(/lib/firmware/universal-audio/plugins/manifest.json, format
"open-apollo-plugin-dsp/1"): each entry of its "plugins" list has
"plugin_id" (a string) and "render_id" (an int, or null when the plug-in
has none).  The daemon's catalogue (ua_catalogue) lays the plug-ins on
/plugins/N in its own order, each entry with its "plugin_id", so
/plugins/N -> plugin_id -> render_id -> words[render_id - 0x800].

No hardware access of its own: the query is a callable the caller hands
in, and the caller decides on which thread it runs (it blocks for up to
~2 s while the device answers).
"""

from __future__ import annotations

import errno
import json
import logging
import time
from collections import Counter
from pathlib import Path
from typing import Callable

log = logging.getLogger(__name__)

# The board table (driver/ua_ioctl.h, UA_BOARD_QUERY_*)
BOARD_WORDS = 768
BOARD_BASE = 0x800          # words[i] is for render id 0x800 + i

# Record states, UA's SDK numbering (driver/ua_ioctl.h UA_PLUGIN_LICENSE_*)
AUTHORIZED = 0
DEMO = 1                    # a demo is running
NOT_STARTED = 2             # demo not started
EXPIRED = 3                 # demo expired
UNAVAILABLE = 4             # also the driver's "unfilled" sentinel

# The words with a fixed meaning; any other word is a running demo whose
# low 31 bits are the countdown (days << 12, plus minutes)
_WORD_STATES = {
    0x81000000: AUTHORIZED,
    0x82000000: AUTHORIZED,
    0x80000000: NOT_STARTED,
    0x00000000: EXPIRED,
    0x83000000: UNAVAILABLE,
}

DEMO_DAYS_MAX = 365         # the SDK caps the countdown at a year

# The four properties, in publishing order, with their tree types
FIELDS = (("Authorized", "bool"), ("Status", "string"),
          ("Demoing", "bool"), ("DemoDaysRemaining", "int"))

UNKNOWN = {"Authorized": False, "Status": "", "Demoing": False,
           "DemoDaysRemaining": 0}

MANIFEST_FORMAT = "open-apollo-plugin-dsp/1"
DEFAULT_MANIFEST = Path("/lib/firmware/universal-audio/plugins/manifest.json")

# No more than one query per this many seconds
MIN_QUERY_INTERVAL = 5.0

# UA_FEAT_FEATURES2 / UA_FEAT2_PLUGIN_HOST (hardware.py, driver/ua_ioctl.h)
_FEAT_FEATURES2 = 1 << 31
_FEAT2_PLUGIN_HOST = 1 << 0


# ── Decoding one word ───────────────────────────────────────────────

def state_of(word: int) -> int:
    """The SDK's record state for a board-table word: 0x81000000 and
    0x82000000 authorized, 0x80000000 demo not started, 0 demo expired,
    0x83000000 unavailable, any other word a running demo."""
    return _WORD_STATES.get(word, DEMO)


def demo_days(word: int) -> int:
    """Days left of a running demo: whole days of the countdown, a partial
    day counting as one, capped at 365 as the SDK caps it."""
    days = ((word >> 12) & 0x7FFFF) + (1 if word & 0xFFF else 0)
    return min(DEMO_DAYS_MAX, days)


def status_for(state: int | None, days: int = 0) -> dict:
    """The four /plugins/N properties for a record state.

    The strings are what the Mac Mixer Engine reported for these states
    (a capture of its /plugins tree with authorized plug-ins, running,
    unstarted and expired demos): an observed correspondence, not a proven
    one-to-one enum.  It said "Demo (13 days left)"; "1 day left" in the
    singular was never observed, so the plural form is used for every
    count.  It also reported Demoing true for every authorized plug-in, so
    that is what state 0 publishes.  State 4 (unavailable), an unknown
    state and no answer at all (None) publish the empty status: nothing
    is invented.
    """
    if isinstance(state, bool) or not isinstance(state, int):
        return dict(UNKNOWN)
    if state == AUTHORIZED:
        return {"Authorized": True, "Status": "Authorized for all devices",
                "Demoing": True, "DemoDaysRemaining": 0}
    if state == DEMO:
        n = max(0, min(DEMO_DAYS_MAX, int(days)))
        return {"Authorized": False, "Status": f"Demo ({n} days left)",
                "Demoing": True, "DemoDaysRemaining": n}
    if state == NOT_STARTED:
        return {"Authorized": False, "Status": "Demo not started",
                "Demoing": False, "DemoDaysRemaining": 0}
    if state == EXPIRED:
        return {"Authorized": False, "Status": "Demo expired",
                "Demoing": False, "DemoDaysRemaining": 0}
    return dict(UNKNOWN)


def word_for(words, render_id) -> int | None:
    """The table word for a render id; None without a table, for a render
    id outside 0x800..0xAFF or one that is not an int."""
    if words is None or len(words) != BOARD_WORDS:
        return None
    if isinstance(render_id, bool) or not isinstance(render_id, int):
        return None
    if not BOARD_BASE <= render_id < BOARD_BASE + BOARD_WORDS:
        return None
    return words[render_id - BOARD_BASE]


def decide(words, render_id) -> tuple[int | None, dict]:
    """(record state or None, the four properties) for one plug-in."""
    word = word_for(words, render_id)
    if word is None:
        return None, dict(UNKNOWN)
    state = state_of(word)
    return state, status_for(state, demo_days(word) if state == DEMO else 0)


# ── The driver feature ──────────────────────────────────────────────

def plugin_host_available(backend) -> bool:
    """True when the open driver advertises UA_FEAT2_PLUGIN_HOST (loaded
    with plugin_host=1).  False for an older driver, plugin_host=0, a
    backend without the feature words (the USB parts) or no backend."""
    if backend is None:
        return False
    has = getattr(backend, "has_feature", None)
    if not callable(has) or not has(_FEAT_FEATURES2):
        return False
    get = getattr(backend, "features2", None)
    words2 = get() if callable(get) else 0
    return type(words2) is int and bool(words2 & _FEAT2_PLUGIN_HOST)


# ── Render ids ──────────────────────────────────────────────────────

def load_render_ids(path: str | Path | None = None) -> dict[str, int]:
    """{plugin_id: render_id} from the plug-in DSP manifest (`path`, else
    DEFAULT_MANIFEST).  A plug-in whose render id is null, or that two
    entries give different render ids, is left out (its status stays
    unknown).  {} when the manifest is missing or not a
    "open-apollo-plugin-dsp/1" document."""
    p = Path(path) if path is not None else DEFAULT_MANIFEST
    try:
        with open(p, encoding="utf-8") as f:
            doc = json.load(f)
    except FileNotFoundError:
        log.info("No plug-in DSP manifest (%s): plug-in licence status "
                 "stays empty; run `oa-assets dsp` to install it", p)
        return {}
    except (OSError, ValueError) as e:
        log.warning("Plug-in DSP manifest %s unreadable: %s", p, e)
        return {}
    if not isinstance(doc, dict) or doc.get("format") != MANIFEST_FORMAT \
            or not isinstance(doc.get("plugins"), list):
        log.warning("%s is not a %s manifest", p, MANIFEST_FORMAT)
        return {}
    out: dict[str, int] = {}
    ambiguous: set[str] = set()
    for entry in doc["plugins"]:
        if not isinstance(entry, dict):
            continue
        pid, rid = entry.get("plugin_id"), entry.get("render_id")
        if not isinstance(pid, str) or not pid:
            continue
        if isinstance(rid, bool) or not isinstance(rid, int) or rid < 0:
            continue
        if out.get(pid, rid) != rid:
            ambiguous.add(pid)
        out[pid] = rid
    for pid in ambiguous:
        del out[pid]
    if ambiguous:
        log.warning("Plug-in DSP manifest %s gives %d plug-ins more than one "
                    "render id: their licence status stays empty", p,
                    len(ambiguous))
    return out


def catalogue_render_ids(plugins, render_ids: dict) -> list:
    """The render id of each catalogue plug-in (/plugins/N in order), None
    where the plug-in has no plugin_id or the manifest does not know it."""
    out = []
    for p in plugins:
        pid = p.get("plugin_id") if isinstance(p, dict) else None
        out.append(render_ids.get(pid) if isinstance(pid, str) else None)
    return out


# ── The mirror ──────────────────────────────────────────────────────

class LicenseMirror:
    """Publishes the device's answer on /plugins/N.

    `publish(path, value, kind)` sets one property (kind: "bool",
    "string" or "int"); `plugins` is the catalogue's plug-in list in
    /plugins/N order; `render_ids` is {plugin_id: render_id}
    (load_render_ids; the attribute may be replaced); `query()` returns
    (date, epoch, words) or raises, e.g. HardwareBackend.board_query.

    refresh() does everything on the caller's thread.  A caller that must
    not block (the daemon's event loop) splits it: begin() on its own
    thread, fetch() on a worker (it only runs the query), apply() back on
    its own thread.  A result fetched before the last invalidate() is
    dropped, so an answer from before a reconnect or a rate change is
    never shown.
    """

    def __init__(self, publish: Callable, plugins, render_ids: dict,
                 query: Callable, *, clock: Callable = time.monotonic,
                 min_interval: float = MIN_QUERY_INTERVAL):
        self._publish = publish
        self.plugins = list(plugins)
        self.render_ids = dict(render_ids)
        self._query = query
        self._clock = clock
        self.min_interval = float(min_interval)
        self._last_query: float | None = None
        self.generation = 0          # bumped by invalidate()
        self.showing_answer = False  # a device answer is published now
        self.queries = 0
        self.last_error: int | None = None   # errno of the last failure
        self.last_counts: dict = {}

    # ── Rate limit and tokens ──

    def wait_time(self, now: float | None = None) -> float:
        """Seconds until the next query may run (0: now)."""
        if self._last_query is None:
            return 0.0
        now = self._clock() if now is None else now
        return max(0.0, self.min_interval - (now - self._last_query))

    def begin(self, now: float | None = None) -> int | None:
        """Take the next query slot: a token for apply(), or None while the
        rate limit says wait."""
        now = self._clock() if now is None else now
        if self.wait_time(now) > 0:
            return None
        self._last_query = now
        self.queries += 1
        return self.generation

    def invalidate(self):
        """The device's epoch changed (reconnect, rate change, device
        gone): results still in flight are dropped and an answer on show is
        replaced by the unknown status until a new one arrives."""
        self.generation += 1
        if self.showing_answer:
            self.publish_unknown()

    # ── The query (any thread) ──

    def fetch(self):
        """Run the query.  Returns the 768 words, or the exception it
        raised (an OSError from the driver, or ValueError for an answer
        of the wrong shape).  Publishes nothing, so any thread may run
        it."""
        try:
            result = self._query()
            _date, _epoch, words = result
            words = tuple(words)
            if len(words) != BOARD_WORDS or not all(
                    type(w) is int and 0 <= w <= 0xFFFFFFFF for w in words):
                raise ValueError("board table of the wrong shape")
            return words
        except Exception as e:      # whatever it was: no answer
            return e

    # ── Publishing (the caller's own thread) ──

    def apply(self, result, token: int) -> bool:
        """Publish a fetch() result: the decisions for an answer, the
        unknown status for every plug-in on a failure.  Returns False (and
        publishes nothing) for a result from before the last
        invalidate()."""
        if token != self.generation:
            log.debug("Plug-in licence answer from an earlier epoch dropped")
            return False
        if isinstance(result, BaseException) or result is None:
            err = getattr(result, "errno", None)
            self.last_error = err if isinstance(err, int) else errno.EIO
            name = errno.errorcode.get(self.last_error, str(self.last_error))
            if self.last_error == errno.EOPNOTSUPP:
                log.info("Plug-in licence query not served (plug-in host "
                         "off): no licence status")
            else:
                log.warning("Plug-in licence query failed (%s): no licence "
                            "status for %d plug-ins", name, len(self.plugins))
            self.publish_unknown()
            return True
        self.last_error = None
        counts = self._publish_all(result)
        self.showing_answer = True
        self.last_counts = counts
        log.info("Plug-in licences from the device: %d authorized, %d demo "
                 "running, %d demo not started, %d demo expired, %d "
                 "unavailable, %d unknown (no render id in the table)",
                 counts["authorized"], counts["demo"], counts["not_started"],
                 counts["expired"], counts["unavailable"], counts["unknown"])
        return True

    def refresh(self) -> bool:
        """Query and publish on this thread.  False (nothing done) while
        the rate limit says wait."""
        token = self.begin()
        if token is None:
            return False
        self.apply(self.fetch(), token)
        return True

    def publish_unknown(self):
        """The unknown status for every plug-in."""
        self._publish_all(None)
        self.showing_answer = False

    def _publish_all(self, words) -> Counter:
        names = {AUTHORIZED: "authorized", DEMO: "demo",
                 NOT_STARTED: "not_started", EXPIRED: "expired",
                 UNAVAILABLE: "unavailable", None: "unknown"}
        counts = Counter({n: 0 for n in names.values()})
        rids = catalogue_render_ids(self.plugins, self.render_ids)
        for index, rid in enumerate(rids):
            state, status = decide(words, rid)
            counts[names[state]] += 1
            base = f"/plugins/{index}/"
            for key, kind in FIELDS:
                self._publish(base + key, status[key], kind)
        return counts
