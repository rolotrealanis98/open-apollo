# WEBBRIDGE: the host bridge UA's HTML plug-in GUIs expect

This note covers UA's LUNA "lunacomponent" GUIs (`gui.bundle/Contents/Resources/html/{index.html,bundle.js,manifest.json}`)
and what console-rs must provide to run them in wry/WebKitGTK against the UA Mixer Engine.

Tags: **[obs]** seen in code or a binary, with a function, line or address. **[inf]** inferred. **[unk]** unknown.

Sources:
* `bundle.js` of `uaudio_ua_1176_rev_a`: `@uaudio/client` 1.7.0, gui 1.12.0. Pretty-printed with esbuild 0.28 in a scratch dir; line numbers below (`L6654`) refer to that copy.
* `bundle.js` of `uaudio_ssl_g_bus_compressor`: client 1.9.0, gui 1.12.2. Cited as `G:L…`.
* A grep of the other 65 bundles.
* Windows `Rack.dll`, UAD Console 1.2.9: `rack/tools/q.py` plus Ghidra pseudo-C in `rack/decomp/{1809ed3b0,1809ecc10,1809e95e0,1816bce60,1809eb610,181a72770}.c`.
* Read-only `strings` and `lsof` on the Mac (UAD Console 1.3.1).

Nothing was connected to any engine.

## 0. Summary

| Question | Answer |
|---|---|
| Transport | A WebSocket from the page to `ws://<ip|host>:<port>`, with one text frame per message. **[obs]** `createWSConn` L6590–6603. Only `ws` and `ble` are registered. `ble` is an iframe `window.parent.postMessage` transport, which is useless in wry. |
| URL to load | `…/html/index.html?host=127.0.0.1&port=<bridge port>&path=<plugin node path>`. Console builds the same three keys. Do **not** put `path` first (§1). |
| Dialect | UANet, the same text dialect as engine TCP 4710. Requests are `verb path?urlencoded-params [json-value]`; replies and pushes are JSON `{path,data,parameters,error}`. Over WebSocket there is no NUL terminator. **[obs]** encoder `W.encode` L6410. **[inf]** "same as 4710", by comparison with `console-port/protocol/PROTOCOL.md` §2 and §3. |
| Tree shape | It is **not** the engine's tree. The GUI needs a LUNA/HB plug-in subtree: `<path>` is an `effect` node, `<path>/controls/<id>` are `effect_control` nodes with snake_case properties (`normalized_value`, `real_value`, `string_value`, …), plus meters and commands. Controls are addressed **by string id** (`attack`, `sc_filter`), not by engine ordinal. |
| Port 4718 | This is the **HB "deviced" default port** (`default_deviced_port` = 0x126e in Rack.dll), not the UA Mixer Engine. On the Mac nothing listens on 4718; the engine listens only on 4710 **[obs lsof]**. The engine binary contains WebSocket-upgrade strings but none of the GUI's control vocabulary, so console-rs must run its own local WebSocket bridge that translates to 4710. |

## 1. URL parameters

There are two parsers [obs]:
* `MixerBase.doConnect` (L6650) uses `M(window.location.search)`, a hand split on `&`/`=`. It keeps only pairs with exactly one `=` and runs `decodeURIComponent` on the values.
* `Application.getURLParams` (L17418) uses `new URLSearchParams(window.location.href)`. Because it receives the full href and not just the query, **the first query key is glued to the URL** (`file:///…/index.html?ip`) and cannot be found. The page then loses `path`, and the GUI starts **offline** (`n2 = !t2.path`, L17292), meaning no UANet connection. Console puts `host` first.

| Key | Read by | Default | Meaning |
|---|---|---|---|
| `ip`, `host` | doConnect `j(o2,["ip","host"],…)`; getURLParams → deviceId | `deviceId` from getURLParams | WebSocket host. |
| `deviceId` | getURLParams only, then passed to doConnect as the fallback ip | – | Host alias. |
| `port` | both | `"4718"` (L6654) | WebSocket port. |
| `path` | both. getURLParams wins: `basePath = r2 || j(o2,"path")` | none, which means offline mode | Base node of the plug-in instance. Trailing `/` is stripped (`k()`). |
| `transport` | getURLParams | `"ws"` | `ws` or `ble`. Anything else throws `no transport registered` (L17528). |
| `setplugins` | doConnect, bool | `true` | Parsed and stored in `this.setPlugins` but **never used** in client 1.7.0 or 1.9.0 [obs]. |
| `meters` | doConnect, bool (`"1"`/`"true"`/`"0"`/`"false"`) | `true` | When true, each meter's `<meter>/level/value` is subscribed one by one (`createMeterValues`, L7202). |
| `inHardwareOnlyDaemon` | not a URL key | `""` | A doConnect option. When set, the GUI skips connecting if ip=`localhost` or if the URL key it names is false. No caller passes it, so it is unused [obs]. |

Globals the page reads:
* `window.UA_DEBUG="1"` turns on verbose logs and sessionStorage writes (L6107).
* `window.ua_amber_env` (L8656).
* The functions in §6.

## 2. Transports

| `transport` | Class | Mechanism |
|---|---|---|
| `ws` (default) | `Y` + `K` (L6498, L6430) | `new WebSocket("ws://"+ip+":"+port)`. Outgoing text is `W.encode(msg)`. Each incoming frame is passed to `JSON.parse`, so a frame must be exactly one JSON object with no NUL. |
| `ble` | `ir` (L17184) | Despite the name, this is `window.parent.postMessage({command, client:"<frameName>:<uuid>", address, port, payload}, window.origin)`. Commands are `connect`/`message`/`close`. It expects back `{command:"open"|"message"|"error"|"close"|"pong", client, address, port, payload}`. The payload is the **structured** `{action,path,data,parameters}` object, not encoded. It needs an iframe and a non-null `window.origin`. **Do not use it** [inf]. |

There is **no** `chrome.webview`, `webkit.messageHandlers` or `ipc` transport for UANet in the bundle [obs].

## 3. Connection lifecycle

* **Connect** (`Y.connect` → `tryWSConn`).
  * The first attempt times out after 1000 + 1000·attempts ms, capped at 100 s.
  * On failure it loops: sleep 500 ms, retry.
  * After a close it sleeps 500 ms and reconnects, **forever**, unless the close code is `4000` or the client closed the socket itself.
  * Close codes: 4000 closedByUser, 4001 lostConnection, 4002 closedByConnectionManager, 4003 reconnectAfterFailure, 4100 unknown [obs L6181].
* **On open** (`clientConnected`, L6730).
  * Start the heartbeat.
  * Call `refreshAllPaths()`.
  * After a reconnect, the GUI re-fetches the subtree and rebuilds its store.
* **Heartbeat** (class `N`, L6318).
  * Sends `get /status?cmd_id=heartbeat&properties=0&children=0&message_id=…` immediately on open, then every **5000 ms**.
  * Any incoming message also counts as a sign of life.
  * Verification is off unless `verifyHeartbeat(true)` is called, and nothing calls it [obs], so a missing reply never closes the socket.
  * `start_heartbeat`/`stop_heartbeat` ("routerd") exist but have no callers.
* **Ready.**
  * `loadUANet` resolves only after the reply to `get_base_path_subtree` has been parsed (`parsingComplete`).
  * Then `notifyGUILoaded` runs (L17356). If a global `notify_gui_loaded()` exists it is called; otherwise the GUI sends `post <path>/commands/notify_gui_loaded`.
  * That post is retried every 1000 + 1000·n ms until a reply arrives. An error reply, or an absent command path, also counts as done.

Exact frames sent on open (encoder re-run with uuid `U`):

```
get /status?cmd_id=heartbeat&properties=0&children=0&message_id=U%3A0
get /devices/0/inputs/2/effects/3?flatvalue=1&commands=1&cmd_id=get_base_path_subtree&recursive=1&propfilter=normalized_value%2Cstring_value%2Creal_value%2Creal_values%2Cstring_values%2Cname%2Cnum_steps%2Cdiscrete%2Clevel%2Cdata%2Ctouch_count%2Coutput_stem_format%2Cpreset_name%2Cpreset_uid_dirty%2Cpreset_uid%2Cplugin_power%2Cstem_format%2Ctype%2Cvalue&message_id=U%3A1
subscribe /devices/0/inputs/2/effects/3?recursive=1&cmd_id=subscribe&message_id=U%3A2
subscribe /devices/0/inputs/2/effects/3/meters/vu/level/value?cmd_id=subscribe&message_id=U%3A3
```

Notes on these frames:
* Client 1.9.0 adds `private_data_type` to `propfilter`.
* The meter subscribe is sent once per meter node.
* Right after these frames come `post /logs/send_log …` frames (§5).

## 4. Wire format

**Requests** (`W.encode`, L6410) [obs]:
* Form: `<action> <path>[?<params>][ <data>]`.
* `params` is encoded by `T()`: `encodeURIComponent(k)=encodeURIComponent(v)` joined with `&`.
* `data` is encoded by `q()`: numbers and bools with `toString()`, objects with `JSON.stringify`, and strings as `"` + s + `"` **without escaping**.
* Every request carries `message_id=<uuid>:<n>`.
* Verbs: `get set post subscribe unsubscribe`.

| Purpose | Example (exact shape) |
|---|---|
| Value set from a knob binding (`setWithReceipt`) | `set <P>/controls/attack/normalized_value/value?cmd_id=set&cmd_listener=%24PATH%3A<urlenc path>%24SEED%3A417230981552&message_id=U%3A5 0.42` |
| Value set from app code (`setRealValue`, `setNormalizedValue`) | `set <P>/controls/ratio/real_value/value?cmd_id=set&message_id=U%3A6 2` |
| Touch begin/end, which wraps every binding write | `post <P>/controls/attack/set_touch?message_id=U%3A4 {"client_uid":"U","touch":true}` |
| Reset to default (Ctrl- or Alt-click) | `post <P>/controls/attack/set_control_default_value?message_id=… {"client_uid":"U"}` |
| GUI loaded | `post <P>/commands/notify_gui_loaded?preupdates=1&cmd_id=post&cmd_listener=…&message_id=…` |
| Log | `post /logs/send_log?preupdates=1&cmd_id=post&cmd_listener=…&message_id=… {"category":"HBGF","severity":"info","message":"1176 Rev A Compressor: GUI State: …"}` |
| Display conversion (some GUIs) | `post <P>/controls/bass/get_string_for_normalized_value?preupdates=1&cmd_id=post&cmd_listener=… 0.5`. The reply's `data` is the string. |

**Replies and pushes** (`W.decode`, `clientMessage`, `handleRequestListener`, L6417, L6779, L6713) [obs]:
* Shape: `{"path":…, "data":…, "parameters":{…}, "error":…}`.
* `parameters.cmd_id` routes the reply:

| `cmd_id` | Handling |
|---|---|
| `get_base_path_subtree` | Builds the store. |
| `heartbeat` | Counts as alive. |
| `set`, `get` | Value goes to the store and the receipt resolves. |
| `subscribe`, `unsubscribe`, `post` | Receipt only. **Not stored.** |
| absent | Value update: `updateStore`, then listeners. |

* **Receipts.** A request made with `cmd_listener` resolves only when a reply echoes `parameters.cmd_listener` **and** has `path` equal to the request path; otherwise it rejects with `MISMATCHED_UANET_REQUEST`.
  * A reply with `error` → `UANET_MESSAGE_ERROR`.
  * A reply without `data` → `INVALID_UANET_MESSAGE`.
* **Self-echo filter.** Messages whose `message_id` starts with the client's uuid are dropped unless `cmd_id` is one of heartbeat, get_base_path_subtree, get, set, post, subscribe or unsubscribe.
* **Pushes** must look like `{"path":"<P>/controls/attack/normalized_value/value","data":0.5}`. If a push carries a `cmd_id` such as `subscribe`, it is ignored.
* **Optional batch push.** `{"path":"multi","data":{"data":[…],"path":[nodeIdx…],"name":[strIdx…],"parent":[nodeIdx…],"string":[…],"added"?:[],"removed"?:[]}}`.
  * A path is rebuilt as `"/"+string[name[i]]`, walking `parent` until `parent[i]==i` (function `$`, L6368).
  * The host does not need to send this format.

Example reply to the subtree request. The structure is [obs] from the parser `J.parse` (L6973); the values are illustrative:

```json
{"path":"/devices/0/inputs/2/effects/3",
 "parameters":{"flatvalue":"1","commands":"1","cmd_id":"get_base_path_subtree","recursive":"1","message_id":"U:1"},
 "data":{"properties":{"type":"effect","preset_name":"Default","preset_uid":"","preset_uid_dirty":false,
                       "plugin_power":1,"stem_format":"mono","output_stem_format":"mono"},
  "children":[
   {"path":"commands","properties":{},"commands":["notify_gui_loaded","open_link","set_preset"]},
   {"path":"controls","properties":{},"children":[
     {"path":"attack","properties":{"type":"effect_control","name":"Attack","normalized_value":0.5,"real_value":0.5,
       "string_value":"4","num_steps":0,"discrete":false,"real_values":[],"string_values":[],"touch_count":0},
      "commands":["set_touch","set_control_default_value","get_string_for_normalized_value"]},
     {"path":"ratio","properties":{"type":"effect_control","name":"Ratio","normalized_value":0.1,"real_value":1,
       "string_value":"4:1","num_steps":11,"discrete":true,"real_values":[0,1,2,3,4,5,6,7,8,9,10],
       "string_values":["None","4:1","8:1","…"],"touch_count":0},"commands":["set_touch","set_control_default_value"]}]},
   {"path":"meters","properties":{},"children":[{"path":"vu","properties":{"type":"meter","level":[0.0]}}]}]}}
```

Parser rules [obs]:
* A node without a `properties` object is skipped together with its subtree.
* `children` is an **array** of nodes named by `path`. The engine's own replies use an object keyed by name (PROTOCOL.md §3), so they do not fit.
* Properties are **flat** values (`flatvalue=1`). They are stored at `<node>/<prop>/value`.
* `commands` entries are stored at `<node>/<cmd>`.
* A node's role comes from `properties.type`:
  * `effect_control` (or `control` when port == `"4740"`, "mixerd"), `meter`, `private_data`, `effect` or `folder`.
  * Descendants of a typed node are recorded too.
* The control id is the last path segment.
* `set_touch` and `set_control_default_value` work only when the control's parent segment is literally `controls` (`getSelectorID`, L7267).
* The effect's commands must sit at `<effect>/commands/<cmd>`. Its properties are read from `<effect>/<prop>/value`.
* The meter value is `<meter>/level/value`, an **array**; the GUI uses `[0]`.

## 5. What the GUI reads and writes

* **Selectors and properties** (`AudioPlugin`, L7280):

| Selector | Properties |
|---|---|
| `controls` | normalized_value, real_value, string_value, name, num_steps, discrete, real_values, string_values, touch_count |
| `meters` | level |
| `private_data` | data |
| `effect` | preset_name, preset_uid, preset_uid_dirty, plugin_power, stem_format, output_stem_format |

* **Effect commands.** 1.7.0 supports `send_midi set_effect_controls untouch_controls notify_gui_loaded open_link open_link_{info,buy,doc,video}`. 1.9.0 adds `preset_browser_{toggle,increment,decrement}` and `set_preset` (G:L7718). The payloads of `set_effect_controls`, `untouch_controls` and `set_preset` are [unk].
* **Values.**
  * Knobs (`ua-knob`) bind `normalized_value`.
  * Stepped controls use `real_value` as the **step index**. 1176 examples:
    * ratio 0…10, per `getRatioFrameIndex`.
    * headroom 0…6, from `setRealValue(v±1)` with bounds 0…6.
    * `meter` 0…3, `sc_filter` 0/1, `power` 0/1.
  * Dropdowns read the `real_values` and `string_values` arrays.
* **Startup writes.** The GUI writes nothing at startup. The binding updater re-enters `guard()`, which suppresses echo writes (L17589) [obs]. Writes happen only on interaction: mouse, wheel, Ctrl/Alt-click reset, button zones. Log and notify posts start right after connecting.
* **Meters.**
  * Subscribed per meter path (§3). The GUI never polls.
  * The rate is whatever the host pushes [obs]. Engine meters are polled by the host; ConsoleLink polls at 60 Hz [obs PROTOCOL.md §5].
  * 1176 `vu` is `level[0]` and is used directly as the needle's normalized frame position.
* **Presets.**
  * The GUI never loads `algo.bundle/…/presets/*.json`; there is no fetch of them [obs].
  * Presets are host-side: `lunaPresetBrowser*` functions or `preset_browser_*` commands, plus `preset_name`/`preset_uid` for display.
  * Preset files are `{plugin_id,uid,name,tags,chunk:<base64>}`, where `chunk` is an opaque LUNA state blob. How it maps to UAD-2 is [unk].
* **Sizing.**
  * The size is fixed by the GUI's `manifest.json` `window` (1176: 950×175; the loading screen uses the same size, L17475).
  * There are no resize messages [obs].
  * 2x art is chosen by `devicePixelRatio` through `image-set`.
  * Console also has a "GUI scale factor" (Mac strings), so zoom is a host concern [inf].
* **Offline mode.** Without `path`, the GUI renders but does not connect and does not call notify. This is useful as a first smoke test [obs].

## 6. Host side: what Console does

| Item | Finding |
|---|---|
| URL | (1.3.1 on macOS: §8.) `Rack_PluginWebViewHelper::GetSelectedInsertURL` (0x1809ed3b0) builds `<guiBase>` + (`/uaserver.html` if source mode == 8, else `/index.html`) + `?host=` + client host + `&port=` + client port + `&path=` + insert path with trailing `/` trimmed + optional `&plugin_id=<id>` [obs strings 0x183879a10…a48; order from the pseudo-C]. Host and port are those of the `MxClnt_Client` that owns the insert, so **Console points the GUI straight at the server for that client and does not proxy it** [inf]. |
| GUI base | An unnamed helper switches on source mode: 1 → `http://<client host>/gui/plugins/…` + `.uad/html`; 2 → `http://js.uaudio.com/…`; 3 → `http://127.0.0.1…`; 4/5/8 → `file://` + the installed component path. The default is 5 unless a feature flag is set [obs strings; inf meaning]. |
| Insert path | `MxClnt_Paths` vfunc +0x98. The UAD2 variant (0x181a72770) looks up a stored slot path per channel and insert, falling back to a stringstream path built from `device` and `effect`. The HB variant is 0x181a7ef40. On UAD2 this is most likely `/devices/D/inputs/N/effects/K` [inf]. |
| 4718 | `default_deviced_port` is a UAFeatureKey_Int at 0x184c33f70, value 0x126e, initialised at 0x180891be8. `ClientTypeToPort` (0x1816cb4c0) gives it to client types 0/1 (HB, `_uadevice._tcp.`); type 5 (UAD2) gets 4710 and types 2/3/4 get 4720 [obs]. |
| Who can serve the tree | Mac **UA Mixer Helper** (inside UAD Console.app, listening on 4720 and 4748) contains the whole GUI vocabulary: `effect_control set_touch set_control_default_value real_values string_values touch_count plugin_power untouch_controls set_effect_controls send_midi /plugins manifest` [obs strings]. Mac **UA Mixer Engine** has only `normalized_value`, `/normalized_value/value`, `NormalizedValue`, `VendorSpecific`, `deviced`, `HTTP/1.1 101 Switching Protocols` and `Sec-WebSocket-Key` [obs]. So the engine can upgrade to WebSocket, but probably cannot serve this tree [inf]. The Wine `UAMixerEngine.exe` is a 149 KB stub with no UA strings. |
| Injected functions | `Rack_PluginWebView` ctor (0x1809e95e0) registers `lunaShowInfoPage`, `lunaOpenInExternalBrowser`, `lunaDownloadPlugin`, `notify_gui_loaded`, `lunaPresetBrowserIncrement`, `lunaPresetBrowserDecrement` and `lunaPresetBrowserToggle`. Each handler receives (std::string, rapidjson value). `UAPW_WebViewImpl` (0x1816bce60) injects `function NAME(x){ var msg_json = {}; msg_json["message"] = "NAME"; msg_json["payload"] = x; console.log(msg_json); window.chrome.webview.postMessage(msg_json); }`. On the Mac the same functions call `window.webkit.messageHandlers.NAME.postMessage(z)` [obs]. The GUI calls these only as fallbacks when the UANet post fails (`tryUANetOrInjectedFn`, G:L7729), except `notify_gui_loaded`, which it prefers when present. |
| Show timing | `ShouldWaitForGUILoaded` (0x1809eb610) reads the GUI manifest's `client_interface_support_version` and compares it with `"1.3.2"`. Console keeps the view hidden until `notify_gui_loaded` arrives [inf]. |
| `host://` | Console maps `host://` back to `http://` (0x1809ee390). The GUI has a helper that rewrites links to `host://` (L17655) for external links [inf]. |

## 7. Minimal Rust host design

1. **Load the page.** As Console 1.3.1 does: `ua-content://gui/index.html?host=127.0.0.1&port=P&path=<slot path>&plugin_id=<id>`, with a `ua-content` handler rooted at the component's html directory (§8; this is what console-rs does, and `fetch("hbgf_dev_features.json")` works). The 1.2.x `file://` form also works. Keep `host` first. Size the view from `manifest.window`.
2. **Run a bridge.** Bind 127.0.0.1 on an ephemeral port, accept WebSockets from any Origin (a `file://` page sends `Origin: null`, a `ua-content` page `Origin: ua-content://gui`), and treat every text frame as one UANet request.
3. **Answer locally:**
   * `get /status…heartbeat`: reply `{"path":"/status","data":{},"parameters":<echo>}`.
   * `post /logs/send_log`: ack, and optionally forward to the tracing log.
   * `post …/commands/notify_gui_loaded`: ack, then show the window.
   * `post …/set_touch`: ack and keep the state locally; the engine has no touch concept [inf].
   * `get_base_path_subtree`: synthesise the §4 tree from engine data (`EffectName`, `parameters/k/{Name,NormalizedValue,StringValue,StepValue}`) and a per-plugin **id↔ordinal map**.
4. **Translate:**
   * `set <P>/controls/<id>/normalized_value/value v` → engine `set <slot>/parameters/<k>/NormalizedValue/value v`.
   * `real_value` step `s` → normalized `s/(n-1)`. This is [inf]; it matches the spec defaults: ratio 0.1 = 1/10, headroom 0.5 = 3/6, meter 1.0 = 3/3.
   * `set_control_default_value` → the spec `default`.
   * Engine pushes of NormalizedValue/StringValue → GUI pushes of `normalized_value`, `real_value` and `string_value`, **without** `cmd_id`.
   * Meters: poll `get <slot>/VendorSpecific/value?floats=<id>` (1176 `vu` = float 162), apply the spec `calibration` and `smoothing`, then push `{"path":"<P>/meters/vu/level/value","data":[x]}`.
5. **Build the id map.** Scrape ids from the bundle: `ua-param` values and `registerCallbackListener({id})`. Join them to ConsoleLink spec elements: element `id` (e.g. `input-knob`) → GUI state `ua-param` → spec `param` index. 1176 [obs from the GUI plus the spec]: input 0, output 1, attack 2, release 3, ratio 4, meter 5, sc_filter 6, mix 7, headroom 8, power 9, meter `vu`. The ids are **not** a slug of the engine names: SSL G uses `thresh`, `make_up`, `fade_rate`, `reset_autofade`, `fade_is_flashing` [obs].

### Rust host checklist

- [ ] Put `host`/`ip` first in the query, never `path`. Use `transport=ws`, or leave it out.
- [ ] Send one JSON object per WebSocket text frame. Strip the engine's `\0` framing. Accept `Origin: null`.
- [ ] Parse requests as `verb SP path ['?' query] [SP value]`. The value is JSON, or a bare `"string"` with **no escaping**. Decode the query with `%`-decoding.
- [ ] Echo the request's `parameters` (`cmd_id`, `cmd_listener`, `message_id`) in every reply, and use the **exact** request `path`.
- [ ] Pushes carry no `cmd_id`. Use flat values, at `<node>/<prop>/value`.
- [ ] Subtree rules: `children` is an array, every node has `properties`, controls sit under `…/controls/<id>`, effect commands under `<P>/commands/`, meter `level` is an array.
- [ ] Reply to the heartbeat every 5 s. Close with code **4000** to stop the GUI's endless reconnect, or tear the webview down first.
- [ ] Answer `/logs/send_log`, `notify_gui_loaded`, `set_touch` and `get_*_for_normalized_value` locally. Never forward unknown `post`s to the engine.
- [ ] **Write gate.** While writes are disabled, reply to `set` and `set_control_default_value` with `{"path":p,"error":"writes disabled","parameters":<echo>}`, then push the engine's current value back so the knob snaps back. The GUI has already updated itself optimistically (`isShortCircuit`). Never translate a GUI write unless the app's engine write gate is open.
- [ ] Inject `window.notify_gui_loaded` and `lunaOpenInExternalBrowser` (and optionally the `lunaPresetBrowser*` functions) through `with_initialization_script`, bridged to `window.ipc.postMessage(JSON.stringify({message,payload}))`.
- [ ] Do not use `transport=ble`: on a `file://` page `window.origin` is `"null"`. Console 1.3.1's own scheme, `ua-content://gui/…` (§8), does reach `ws://127.0.0.1` from WebKitGTK through wry: tested with the 1176 Rev A (origin `ua-content://gui`, GUI loaded, 0.000% pixels off the reference).

## 8. `ua-content://`: how Console 1.3.1 loads the page (macOS)

Sources: Mac `Rack` 1.3.1 x86_64 (thinned slice), REA evidence `ev_c165e9a8…` (0x1df630),
`ev_69f64119…` (0x1a44280), `ev_7afd3171…` (`setURLSchemeHandler:forURLScheme:` in 0x1a43360), and
`llvm-objdump --macho --objc-meta-data` plus disassembly of the `UAPWSchemeHandler` class.
Implemented in [`content`](src/content.rs) and used by [`webview`](src/webview.rs).

| Item | Finding |
|---|---|
| URL | Same builder as 1.2.9 (§6). In source mode 5, when the GUI base starts with `file://`, the folder (the URL minus `file://`) goes to an out-parameter and the base becomes `"ua-content"` + `"://gui"`. The page is `ua-content://gui/index.html?host=<h>&port=<p>&path=<insert path, one trailing / dropped>&plugin_id=<id>` [obs]. `plugin_id` is appended because the out-parameter is set; the 1.2.x `file://` URL has the first three keys. Values are appended unencoded (engine paths need no escaping). |
| Page view | `location.href` is that URL and `location.origin` is `ua-content://gui` (checked in WebKitGTK). `getURLParams` glues `host` to the href and drops it, but finds `port`, `path` and `plugin_id`; `doConnect` reads `host` from `location.search` (§1). `GuiQuery::parse` reproduces both, and the tests check Console's URL survives them. The 1.7.0 and 1.9.0 clients never read `plugin_id`. |
| Handler | `UAPWSchemeHandler` (ivars `mRootPath`, `mAllowedRoots`, `mActiveTasks`; methods `setRootPath:`, `setAllowedRoots:`, `resolvePathForURL:` 0x1a3e620, `removeTaskIfActive:`, `webView:startURLSchemeTask:` 0x1a3ea10, `webView:stopURLSchemeTask:` 0x1a3f100). The root is set per web view at 0x1a442e8 (from the out-parameter); allowed roots at 0x1a4440c. |
| Resolution | `NSURLComponents.path` (percent-decoded; host `gui` ignored) → `stringByStandardizingPath` → appended to the root → standardized again; served only if it equals the root or starts with `root + "/"`, and the root lies under an allowed root [obs]. |
| Reply | File read on a global queue (`dataWithContentsOfFile:`), delivered on the main queue if the task is still active: `NSHTTPURLResponse` status **200**, `HTTP/1.1`, headers `Content-Type` (lowercased extension → html/htm `text/html`, js/mjs `text/javascript`, css `text/css`, json/map `application/json`, png, jpg/jpeg, gif, svg `image/svg+xml`, webp, ico `image/x-icon`, woff, woff2, ttf, otf, wasm, mp4, webm, mp3 `audio/mpeg`, wav, txt, xml `application/xml`, pdf; otherwise `application/octet-stream`), `Content-Length`, `Access-Control-Allow-Origin: *`, `Cache-Control: no-cache` [obs]. |
| Errors | No root or a path outside it: "No content root or invalid path"; unreadable file: "Failed to read file". Both fail the task with `NSURLErrorDomain` −1100 [obs]. A wry handler must return a response, so ours answers 404. |
| Windows | 1.2.9 keeps `file://` with a WebView2 `WebResourceRequested` filter. Whether Windows 1.3.x moved to `ua-content` is [unk] (`unk_f0ad4779…`). |

console-rs serves the scheme in wry with one `ContentRoots` per web view (`for_component`: root
= the component's `html` folder, allowed root = its `.lunacomponent`). It also accepts
`ua-content://gui/<component id>/…` (`for_library`, one handler for a whole library) and serves
`index.html` for an empty path; Console does neither. `GuiSource::File` keeps the 1.2.x
`file://` page. Apple hosts register the same handler with `WKURLSchemeHandler`
(`console-rs/apple/README.md`).

## 9. Unknowns

- [unk] Whether UA Mixer Engine 4710 accepts a WebSocket upgrade and answers `flatvalue=1` gets with array `children`. Do not rely on it: the control vocabulary is missing from the engine.
- [unk] Which helper port (4720 or 4748 on the Mac) and which path layout serve native (UADx) inserts in UAD Console, and the exact `path` value Console passes for UAD2 inserts. To settle this, decompile the callers of 0x1809ed3b0 (0x1809ee270, 0x180df91f0, 0x180dff800) and `MxClnt_HBPaths` 0x181a7ef40.
- [unk] What GUI source mode 8 (`uaserver.html`) is for, and the condition at 0x184c27080.
- [unk] Per-plugin id maps for the 65 other GUIs. Also whether the LUNA and UAD-2 enumerations agree: 1176 ratio steps 8–10 are three or four buttons in the GUI (`[12,8,4]`, `[20,12,8]`, all four), but the spec labels them "4:1+12:1", "8:1+20:1", "4:1+20:1". Check against the engine `StringValue`.
- [unk] Payloads and replies for `set_effect_controls`, `untouch_controls`, `set_preset` and `private_data`, and what the 1176 attack "off" (normalized 0) means on UAD-2.
- [unk] Exact meter push rate and scaling in LUNA. The VendorSpecific calibration is from ConsoleLink, not from UA.
