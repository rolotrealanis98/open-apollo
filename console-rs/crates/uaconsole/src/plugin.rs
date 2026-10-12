//! Plug-in windows from the plugin-ui crate as overlay panels.
//!
//! Tapping a loaded insert (or Unison) slot opens UAD Console's plug-in window
//! ([`plugin_ui::PluginWindow`]): the title bar, the UAPW toolbar and the plug-in's panel (its
//! ConsoleLink spec, else the generic parameter list). With an engine model the panel reads and
//! writes the slot's `parameters/<k>` through the engine store (writes obey the store's gate,
//! read-only by default); with preview data it edits local values.
//!
//! UA's HTML GUIs (lunacomponents) need the `webview` feature. With it, a plug-in that has one
//! opens UA's GUI in a WebKitGTK top-level window next to our chrome, served by a per-window
//! UANet bridge on 127.0.0.1 that answers from the engine slot (WEBBRIDGE.md). wgpu and GTK
//! surfaces cannot share one window portably, and Wayland clients cannot position their own
//! top-level windows, so the GUI window is placed by the compositor; the chrome's editor area
//! says where the GUI is. Without the feature, those plug-ins show their spec panel.

use std::sync::Arc;

use engine_model::EngineMixer;
use houston::{ImageCache, Rect};
use plugin_ui::{Content, EngineSlot, LunaLibrary, MockParams, ParamSnapshot, PluginSpecLibrary, PluginWindow, WindowEvent};
use rack::model::{InsertState, StripInfo};
use rack::{Action, Control, ControlId, Hit, Interaction, MixerModel, PluginWindowButton as B, Ui};
use ua_engine::InsertPaths;

use crate::overlay::{Panel, PanelResponse, PluginHook, PointerEvent, UNISON_SLOT};

pub struct PluginUiHook {
    specs: Option<Arc<PluginSpecLibrary>>,
    luna: Arc<LunaLibrary>,
    /// Host UA's HTML GUIs when one matches (needs the webview; off until the app embeds it).
    pub prefer_web: bool,
}

impl PluginUiHook {
    /// Finds the spec folder (and registers it with `images`, which gfx draws through) and the
    /// lunacomponents.
    pub fn new(images: &ImageCache) -> Self {
        let specs = PluginSpecLibrary::locate().map(Arc::new);
        match &specs {
            Some(lib) => {
                lib.attach(images);
                log_line(&format!("plug-in specs: {} in {}", lib.entries.len(), lib.folder.display()));
            }
            None => log_line("plug-in specs: none found (generic parameter lists only)"),
        }
        Self { specs, luna: Arc::new(LunaLibrary::locate()), prefer_web: cfg!(feature = "webview") }
    }
}

fn log_line(s: &str) {
    eprintln!("uaconsole: {s}");
}

fn engine(model: &dyn MixerModel) -> Option<&EngineMixer> {
    model.as_any()?.downcast_ref()
}

fn engine_mut(model: &mut dyn MixerModel) -> Option<&mut EngineMixer> {
    model.as_any_mut()?.downcast_mut()
}

/// A strip by id; `inN` / `auxN` (the preview's ids) also name the model's Nth input / aux.
fn resolve_strip(model: &dyn MixerModel, id: &str) -> Option<StripInfo> {
    let inputs = model.inputs();
    let auxes = model.auxes();
    if let Some(s) = inputs.iter().chain(&auxes).find(|s| &*s.id == id) {
        return Some(s.clone());
    }
    let nth = |list: &[StripInfo], rest: &str| rest.parse::<usize>().ok().and_then(|n| list.get(n).cloned());
    match (id.strip_prefix("aux"), id.strip_prefix("in")) {
        (Some(n), _) => nth(&auxes, n),
        (_, Some(n)) => nth(&inputs, n),
        _ => None,
    }
}

impl PluginHook for PluginUiHook {
    fn on_open_plugin(&mut self, strip_id: &str, slot: u8, model: &dyn MixerModel) -> Option<Box<dyn Panel>> {
        let strip = resolve_strip(model, strip_id)?;
        let state = if slot == UNISON_SLOT { model.unison(&strip) } else { model.inserts(&strip).get(slot as usize).cloned()? };
        // An empty slot opens Console's plug-in browser, not a window.
        let InsertState::Loaded { name, power } = state else { return None };
        let channel = model.main(&strip).name;
        let channel = if channel.is_empty() { strip.title.clone() } else { channel };
        let which = if slot == UNISON_SLOT { "Unison".to_string() } else { format!("Insert {}", slot as u32 + 1) };
        let paths = engine(model).and_then(|e| e.insert_slot(&ControlId { strip: strip.id.clone(), control: Control::Insert(slot) }));
        let slot_path = paths.as_ref().map(|p| p.base.clone()).unwrap_or_else(|| format!("{}/effects/{slot}", strip.id));
        let mut window = PluginWindow::open(slot_path, &name, &format!("{channel}, {which}"), self.specs.as_deref(), Some(&self.luna), self.prefer_web);
        window.chrome.power = power;
        if !self.prefer_web {
            if let Some(gui) = self.luna.component_for_effect(&name) {
                log_line(&format!("{name}: UA's web GUI ({}) is available; showing the {} panel", gui.id, window.content.kind()));
            }
        }
        let offline = match (&paths, &window.content) {
            (Some(_), _) => None,
            (None, Content::Spec(p)) => Some(MockParams::from_spec(&p.spec)),
            (None, _) => Some(MockParams::default()),
        };
        let snapshot = match (&offline, engine(model), &paths) {
            (Some(m), ..) => ParamSnapshot::capture(m),
            (None, Some(e), Some(p)) => ParamSnapshot::capture(&EngineSlot::peek(e.store(), p.clone()).view(e.store())),
            _ => ParamSnapshot::default(),
        };
        Some(Box::new(WindowPanel {
            window,
            pending: paths,
            engine: None,
            offline,
            snapshot,
            hits: Vec::new(),
            interaction: Interaction::default(),
            origin: (0.0, 0.0),
            specs: self.specs.clone(),
            #[cfg(feature = "webview")]
            web: None,
        }))
    }
}

/// One open plug-in window.
pub struct WindowPanel {
    window: PluginWindow,
    /// The engine slot, opened (refresh + subscriptions) on the first sync.
    pending: Option<InsertPaths>,
    engine: Option<EngineSlot>,
    /// Local values when there is no engine (preview data).
    offline: Option<MockParams>,
    /// What the last sync read, for drawing.
    snapshot: ParamSnapshot,
    hits: Vec<Hit>,
    interaction: Interaction,
    origin: (f32, f32),
    #[cfg_attr(not(feature = "webview"), allow(dead_code))]
    specs: Option<Arc<PluginSpecLibrary>>,
    /// UA's HTML GUI, its window and bridge (web content only).
    #[cfg(feature = "webview")]
    web: Option<web::WebHost>,
}

impl WindowPanel {
    fn refresh(&mut self, model: &mut dyn MixerModel) {
        if let Some(e) = engine_mut(model) {
            let store = e.store_mut();
            if self.engine.is_none() {
                if let Some(paths) = self.pending.take() {
                    self.engine = Some(EngineSlot::open(store, paths));
                }
            }
            if let Some(slot) = &mut self.engine {
                slot.sync(store);
                self.snapshot = ParamSnapshot::capture(&slot.view(store));
                if let Some(p) = slot.power(store) {
                    self.window.chrome.power = p;
                }
                self.window.chrome.preset = slot.preset(store).unwrap_or_else(|| "Default".into());
            }
        } else if let Some(m) = &self.offline {
            self.snapshot = ParamSnapshot::capture(m);
        }
        #[cfg(feature = "webview")]
        self.serve_web(model);
    }

    /// Opens UA's GUI on first use, then services GTK and the bridge (called at 30 Hz: the
    /// panel is live while a GUI is open).
    #[cfg(feature = "webview")]
    fn serve_web(&mut self, model: &mut dyn MixerModel) {
        let Content::Web(component) = &self.window.content else { return };
        if self.web.is_none() {
            match web::WebHost::open(component, &self.window, &self.snapshot, self.specs.as_deref()) {
                Ok(host) => self.web = Some(host),
                Err(e) => {
                    log_line(&format!("{}: web GUI: {e}", self.window.effect_name));
                    return;
                }
            }
        }
        let Some(host) = &mut self.web else { return };
        match (&self.engine, engine_mut(model)) {
            (Some(slot), Some(e)) => {
                let writes = e.store().writer().writes_enabled;
                host.service(&mut slot.view_mut(e.store_mut()), writes);
            }
            _ => match &mut self.offline {
                Some(m) => host.service(m, true),
                None => host.service(&mut self.snapshot, false),
            },
        }
    }

    fn apply(&mut self, action: &Action, model: &mut dyn MixerModel) -> Option<WindowEvent> {
        if let (Some(slot), Some(e)) = (&self.engine, engine_mut(model)) {
            let mut params = slot.view_mut(e.store_mut());
            return self.window.apply(action, &mut params);
        }
        match &mut self.offline {
            Some(m) => self.window.apply(action, m),
            None => self.window.apply(action, &mut self.snapshot),
        }
    }

    fn button(&mut self, b: B, model: &mut dyn MixerModel) {
        match b {
            B::Power => {
                let on = !self.window.chrome.power;
                match (&self.engine, engine_mut(model)) {
                    (Some(slot), Some(e)) => slot.set_power(e.store_mut(), on),
                    _ => self.window.chrome.power = on,
                }
            }
            other => log_line(&format!("plug-in window: {other:?} is not implemented yet")),
        }
    }

    fn close(&mut self, model: &mut dyn MixerModel) {
        if let (Some(slot), Some(e)) = (&mut self.engine, engine_mut(model)) {
            slot.close(e.store_mut());
        }
        #[cfg(feature = "webview")]
        if let Some(host) = self.web.take() {
            host.close();
        }
    }
}

impl Panel for WindowPanel {
    fn size(&self) -> (f32, f32) {
        self.window.size(&self.snapshot)
    }

    fn build(&mut self, ui: &mut Ui, rect: Rect) {
        self.origin = (rect.x, rect.y);
        let first = ui.hits.len();
        self.window.build(ui, self.origin, &self.snapshot);
        // The stack drops panel hits from the frame; keep ours for `pointer`.
        self.hits = ui.hits[first..].to_vec();
    }

    fn pointer(&mut self, ev: PointerEvent, model: &mut dyn MixerModel) -> PanelResponse {
        let (dx, dy) = self.origin;
        let actions = match ev {
            PointerEvent::Down { x, y, double } => self.interaction.pointer_down(&self.hits, x + dx, y + dy, double),
            PointerEvent::Move { x, y, fine } => self.interaction.pointer_move(x + dx, y + dy, fine),
            PointerEvent::Up { .. } => self.interaction.pointer_up(),
            PointerEvent::Wheel { .. } => return PanelResponse::Ignored,
        };
        if actions.is_empty() {
            return if matches!(ev, PointerEvent::Down { .. }) { PanelResponse::Handled } else { PanelResponse::Ignored };
        }
        let mut close = false;
        for a in &actions {
            match self.apply(a, model) {
                Some(WindowEvent::Close) => close = true,
                Some(WindowEvent::Button(b)) => self.button(b, model),
                _ => {}
            }
        }
        self.refresh(model);
        if close {
            self.close(model);
            return PanelResponse::Close;
        }
        PanelResponse::Handled
    }

    fn sync(&mut self, model: &mut dyn MixerModel) {
        self.refresh(model);
    }

    /// Console's plug-in windows are independent windows: the mixer stays usable.
    fn modal(&self) -> bool {
        false
    }

    /// A web GUI needs GTK and its bridge serviced every tick.
    fn live(&self) -> bool {
        matches!(self.window.content, Content::Web(_))
    }
}

#[cfg(feature = "webview")]
mod web {
    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    use plugin_ui::bridge::{BridgeEvent, BridgeServer};
    use plugin_ui::uanet::{ControlMap, UaNetBridge, scrape_control_ids};
    use plugin_ui::webview::{self, WebPanelWindow};
    use plugin_ui::{GuiQuery, LunaComponent, ParamSource, PluginSpecLibrary, PluginWindow};

    /// UA's GUI in a GTK top-level window and the bridge it talks to.
    pub struct WebHost {
        window: WebPanelWindow,
        server: BridgeServer,
        uanet: UaNetBridge,
    }

    fn gtk_ready() -> Result<(), String> {
        static INIT: OnceLock<Result<(), String>> = OnceLock::new();
        INIT.get_or_init(webview::init).clone()
    }

    impl WebHost {
        pub fn open(component: &LunaComponent, window: &PluginWindow, params: &dyn ParamSource, specs: Option<&PluginSpecLibrary>) -> Result<Self, String> {
            gtk_ready()?;
            let server = BridgeServer::bind(0).map_err(|e| e.to_string())?;
            let bundle = std::fs::read_to_string(component.html_dir.join("bundle.js")).unwrap_or_default();
            let spec = specs.and_then(|l| l.spec_for_effect(&window.effect_name));
            let meters = spec.as_ref().and_then(|s| s.meter_sources.as_ref()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
            let map = ControlMap::build(&scrape_control_ids(&bundle), params, &BTreeMap::new(), meters);
            // Console 1.3.1's keys (host, port, path, plugin_id); the bridge answers that path.
            let query = GuiQuery::console("127.0.0.1", server.port(), &window.slot, &component.id);
            let mut uanet = UaNetBridge::for_query(&query, map);
            if let Some(s) = &spec {
                uanet.defaults = s.parameters.iter().map(|p| (p.index, p.default_value.unwrap_or(0.0))).collect();
            }
            uanet.effect.preset_name = window.chrome.preset.clone();
            uanet.effect.power = window.chrome.power;
            let gui = WebPanelWindow::open(component, &query, &window.chrome.title, false)?;
            Ok(Self { window: gui, server, uanet })
        }

        /// Pumps GTK, answers the GUI's frames and pushes value changes.
        pub fn service(&mut self, params: &mut dyn ParamSource, writes_enabled: bool) {
            webview::pump();
            self.uanet.writes_enabled = writes_enabled;
            for ev in self.server.poll() {
                if let BridgeEvent::Text(id, frame) = ev {
                    for reply in self.uanet.handle(&frame, params) {
                        self.server.send(id, &reply);
                    }
                }
            }
            for push in self.uanet.pushes(params) {
                self.server.broadcast(&push);
            }
        }

        pub fn close(self) {
            self.window.close();
            webview::pump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use houston::Resources;
    use rack::{Assets, PreviewMixer};

    fn assets() -> Assets {
        let r = Resources { root: PathBuf::from("/nonexistent"), console_version: None };
        Assets::from_parts(r.clone(), houston::Appearance::default(), Arc::new(ImageCache::new(r)))
    }

    fn fixture() -> EngineMixer {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json");
        EngineMixer::from_snapshot(path).expect("fixture")
    }

    #[test]
    fn opens_loaded_slots_only() {
        let assets = assets();
        let mut hook = PluginUiHook::new(&assets.images);
        let model = fixture();
        assert!(hook.on_open_plugin("in0", 0, &model).is_none(), "empty slot");
        assert!(hook.on_open_plugin("in0", 1, &model).is_some(), "Vitalizer on in0 insert 2");
        assert!(hook.on_open_plugin("/devices/0/inputs/1/effects/0", 0, &model).is_none(), "not a strip id");
        assert!(hook.on_open_plugin("/devices/0/inputs/1", 0, &model).is_some(), "Curve Bender");
        // Preview data: loaded slots open offline (local values), empty ones don't.
        let preview = PreviewMixer::default();
        assert!(hook.on_open_plugin("in0", 1, &preview).is_some(), "preview Vitalizer");
        assert!(hook.on_open_plugin("in0", 3, &preview).is_none(), "preview empty slot");
    }

    #[test]
    fn offline_panel_edits_local_values() {
        let assets = assets();
        let mut hook = PluginUiHook::new(&assets.images);
        let mut model = PreviewMixer::default();
        let mut panel = hook.on_open_plugin("in2", 1, &model).expect("preview dbx 160");
        panel.sync(&mut model);
        let (w, h) = panel.size();
        let mut ui = Ui::new(&assets);
        panel.build(&mut ui, Rect::new(0.0, 0.0, w, h));
        let power = ui.hits.iter().find(|h| h.id.control == Control::PluginWindow(B::Power)).expect("power button").rect;
        let (x, y) = power.center();
        assert_eq!(panel.pointer(PointerEvent::Down { x, y, double: false }, &mut model), PanelResponse::Handled);
    }

    #[test]
    fn panel_reads_engine_and_writes_through_the_gate() {
        let assets = assets();
        let mut hook = PluginUiHook::new(&assets.images);
        let mut model = fixture();
        let mut panel = hook.on_open_plugin("in0", 1, &model).unwrap();
        panel.sync(&mut model);
        let (w, h) = panel.size();
        let mut ui = Ui::new(&assets);
        panel.build(&mut ui, Rect::new(100.0, 50.0, w, h));
        // The title and the engine-backed parameter list (spec panel or generic list).
        let texts: Vec<String> = ui.list.items.iter().filter_map(|d| if let rack::Draw::Text { text, .. } = d { Some(text.clone()) } else { None }).collect();
        assert!(texts.iter().any(|t| t.ends_with(", Insert 2")), "{texts:?}");
        // Drag the first parameter drag target up by 100 pt: one gesture, local only (gate closed).
        let drag = ui.hits.iter().find(|h| matches!(h.id.control, Control::PluginParam(_))).cloned().expect("a parameter hit");
        let Control::PluginParam(k) = drag.id.control else { unreachable!() };
        let path = format!("{}/parameters/{k}/NormalizedValue", drag.id.strip);
        let before = model.store().value(&path).and_then(|v| v.as_f64()).unwrap();
        let (cx, cy) = drag.rect.center();
        for ev in [PointerEvent::Down { x: cx - 100.0, y: cy - 50.0, double: false }, PointerEvent::Move { x: cx - 100.0, y: cy - 150.0, fine: false }, PointerEvent::Up { x: cx - 100.0, y: cy - 150.0 }] {
            assert_ne!(panel.pointer(ev, &mut model), PanelResponse::Ignored);
        }
        let after = model.store().value(&path).and_then(|v| v.as_f64()).unwrap();
        assert!(after > before || after == 1.0, "{before} → {after}");
        assert_eq!(model.store().writer().sent_count(), 0, "read-only by default");
        // Close releases the parameter subscriptions.
        let close = ui.hits.iter().find(|h| h.id.control == Control::PluginWindow(B::Close)).unwrap().rect;
        let (x, y) = close.center();
        assert!(model.store().retained_subscriptions().count() > 0);
        assert_eq!(panel.pointer(PointerEvent::Down { x: x - 100.0, y: y - 50.0, double: false }, &mut model), PanelResponse::Close);
        assert_eq!(model.store().retained_subscriptions().count(), 0);
    }
}
