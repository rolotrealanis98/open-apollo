//! `--drive FILE`: a headless console driven by a script, through the same hit, drag and menu
//! paths the window uses (`App::pointer_down/motion/up`), against any model source (an engine,
//! a snapshot or preview data). For end-to-end checks without a display: the harness in
//! `tools/drive/` runs scripts against `mixer-engine/tests/fake_hw_daemon.py` and reads what
//! the daemon wrote.
//!
//! One command per line (`#` comments). A control is named `STRIP:CONTROL`, the strip id and the
//! `Debug` form of `rack::Control` without spaces (`in0:Mute`, `in0:Preamp("48v")`,
//! `in0:Insert(0)`, `monitor:MonitorLevel`, `browser:BrowserRow(3)`); `hits` prints every name
//! on screen. `@X,Y` is a point in console points.
//!
//! MODS are modifier words held for that command, alone or joined with `+` (`alt`, `cmd+alt`):
//! `alt` = Option / Alt, `cmd` = Command / Ctrl, `shift` (or `fine`) = Shift, as the window
//! reads them (`rack::KeyMods`; Shift is the fine adjust).
//!
//! ```text
//! size W H            window size in points (default 1920 1050)
//! view SPEC           --view spec; open NAME: --open name
//! click T [double] [MODS]   press + release on T (`double` for a double-click)
//! rclick T            secondary click (context menus)
//! press T [MODS] / move T|@X,Y [MODS] / release   (press's MODS hold until the release)
//! drag T DX DY [N] [MODS]   press T, move by (DX, DY) in N steps (default 8), release
//! dragto T1 T2 [copy] press T1, move onto T2's centre, release (Console's drag and drop)
//! wheel T LINES [MODS]      scroll wheel over T (with `alt` it adjusts T; else it scrolls)
//! menu LABEL          click the row LABEL of the open menu (opens submenus on the way)
//! key TEXT|enter|escape|backspace   text to the focused field
//! command ID          a Console command id (`show_browser`, `undo`, …)
//! wait MS             pump the model for MS milliseconds
//! settle              pump until nothing changes for 300 ms (≤ 5 s)
//! shot PATH           render a PNG of the current frame
//! hits [PREFIX [N]]   print the names and rects of the hits on screen (the top plug-in
//!                     window's too); with N, fail unless exactly N distinct names start with PREFIX
//! layers              print each layer (separate window) id, kind and rect
//! layer PREFIX [larger|smaller|same]   print the first layer whose id starts with PREFIX;
//!                     with a word, fail unless its size compares so with the last `layer PREFIX`
//! nolayer PREFIX      fail if a layer's id starts with PREFIX (a window that should have closed)
//! text TEXT           fail unless TEXT is drawn (texts joined by spaces: wrapped lines match)
//! echo TEXT
//! ```
//!
//! Exit status: 0 when every line ran; 1 at the first failing line (printed with its number).
// ponytail: the pointer handlers mirror App's; fold App onto this when the window needs the
// same script path.

use std::path::Path;
use std::time::{Duration, Instant};

use houston::Rect;
use rack::menu::MenuCmd;
use rack::{Action, Assets, Control, ControlId, Hit, Interaction, KeyMods, MixerModel, ViewState};
use rack::surfaces::Effect;

use crate::frame::{Frame, build_frame, layout_for};
use crate::overlay::{OverlayStack, PointerEvent, Routed};

pub struct Driver {
    assets: Assets,
    model: Box<dyn MixerModel>,
    mixer: ViewState,
    overlays: OverlayStack,
    interaction: Interaction,
    frame: Frame,
    cursor: (f32, f32),
    deferred_tap: Option<Hit>,
    /// Modifiers held (a command's MODS words): Option/Alt and Command/Ctrl, and Shift (fine).
    keys: KeyMods,
    fine: bool,
    copy: bool,
    scale: f32,
    /// Sizes the `layer` command printed, by prefix.
    layer_sizes: std::collections::HashMap<String, (f32, f32)>,
}

pub fn name(id: &ControlId) -> String {
    let c = format!("{:?}", id.control);
    format!("{}:{}", id.strip, c.split_whitespace().collect::<String>())
}

/// A command's modifier words (`alt`, `cmd`, `shift`/`fine`, or joined: `cmd+alt`).
fn key_words(args: &[&str]) -> (KeyMods, bool) {
    let (mut k, mut shift) = (KeyMods::default(), false);
    for w in args.iter().flat_map(|a| a.split('+')) {
        match w {
            "alt" | "option" => k.alt = true,
            "cmd" | "ctrl" => k.cmd = true,
            "shift" | "fine" => shift = true,
            _ => {}
        }
    }
    (k, shift)
}

fn centre(r: Rect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

impl Driver {
    pub fn new(assets: Assets, model: Box<dyn MixerModel>, scale: f32) -> Self {
        let mut overlays = OverlayStack::new(Box::new(crate::plugin::PluginUiHook::new(&assets.images)));
        let mixer = ViewState { window: Some((1920.0, 1050.0)), menu_bar: true, ..ViewState::default() };
        let frame = build_frame(&assets, &*model, &mixer, &mut overlays, Rect::new(0.0, 0.0, 1.0, 1.0), None);
        let mut d = Self {
            assets,
            model,
            mixer,
            overlays,
            interaction: Interaction::default(),
            frame,
            cursor: (0.0, 0.0),
            deferred_tap: None,
            keys: KeyMods::default(),
            fine: false,
            copy: false,
            scale,
            layer_sizes: Default::default(),
        };
        d.rebuild();
        d
    }

    fn rebuild(&mut self) {
        let (w, h) = layout_for(&*self.model, &self.mixer).size();
        self.overlays.sync(&mut *self.model);
        self.frame = build_frame(&self.assets, &*self.model, &self.mixer, &mut self.overlays, Rect::new(0.0, 0.0, w, h), None);
    }

    fn pump(&mut self, for_: Duration) {
        let end = Instant::now() + for_;
        loop {
            self.model.pump();
            if Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.rebuild();
    }

    /// Pumps until a 300 ms window with no model change (≤ 5 s).
    fn settle(&mut self) {
        let start = Instant::now();
        let mut quiet = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if self.model.pump() {
                quiet = Instant::now();
            } else if quiet.elapsed() > Duration::from_millis(300) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.rebuild();
    }

    fn find(&self, target: &str) -> Result<(f32, f32), String> {
        if let Some(p) = target.strip_prefix('@') {
            let (x, y) = p.split_once(',').ok_or("point is @X,Y")?;
            return Ok((x.parse().map_err(|_| "bad x")?, y.parse().map_err(|_| "bad y")?));
        }
        // The top plug-in window's controls first (the overlay takes presses on it).
        self.overlays
            .top_hits()
            .iter()
            .rev()
            .chain(self.frame.hits.iter().rev())
            .find(|h| name(&h.id) == target)
            .map(|h| centre(h.rect))
            .ok_or_else(|| format!("no hit named {target:?} (try `hits`)"))
    }

    // MARK: pointer (as App does it)

    fn apply(&mut self, actions: Vec<Action>) {
        for a in actions {
            for effect in self.mixer.route(a, &*self.model) {
                match effect {
                    Effect::Model(Action::Tap(ControlId { strip, control: Control::Insert(slot) })) => {
                        self.overlays.on_open_plugin(&strip, slot, &*self.model);
                    }
                    Effect::Model(a) => self.model.apply(&a),
                    Effect::SetText(id, text) => self.model.set_text(&id, &text),
                }
            }
        }
        self.rebuild();
    }

    fn route(&mut self, ev: PointerEvent) -> bool {
        let routed = self.overlays.pointer(ev, &mut *self.model);
        let forwarded = self.overlays.take_mixer_actions();
        if !forwarded.is_empty() {
            self.apply(forwarded);
        }
        match routed {
            Routed::ToMixer => false,
            Routed::Consumed => true,
            Routed::Changed => {
                self.rebuild();
                true
            }
        }
    }

    fn down(&mut self, double: bool) {
        let (x, y) = self.cursor;
        if self.route(PointerEvent::Down { x, y, double }) {
            return;
        }
        let armed = self.mixer.drag_press(&self.frame.hits, x, y, &*self.model);
        self.interaction.controls_mode = rack::ControlsMode::of(&*self.model);
        self.interaction.set_default = self.mixer.modifier == Some(rack::surfaces::Modifier::SetDefault);
        self.interaction.mods = self.keys;
        let actions = self.interaction.pointer_down(&self.frame.hits, x, y, double);
        if armed {
            if let [Action::Tap(_)] = actions.as_slice() {
                self.deferred_tap = self.frame.hits.iter().rev().find(|h| h.rect.contains(x, y)).cloned();
                return;
            }
        }
        self.apply(actions);
    }

    fn motion(&mut self) {
        let (x, y) = self.cursor;
        if self.route(PointerEvent::Move { x, y, fine: self.fine }) {
            return;
        }
        if self.mixer.drag_pending.is_some() || self.mixer.dragging() {
            if self.mixer.drag_motion(&self.frame.hits, x, y, self.copy, &*self.model) {
                self.rebuild();
            }
        } else if self.interaction.is_dragging() {
            let actions = self.interaction.pointer_move(x, y, self.fine);
            self.apply(actions);
        } else if self.mixer.menu.is_some() && self.mixer.menu_hover(&self.frame.hits, x, y) {
            self.rebuild();
        }
    }

    fn up(&mut self) {
        let (x, y) = self.cursor;
        let tap = self.deferred_tap.take();
        if let Some(effects) = self.mixer.drag_release(&*self.model) {
            for e in effects {
                match e {
                    Effect::Model(a) => self.model.apply(&a),
                    Effect::SetText(id, text) => self.model.set_text(&id, &text),
                }
            }
            self.rebuild();
            return;
        }
        if let Some(hit) = tap.filter(|h| h.rect.contains(x, y)) {
            self.apply(vec![Action::Tap(hit.id)]);
            return;
        }
        self.route(PointerEvent::Up { x, y });
        let actions = self.interaction.pointer_up();
        self.apply(actions);
    }

    fn context_click(&mut self) {
        let (x, y) = self.cursor;
        if self.mixer.context_click(&self.frame.hits, x, y) {
            self.rebuild();
        }
    }

    fn move_to(&mut self, p: (f32, f32), steps: usize) {
        let from = self.cursor;
        for i in 1..=steps.max(1) {
            let t = i as f32 / steps.max(1) as f32;
            self.cursor = (from.0 + (p.0 - from.0) * t, from.1 + (p.1 - from.1) * t);
            self.motion();
        }
    }

    /// Clicks the open menu's row `label`, opening submenus that lead to it.
    fn menu_row(&mut self, label: &str) -> Result<(), String> {
        for _ in 0..4 {
            let rows: Vec<(ControlId, Rect, String)> = self
                .frame
                .hits
                .iter()
                .filter(|h| matches!(h.id.control, Control::Menu(MenuCmd::Row { .. }) | Control::Menu(MenuCmd::OpenSub { .. })))
                .map(|h| (h.id.clone(), h.rect, self.frame.list.text_in(h.rect).unwrap_or_default()))
                .collect();
            if rows.is_empty() {
                return Err("no menu is open".into());
            }
            if let Some((_, r, _)) = rows.iter().find(|(_, _, t)| t == label) {
                let c = centre(*r);
                self.move_to(c, 1);
                self.down(false);
                self.up();
                return Ok(());
            }
            // Hover the first unopened submenu and look again.
            let Some((_, r, _)) = rows.iter().find(|(id, _, _)| matches!(id.control, Control::Menu(MenuCmd::OpenSub { .. }))) else {
                return Err(format!("no menu row {label:?}; rows: {:?}", rows.iter().map(|r| &r.2).collect::<Vec<_>>()));
            };
            self.move_to(centre(*r), 1);
        }
        Err(format!("no menu row {label:?}"))
    }

    fn shot(&mut self, path: &Path) -> Result<(), String> {
        let gpu = gfx::Headless::new().ok_or("no GPU adapter")?;
        let mut renderer = gfx::Renderer::new(&gpu.device, &gpu.queue, gfx::wgpu::TextureFormat::Rgba8Unorm, self.assets.images.clone());
        gpu.render_png(&mut renderer, &self.frame.list, self.frame.layout.size(), self.scale, path).map_err(|e| e.to_string())
    }

    pub fn run_line(&mut self, line: &str) -> Result<(), String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((&cmd, args)) = words.split_first() else { return Ok(()) };
        let arg = |i: usize| args.get(i).copied().ok_or_else(|| format!("{cmd}: missing argument {}", i + 1));
        let num = |i: usize| -> Result<f32, String> { arg(i)?.parse().map_err(|_| format!("{cmd}: bad number {:?}", args[i])) };
        let (keys, shift) = key_words(args);
        match cmd {
            "#" => {}
            "size" => {
                self.mixer.window = Some((num(0)?, num(1)?));
                self.rebuild();
            }
            "view" => {
                self.mixer.apply_spec(arg(0)?)?;
                self.rebuild();
            }
            "open" => {
                if !self.mixer.open_named(arg(0)?) {
                    return Err(format!("open: unknown {:?}", arg(0)?));
                }
                self.rebuild();
            }
            "click" => {
                let p = self.find(arg(0)?)?;
                let double = args.contains(&"double");
                self.move_to(p, 1);
                (self.keys, self.fine) = (keys, shift);
                if double {
                    self.down(false);
                    self.up();
                }
                self.down(double);
                self.up();
                (self.keys, self.fine) = Default::default();
            }
            "rclick" => {
                let p = self.find(arg(0)?)?;
                self.move_to(p, 1);
                self.context_click();
            }
            "press" => {
                let p = self.find(arg(0)?)?;
                self.move_to(p, 1);
                (self.keys, self.fine) = (keys, shift);
                self.down(false);
            }
            "move" => {
                let p = self.find(arg(0)?)?;
                self.fine = shift;
                self.move_to(p, 8);
            }
            "release" => {
                self.up();
                (self.keys, self.fine) = Default::default();
            }
            "drag" => {
                let p = self.find(arg(0)?)?;
                let (dx, dy) = (num(1)?, num(2)?);
                let steps = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8);
                self.move_to(p, 1);
                (self.keys, self.fine) = (keys, shift);
                self.down(false);
                self.move_to((p.0 + dx, p.1 + dy), steps);
                self.up();
                (self.keys, self.fine) = Default::default();
            }
            "dragto" => {
                let a = self.find(arg(0)?)?;
                self.copy = args.get(2) == Some(&"copy");
                self.move_to(a, 1);
                self.down(false);
                let b = self.find(arg(1)?)?;
                self.move_to(b, 12);
                self.up();
                self.copy = false;
            }
            "wheel" => {
                let p = self.find(arg(0)?)?;
                let lines = num(1)?;
                self.move_to(p, 1);
                if !self.route(PointerEvent::Wheel { x: p.0, y: p.1, lines, fine: shift }) {
                    // As App::wheel: Option/Alt adjusts the control, a plain wheel scrolls.
                    let adjust = keys.alt.then(|| crate::input::wheel_actions(&self.frame.hits, p.0, p.1, lines as f64, shift)).flatten();
                    if let Some(actions) = adjust {
                        self.apply(actions);
                    } else if self.mixer.wheel(&self.frame.scrolls, p.0, p.1, -lines * crate::app::SCROLL_PER_LINE) {
                        self.rebuild();
                    }
                }
            }
            "menu" => self.menu_row(&args.join(" "))?,
            "key" => {
                match arg(0)? {
                    "enter" => self.mixer.key_enter(&mut *self.model),
                    "escape" => self.mixer.key_escape(),
                    "backspace" => self.mixer.key_backspace(),
                    _ => self.mixer.key_text(&args.join(" ")),
                }
                self.rebuild();
            }
            "command" => {
                if !self.mixer.run_command_id(arg(0)?, &mut *self.model) {
                    return Err(format!("command: {:?} did not run", arg(0)?));
                }
                self.rebuild();
            }
            "wait" => self.pump(Duration::from_millis(num(0)? as u64)),
            "settle" => self.settle(),
            "shot" => self.shot(Path::new(arg(0)?))?,
            "hits" => {
                let prefix = args.first().copied().unwrap_or("");
                let mut names = std::collections::BTreeSet::new();
                for h in self.frame.hits.iter().chain(self.overlays.top_hits()) {
                    let n = name(&h.id);
                    if n.starts_with(prefix) {
                        println!("hit {n} [{:.0},{:.0},{:.0},{:.0}]", h.rect.x, h.rect.y, h.rect.w, h.rect.h);
                        names.insert(n);
                    }
                }
                if args.len() > 1 && names.len() != num(1)? as usize {
                    return Err(format!("hits {prefix}: {} named {names:?}, expected {}", names.len(), args[1]));
                }
            }
            "layers" => {
                for l in &self.frame.layers {
                    let r = l.rect;
                    println!("layer {} {:?} [{:.0},{:.0},{:.0},{:.0}]", l.id, l.kind, r.x, r.y, r.w, r.h);
                }
            }
            "nolayer" => {
                let prefix = arg(0)?;
                if let Some(l) = self.frame.layers.iter().find(|l| l.id.starts_with(prefix)) {
                    return Err(format!("layer {} is still open", l.id));
                }
            }
            "layer" => {
                let prefix = arg(0)?;
                let l = self
                    .frame
                    .layers
                    .iter()
                    .find(|l| l.id.starts_with(prefix))
                    .ok_or_else(|| format!("no layer {prefix:?} (try `layers`)"))?;
                let r = l.rect;
                println!("layer {} [{:.0},{:.0},{:.0},{:.0}]", l.id, r.x, r.y, r.w, r.h);
                let now = (r.w, r.h);
                let before = self.layer_sizes.insert(prefix.to_string(), now);
                if let Some(word) = args.get(1) {
                    let (w0, h0) = before.ok_or("layer: nothing to compare with yet")?;
                    let ok = match *word {
                        "larger" => now.0 > w0 && now.1 > h0,
                        "smaller" => now.0 < w0 && now.1 < h0,
                        "same" => now == (w0, h0),
                        _ => return Err(format!("layer: {word:?} is not larger, smaller or same")),
                    };
                    if !ok {
                        return Err(format!("{w0}x{h0} -> {}x{} is not {word}", now.0, now.1));
                    }
                }
            }
            "text" => {
                let want = args.join(" ");
                let drawn: Vec<&str> = self
                    .frame
                    .list
                    .items
                    .iter()
                    .filter_map(|d| if let rack::Draw::Text { text, .. } = d { Some(text.as_str()) } else { None })
                    .collect();
                if !drawn.join(" ").contains(&want) {
                    return Err(format!("{want:?} is not drawn"));
                }
            }
            "echo" => println!("{}", args.join(" ")),
            _ => return Err(format!("unknown command {cmd:?}")),
        }
        Ok(())
    }
}

/// Runs `script` line by line; the first failure is reported with its line number.
pub fn run(assets: Assets, model: Box<dyn MixerModel>, scale: f32, script: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    let mut d = Driver::new(assets, model, scale);
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        d.run_line(line).map_err(|e| format!("{}:{}: {line}: {e}", script.display(), i + 1))?;
    }
    Ok(())
}
