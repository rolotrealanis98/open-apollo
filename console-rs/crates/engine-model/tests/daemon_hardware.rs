//! End to end without hardware: the console model (`EngineMixer`, as uaconsole drives it)
//! against open-apollo's mixer daemon over a recording fake backend
//! (`mixer-engine/tests/fake_hw_daemon.py`, private loopback port). Each control is pressed as
//! the UI presses it; the test then waits for the driver / mixer call the daemon makes and checks
//! what the console shows afterwards. The inventory is in console-rs/WIRING.md.
//!
//! Skipped when Python or the daemon is missing (`OA_REQUIRE_DAEMON_E2E=1` fails instead).

mod fake_daemon;

use engine_model::EngineMixer;
use fake_daemon::{FakeDaemon, mixer_param, pump_until, settle};
use rack::menu::MenuCmd;
use rack::model::StripInfo;
use rack::settings::{self, SettingOp, SettingsGroup};
use rack::{Action, Control, ControlId, MixerModel};
use serde_json::Value as Json;

const IN0: &str = "/devices/0/inputs/0";
const AUX0: &str = "/devices/0/auxs/0";
const DRIVER_RATE: i64 = 0;
const DRIVER_CLOCK: i64 = 1;
const MONITOR: i64 = 2;
const PREAMP: i64 = 1;

fn id(strip: &str, control: Control) -> ControlId {
    ControlId { strip: strip.into(), control }
}

fn mon(control: Control) -> ControlId {
    id("monitor", control)
}

fn strip(m: &EngineMixer, base: &str) -> StripInfo {
    m.inputs().into_iter().chain(m.auxes()).find(|s| &*s.id == base).unwrap_or_else(|| panic!("no strip {base}"))
}

/// `(bus, sub, value, side)` of a `set_mixer_bus_param`.
fn bus_param(name: &str, args: &[Json]) -> Option<(i64, i64, f64, i64)> {
    (name == "set_mixer_bus_param").then_some(())?;
    Some((args.first()?.as_i64()?, args.get(1)?.as_i64()?, args.get(2)?.as_f64()?, args.get(3)?.as_i64()?))
}

/// `(param, value, accepted)` of a `set_driver_param`.
fn driver_param(name: &str, args: &[Json]) -> Option<(i64, i64, bool)> {
    (name == "set_driver_param").then_some(())?;
    Some((args.first()?.as_i64()?, args.get(1)?.as_i64()?, args.get(2)?.as_bool()?))
}

struct Rig {
    d: FakeDaemon,
    m: EngineMixer,
}

impl Rig {
    fn start(extra: &[&str]) -> Option<Rig> {
        let d = FakeDaemon::start(extra)?;
        let m = d.console();
        Some(Rig { d, m })
    }

    /// Does `act` and waits for a hardware call matching `want`.
    fn press(
        &mut self,
        what: &str,
        act: impl FnOnce(&mut EngineMixer),
        want: impl Fn(&str, &[Json]) -> bool,
    ) -> Vec<(String, Vec<Json>)> {
        let mark = self.d.mark();
        act(&mut self.m);
        self.d.wait_hw(&mut self.m, mark, what, want)
    }

    fn mixer(&mut self, what: &str, act: impl FnOnce(&mut EngineMixer), want: (i64, i64, i64, i64)) {
        self.press(what, act, |n, a| mixer_param(n, a) == Some(want));
    }

    /// A mixer-param write with this type, param and value (any channel index).
    fn other(&mut self, what: &str, act: impl FnOnce(&mut EngineMixer), (ty, param, value): (i64, i64, i64)) {
        self.press(what, act, |n, a| mixer_param(n, a).is_some_and(|(t, _, p, v)| (t, p, v) == (ty, param, value)));
    }

    fn bus(&mut self, what: &str, act: impl FnOnce(&mut EngineMixer), want: impl Fn(i64, i64, f64) -> bool) {
        self.press(what, act, |n, a| bus_param(n, a).is_some_and(|(b, s, v, side)| side == 0 && want(b, s, v)));
    }
}

/// Fader / send coefficient (open-apollo `encode_input_fader`): 10^(dB/20)/√2.
fn coeff(db: f64) -> f64 {
    10f64.powf(db / 20.0) / std::f64::consts::SQRT_2
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

/// TALK and TALKBACK TO MONITOR reach the talkback switch (other param 0x46) and its routing
/// (0x47), and the talkback mic is live in the DSP mix (bus 0x0A at the map's −6 dB, its CUE
/// sends written) so TALK is heard.
#[test]
fn talkback_reaches_the_apollo() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let init = r.d.hw_since(0);
    let tb_main: Vec<f64> = init
        .iter()
        .filter_map(|(n, a)| bus_param(n, a))
        .filter(|(b, s, _, side)| (*b, *s, *side) == (0x0A, 0, 0))
        .map(|x| x.2)
        .collect();
    assert!(tb_main.last().is_some_and(|v| *v > 1.9), "talkback bus at init: {tb_main:?}");
    for sub in [1, 2] {
        assert!(
            init.iter()
                .filter_map(|(n, a)| bus_param(n, a))
                .any(|(b, s, v, _)| (b, s) == (0x0A, sub) && near(v, coeff(-6.0))),
            "talkback CUE send {sub}"
        );
    }
    let tb = r.m.talkback().expect("talkback strip");
    assert_eq!(&*tb.id, "/devices/0/inputs/22");

    assert_eq!(r.m.monitor_options().talkback, Some(false));
    r.other("TALK on", |m| m.apply(&Action::Toggle(mon(Control::Talkback))), (MONITOR, 0x46, 1));
    assert_eq!(r.m.monitor_options().talkback, Some(true));
    settle(&mut r.m, 400);
    assert_eq!(r.m.monitor_options().talkback, Some(true), "the engine kept it");
    // A long press: released, off (momentary).
    r.other("TALK released", |m| m.apply(&Action::Set(mon(Control::Talkback), 0.0)), (MONITOR, 0x46, 0));
    settle(&mut r.m, 400);
    assert_eq!(r.m.monitor_options().talkback, Some(false));
    r.mixer(
        "TALKBACK TO MONITOR",
        |m| m.apply(&Action::Toggle(mon(Control::TalkbackToMonitor))),
        (MONITOR, 0, 0x47, 1),
    );
    settle(&mut r.m, 400);
    assert_eq!(r.m.monitor_options().talkback_to_monitor, Some(true));
}

/// SAMPLE RATE and CLOCK SOURCE, from the status bar and from Settings › HARDWARE, reach the
/// driver; a rate the driver refuses snaps back; an external clock without signal shows as
/// unlocked.
#[test]
fn sample_rate_and_clock_source_reach_the_driver() {
    let Some(mut r) = Rig::start(&["--refuse-rate", "192000"]) else { return };
    let rates = r.m.engine_settings().sample_rates;
    let at = |hz: u32| rates.iter().position(|r| *r == hz).unwrap() as u8;
    assert_eq!(r.m.status().sample_rate, Some(48_000));

    r.press(
        "RATE 96 kHz",
        |m| m.apply(&Action::Tap(id("global", Control::SampleRate(at(96_000))))),
        |n, a| driver_param(n, a) == Some((DRIVER_RATE, 96_000, true)),
    );
    assert_eq!(r.m.status().sample_rate, Some(96_000));
    settle(&mut r.m, 500);
    assert_eq!(r.m.status().sample_rate, Some(96_000), "the driver's rate, read back");
    assert_eq!(
        r.d.tree_since(0).iter().filter(|(p, v)| p == "/SampleRate/value" && v == &Json::from(96_000)).count(),
        1
    );

    // Settings › HARDWARE › SAMPLE RATE: the combo's option index.
    let src = r.m.properties().unwrap();
    let f = settings::fields(src, SettingsGroup::Hardware);
    let rate = f.iter().find(|f| f.label == "SAMPLE RATE").unwrap();
    let settings::FieldKind::Choice { options, .. } = &rate.kind else { panic!() };
    let i = options.iter().position(|o| o == "44.1 kHz").unwrap();
    let key = rate.key.to_string();
    r.press(
        "SAMPLE RATE 44.1 kHz",
        |m| m.apply(&Action::Tap(id("settings", Control::SettingChoose(key.as_str().into(), i as u16)))),
        |n, a| driver_param(n, a) == Some((DRIVER_RATE, 44_100, true)),
    );
    settle(&mut r.m, 500);
    assert_eq!(r.m.engine_settings().sample_rate, Some(44_100));

    // Refused by the driver: the console shows the rate in effect again.
    r.press(
        "RATE 192 kHz (refused)",
        |m| m.apply(&Action::Tap(id("global", Control::SampleRate(at(192_000))))),
        |n, a| driver_param(n, a) == Some((DRIVER_RATE, 192_000, false)),
    );
    pump_until(&mut r.m, "snap back to 44.1 kHz", |m| m.status().sample_rate == Some(44_100));
    assert_eq!(r.m.store().refusals().count(), 1);
    assert!(
        r.d.tree_since(0).iter().all(|(p, v)| !(p == "/SampleRate/value" && v == &Json::from(192_000))),
        "never published"
    );

    // CLOCK: S/PDIF with no signal → unlocked (the status bar's CLOCK outline blinks).
    let clocks = r.m.engine_settings().clock_sources;
    let spdif = clocks.iter().position(|c| c == "S/PDIF").unwrap() as u8;
    assert_eq!(r.m.status().clock_locked, Some(true));
    r.press(
        "CLOCK S/PDIF",
        |m| m.apply(&Action::Tap(id("global", Control::ClockSource(spdif)))),
        |n, a| driver_param(n, a) == Some((DRIVER_CLOCK, 0x00, true)),
    );
    pump_until(&mut r.m, "unlocked", |m| m.status().clock_locked == Some(false));
    assert_eq!(r.m.status().clock_source.as_deref(), Some("S/PDIF"));
    let internal = clocks.iter().position(|c| c == "Internal").unwrap();
    r.press(
        "CLOCK SOURCE Internal (Settings)",
        |m| m.apply_setting("/ClockSource", &SettingOp::Choose(internal)),
        |n, a| driver_param(n, a) == Some((DRIVER_CLOCK, 0x0C, true)),
    );
    pump_until(&mut r.m, "locked", |m| m.status().clock_locked == Some(true));
}

/// Preamp and input-strip controls of ANALOG 1.
#[test]
fn preamp_and_input_strip_controls_reach_the_mixer() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let p = |name: &str| id(IN0, Control::Preamp(name.into()));
    let s = strip(&r.m, IN0);
    r.mixer("48V", |m| m.apply(&Action::Toggle(p("48v"))), (PREAMP, 0, 0x03, 1));
    r.mixer("PAD", |m| m.apply(&Action::Toggle(p("pad"))), (PREAMP, 0, 0x01, 1));
    r.mixer("LOW CUT", |m| m.apply(&Action::Toggle(p("low_cut"))), (PREAMP, 0, 0x04, 1));
    r.mixer("Ø", |m| m.apply(&Action::Toggle(p("phase"))), (PREAMP, 0, 0x05, 1));
    let pre = r.m.preamp(&s).unwrap();
    assert!(pre.phantom && pre.pad && pre.low_cut && pre.phase);
    // Gain 0.5 → 37.5 dB → the hardware's whole dB (38): the C value is dB − 9.
    r.mixer("GAIN", |m| m.apply(&Action::Set(p("preamp_gain"), 0.5)), (PREAMP, 0, 0x06, 29));
    settle(&mut r.m, 400);
    assert_eq!(r.m.preamp(&s).unwrap().gain_text, "38 dB");
    let line = r.m.input_sources(&s).iter().position(|x| x == "LINE").unwrap() as u8;
    r.mixer("MIC → LINE", |m| m.apply(&Action::Tap(id(IN0, Control::InputSource(line)))), (PREAMP, 0, 0x00, 1));
    assert_eq!(r.m.preamp(&s).unwrap().source, "LINE");
    // S/PDIF input SR CONVERT (preamp menu "Sample Rate Conversion").
    r.mixer(
        "SR CONVERT",
        |m| m.apply(&Action::Tap(id("/devices/0/inputs/12", Control::Menu(MenuCmd::SrConvert(true))))),
        (MONITOR, 1, 0x1f, 1),
    );
    r.mixer("Link", |m| m.apply(&Action::Tap(id(IN0, Control::Menu(MenuCmd::Link)))), (PREAMP, 0, 0x02, 1));
    settle(&mut r.m, 400);
    r.mixer("Unlink", |m| m.apply(&Action::Tap(id(IN0, Control::Menu(MenuCmd::Link)))), (PREAMP, 0, 0x02, 0));
    settle(&mut r.m, 400);

    // The daemon starts every input muted: M lit, and the fader waits in the tree.
    assert!(r.m.main(&s).mute);
    let mark = r.d.mark();
    r.m.apply(&Action::Set(id(IN0, Control::Fader), 0.6));
    settle(&mut r.m, 300);
    assert!(
        r.d.hw_since(mark)
            .iter()
            .filter_map(|(n, a)| bus_param(n, a))
            .all(|(b, sub, v, _)| !(b == 0 && sub == 0 && v > 0.0)),
        "a muted strip stays silent"
    );
    // Unmute: the fader's level (0.6 tapered = −10 dB).
    r.bus(
        "M off",
        |m| m.apply(&Action::Toggle(id(IN0, Control::Mute))),
        |b, s, v| (b, s) == (0, 0) && near(v, coeff(-10.0)),
    );
    assert!(!r.m.main(&s).mute);
    r.bus(
        "pan",
        |m| m.apply(&Action::Set(id(IN0, Control::PanL), 0.25)),
        |b, s, v| (b, s) == (0, 0) && v > coeff(-10.0),
    );
    r.bus(
        "S (solo another strip)",
        |m| m.apply(&Action::Toggle(id("/devices/0/inputs/1", Control::Solo))),
        |b, s, v| (b, s) == (0, 0) && v == 0.0,
    );
    r.bus(
        "Clear Solos",
        |m| m.apply(&Action::Tap(id("global", Control::ClearSolos))),
        |b, s, v| (b, s) == (0, 0) && v > 0.0,
    );
    r.bus("M on", |m| m.apply(&Action::Toggle(id(IN0, Control::Mute))), |b, s, v| (b, s) == (0, 0) && v == 0.0);
}

/// The Hi-Z jack takes the preamp out of Mic mode: the daemon publishes `48V/enabled`,
/// `Pad/enabled` (and `IOType/enabled`, `Stereo/enabled`) false, and the console shows them off.
#[test]
fn hi_z_input_disables_48v_and_pad() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let s = strip(&r.m, IN0);
    let pre = r.m.preamp(&s).unwrap();
    assert_eq!(pre.source, "MIC");
    assert!(pre.phantom_enabled && pre.pad_enabled);
    r.m.store_mut().commit(&format!("{IN0}/preamps/0/HiZ"), true);
    pump_until(&mut r.m, "Hi-Z input", |m| {
        let s = strip(m, IN0);
        let p = m.preamp(&s).unwrap();
        p.source == "HI-Z" && !p.phantom_enabled && !p.pad_enabled
    });
    // A console connecting now reads them all from the tree (the open one re-reads only the
    // preamp node on a Hi-Z change)
    let fresh = r.d.console();
    let enabled = |path: &str| fresh.store().meta(path).and_then(|m| m.enabled);
    assert_eq!(enabled(&format!("{IN0}/preamps/0/48V")), Some(false));
    assert_eq!(enabled(&format!("{IN0}/preamps/0/Pad")), Some(false));
    assert_eq!(enabled(&format!("{IN0}/IOType")), Some(false));
    assert_eq!(enabled(&format!("{IN0}/Stereo")), Some(false));
    // CH2 has no jack in: still a Mic input with its own controls
    let two = strip(&r.m, "/devices/0/inputs/1");
    let p2 = r.m.preamp(&two).unwrap();
    assert!(p2.phantom_enabled && p2.pad_enabled);
}

/// The front Hi-Z jack as the unit reports it: readback word 0 bit 24 + input (bit 25 = ANALOG 2,
/// read on Titan's x4 with a cable in, 2026-10-19). ANALOG 2 turns HI-Z with 48V and PAD off,
/// ANALOG 1 stays a Mic input, and pulling the cable gives MIC back.
#[test]
fn hi_z_jack_follows_the_readback() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let in1 = "/devices/0/inputs/1";
    let mut rb = [0u32; 40];
    r.d.set_readback(&rb);
    settle(&mut r.m, 300);
    rb[0] = 1 << 25;
    r.d.set_readback(&rb);
    pump_until(&mut r.m, "ANALOG 2 Hi-Z", |m| {
        let p = m.preamp(&strip(m, in1)).unwrap();
        p.source == "HI-Z" && !p.phantom_enabled && !p.pad_enabled
    });
    // The open console re-reads the input and its partner: no source choice on ANALOG 2, and
    // neither leg can start a link
    let enabled = |m: &EngineMixer, path: &str| m.store().meta(path).and_then(|m| m.enabled);
    pump_until(&mut r.m, "IOType and Stereo metadata", |m| {
        enabled(m, &format!("{in1}/IOType")) == Some(false)
            && enabled(m, &format!("{in1}/Stereo")) == Some(false)
            && enabled(m, &format!("{IN0}/Stereo")) == Some(false)
    });
    let p1 = r.m.preamp(&strip(&r.m, IN0)).unwrap();
    assert_eq!(p1.source, "MIC");
    assert!(p1.phantom_enabled && p1.pad_enabled);
    rb[0] = 0;
    r.d.set_readback(&rb);
    pump_until(&mut r.m, "ANALOG 2 back to Mic", |m| {
        let p = m.preamp(&strip(m, in1)).unwrap();
        p.source == "MIC" && p.phantom_enabled && p.pad_enabled
    });
    pump_until(&mut r.m, "IOType and Stereo back", |m| {
        enabled(m, &format!("{in1}/IOType")) == Some(true) && enabled(m, &format!("{IN0}/Stereo")) == Some(true)
    });
}

/// Link on ANALOG 1 (and from ANALOG 2): one stereo strip, the right preamp ganged, and bus 1
/// carrying the leader's level hard right (UA's engine: SetProperty 0x45, UpdateHardwareState).
#[test]
fn link_drives_the_pair() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let strips = r.m.inputs().len();
    let link = |base: &'static str| move |m: &mut EngineMixer| m.apply(&Action::Tap(id(base, Control::Menu(MenuCmd::Link))));
    r.mixer("Link from ANALOG 2", link("/devices/0/inputs/1"), (PREAMP, 0, 0x02, 1));
    pump_until(&mut r.m, "one stereo strip", |m| m.inputs().len() == strips - 1);
    assert!(strip(&r.m, IN0).stereo);
    let p = |name: &str| id(IN0, Control::Preamp(name.into()));
    r.mixer("48V on both", |m| m.apply(&Action::Toggle(p("48v"))), (PREAMP, 1, 0x03, 1));
    r.mixer("GAIN on both", |m| m.apply(&Action::Set(p("preamp_gain"), 0.5)), (PREAMP, 1, 0x06, 29));
    // Unmute: bus 1 (ANALOG 2) gets the leader's 0 dB hard right, never input 1's own mute.
    let mark = r.d.mark();
    r.press(
        "M off",
        |m| m.apply(&Action::Toggle(id(IN0, Control::Mute))),
        |n, a| bus_param(n, a).is_some_and(|(b, s, v, side)| (b, s, side) == (1, 0, 1) && near(v, 1.0)),
    );
    settle(&mut r.m, 300);
    let last = r.d.hw_since(mark).iter().filter_map(|(n, a)| bus_param(n, a)).filter(|x| (x.0, x.1, x.3) == (1, 0, 1)).last();
    assert!(last.is_some_and(|x| near(x.2, 1.0)), "bus 1 right: {last:?}");
    r.mixer("Unlink", link(IN0), (PREAMP, 0, 0x02, 0));
    pump_until(&mut r.m, "two mono strips", |m| m.inputs().len() == strips);
}

/// An input's sends: level, M, PRE / POST, pan (pans move a stereo send pair with
/// `--stereo-sends`).
#[test]
fn input_sends_reach_the_mixer() {
    let Some(mut r) = Rig::start(&["--stereo-sends"]) else { return };
    let s = strip(&r.m, IN0);
    r.m.apply(&Action::Toggle(id(IN0, Control::Mute)));
    r.m.apply(&Action::Set(id(IN0, Control::Fader), 0.6)); // −10 dB
    settle(&mut r.m, 300);
    assert_eq!(r.m.input_sends(&s)[0].name, "AUX 1");
    // AUX 1 send at 0.7 tapered (−2.3 dB), post-fader by the aux bus's default: + fader.
    r.bus(
        "AUX 1 send level",
        |m| m.apply(&Action::Set(id(IN0, Control::SendLevel(0)), 0.7)),
        |b, s, v| (b, s) == (0, 3) && v > 0.0,
    );
    let post =
        r.d.hw_since(0)
            .iter()
            .rev()
            .filter_map(|(n, a)| bus_param(n, a))
            .find(|(b, s, _, side)| (*b, *s, *side) == (0, 3, 0))
            .unwrap()
            .2;
    // PRE: the fader leaves the send (it gets louder by 10 dB).
    assert_eq!(r.m.send_pre(&s, 0), Some(false));
    r.bus(
        "AUX 1 PRE",
        |m| m.apply(&Action::Toggle(id(IN0, Control::SendPre(0)))),
        |b, s, v| (b, s) == (0, 3) && near(v / post, 10f64.powf(0.5)),
    );
    assert_eq!(r.m.send_pre(&s, 0), Some(true));
    r.bus(
        "AUX 1 send pan",
        |m| m.apply(&Action::Set(id(IN0, Control::SendPan(0)), 0.0)),
        |b, s, v| (b, s) == (0, 3) && v > 0.0,
    );
    settle(&mut r.m, 400);
    assert!(near(r.m.input_sends(&s)[0].pan, 0.0), "pan read back");
    r.bus(
        "AUX 1 send M",
        |m| m.apply(&Action::Toggle(id(IN0, Control::SendMute(0)))),
        |b, s, v| (b, s) == (0, 3) && v == 0.0,
    );
    assert!(r.m.input_sends(&s)[0].muted);
    // COPY MIX TO CUE 1 (fader / pan menu): the root `post /CopyToSends {"send": 2}`.
    r.bus(
        "COPY MIX TO CUE 1",
        |m| m.apply(&Action::Tap(id(IN0, Control::Menu(MenuCmd::CopyMixTo(2))))),
        |b, s, v| (b, s) == (0, 1) && near(v, coeff(-10.0)),
    );
    settle(&mut r.m, 400);
    let cue1 = r.m.input_sends(&s).into_iter().find(|x| x.name == "CUE 1").unwrap();
    assert!((cue1.level - 0.6).abs() < 1e-3, "the CUE 1 knob follows: {}", cue1.level);
}

/// AUX return: fader, MUTE, MONO, PRE / POST (rewrites the input sends that follow it), the
/// aux → CUE send.
#[test]
fn aux_strip_controls_reach_the_mixer() {
    let Some(mut r) = Rig::start(&[]) else { return };
    r.m.apply(&Action::Toggle(id(IN0, Control::Mute)));
    r.m.apply(&Action::Set(id(IN0, Control::Fader), 0.6));
    r.m.apply(&Action::Set(id(IN0, Control::SendLevel(0)), 0.7));
    settle(&mut r.m, 300);
    r.bus(
        "AUX fader",
        |m| m.apply(&Action::Set(id(AUX0, Control::Fader), 0.6)),
        |b, s, v| (b, s) == (0x10, 0) && v > 0.0,
    );
    r.bus("AUX MUTE", |m| m.apply(&Action::Toggle(id(AUX0, Control::Mute))), |b, s, v| (b, s) == (0x10, 0) && v == 0.0);
    r.mixer("AUX MONO", |m| m.apply(&Action::Toggle(id(AUX0, Control::Mono))), (MONITOR, 0, 0x0A, 1));
    r.bus(
        "AUX PRE (input sends follow)",
        |m| m.apply(&Action::Toggle(id(AUX0, Control::PrePost))),
        |b, s, v| (b, s) == (0, 3) && v > 0.0,
    );
    let st = strip(&r.m, AUX0);
    assert!(r.m.main(&st).pre);
    r.bus(
        "AUX → CUE 1 send",
        |m| m.apply(&Action::Set(id(AUX0, Control::SendLevel(0)), 0.6)),
        |b, s, v| (b, s) == (0x10, 1) && v > 0.0,
    );
}

/// The monitor column and the control room: level, MUTE, MONO, DIM and its amount, SOURCE,
/// ALT, a headphone's source.
#[test]
fn monitor_controls_reach_the_mixer() {
    let Some(mut r) = Rig::start(&[]) else { return };
    r.mixer("MONITOR level", |m| m.apply(&Action::Set(mon(Control::MonitorLevel), 0.5)), (MONITOR, 1, 0x01, 165));
    r.mixer("MUTE", |m| m.apply(&Action::Toggle(mon(Control::MonitorMute))), (MONITOR, 0, 0x03, 2));
    r.mixer("MONO", |m| m.apply(&Action::Toggle(mon(Control::MonitorMono))), (MONITOR, 0, 0x03, 3));
    r.other("DIM", |m| m.apply(&Action::Toggle(mon(Control::MonitorDim))), (MONITOR, 0x44, 1));
    r.mixer("DIM amount", |m| m.apply(&Action::Set(mon(Control::DimLevel), 0.5)), (MONITOR, 0, 0x43, 3));
    r.mixer("SOURCE CUE 1", |m| m.apply(&Action::Tap(mon(Control::CrSource(1)))), (MONITOR, 1, 0x04, 1));
    r.mixer("ALT 1", |m| m.apply(&Action::Tap(mon(Control::MonitorAlt(1)))), (MONITOR, 0, 0x3a, 1));
    // ALT 1 TRIM +15 dB: `AltMonTrim` on line outs 1/2 → 0x38, 15 × 2 + 60.
    r.other("ALT 1 TRIM", |m| m.apply(&Action::Set(mon(Control::AltTrim(0)), 0.75)), (MONITOR, 0x38, 90));
    let hp = r.m.headphone_outputs();
    let cue2 = hp[0].options.iter().position(|o| o == "CUE 2").unwrap() as u8;
    r.mixer(
        "HP 1 ← CUE 2",
        |m| m.apply(&Action::Tap(mon(Control::HeadphoneSource { output: 0, option: cue2 }))),
        (MONITOR, 1, 0x3f, 1),
    );
    settle(&mut r.m, 400);
    // HP 1 ← MON: no known hardware encoding; the daemon refuses it and the console shows the
    // source in effect again (CUE 2), nothing reaches the Apollo.
    let mark = r.d.mark();
    let mon_src = hp[0].options.iter().position(|o| o == "MON").unwrap() as u8;
    r.m.apply(&Action::Tap(mon(Control::HeadphoneSource { output: 0, option: mon_src })));
    pump_until(&mut r.m, "HP 1 back on CUE 2", |m| {
        m.headphone_outputs()[0].source == cue2 as usize && m.store().refusals().count() == 1
    });
    assert!(r.d.hw_since(mark).iter().all(|(n, a)| mixer_param(n, a).is_none_or(|(_, _, p, _)| p != 0x3f)));
    // SOURCE CUE 3: no verified encoding; refused, the button snaps back to CUE 1.
    let mark = r.d.mark();
    r.m.apply(&Action::Tap(mon(Control::CrSource(3))));
    pump_until(&mut r.m, "SOURCE back on CUE 1", |m| m.monitor_options().cr_source == 1 && m.store().refusals().count() == 2);
    assert!(r.d.hw_since(mark).iter().all(|(n, a)| mixer_param(n, a).is_none_or(|(t, _, p, _)| (t, p) != (MONITOR, 0x04))));
    let o = r.m.monitor_options();
    assert_eq!((o.dim, o.cr_source, o.alt_selection), (Some(true), 1, 1));
    assert_eq!(r.m.headphone_outputs()[0].source, cue2 as usize);
    // Window › Notifications lists both refusals, newest first.
    let n = r.m.notifications();
    assert_eq!(n.len(), 2, "{n:?}");
    assert!(n.iter().all(|l| l.starts_with('/') && l.contains(": ")), "{n:?}");
}

/// Settings › HARDWARE and the device OPTIONS, OPTIONS › DISPLAY, CUE OUTPUTS.
#[test]
fn settings_reach_the_apollo() {
    let Some(mut r) = Rig::start(&[]) else { return };
    let choose = |key: &'static str, i: u16| {
        move |m: &mut EngineMixer| m.apply(&Action::Tap(id("settings", Control::SettingChoose(key.into(), i))))
    };
    r.mixer(
        "DIGITAL MIRROR ON",
        choose("bool:OFF:ON:/devices/0/outputs/18/MirrorsToDigital", 1),
        (MONITOR, 10, 0x1e, 1),
    );
    r.other("DIGITAL OUTPUT ADAT", choose("/devices/0/TOSLinkOutput", 1), (MONITOR, 0x21, 8));
    r.press("DIGITAL INPUT ADAT", choose("/devices/0/TOSLink", 1), |n, a| {
        mixer_param(n, a).is_some_and(|(t, _, p, _)| (t, p) == (MONITOR, 0x21))
    });
    r.mixer("MON LEVEL 14 dBu", choose("map:mon_level:/devices/0/outputs/18/Pad", 0), (MONITOR, 1, 0x32, 1));
    r.other(
        "OUTPUT REFERENCE 1-2 −10 dBV",
        choose("pair:ref_level:/devices/0/outputs/0/Pad,/devices/0/outputs/1/Pad", 0),
        (MONITOR, 0x19, 1),
    );
    r.mixer("CLIP HOLD NONE", choose("/ClipHold", 0), (MONITOR, 0, 0x13, 0));
    r.mixer("PEAK HOLD NONE", choose("/PeakHold", 0), (MONITOR, 0, 0x14, 0));
    // CUE BUS COUNT shows 2, 3, 4: the third is 4 (CUE 3 and 4 buses on, their outputs active).
    r.mixer("CUE BUS COUNT 4", choose("/CueBusCount", 2), (0, 6, 0x01, 1));
    pump_until(&mut r.m, "CUE 3/4 active", |m| m.cue_outputs().len() == 4);
    r.other("ALT COUNT 1", choose("alt:/devices/0", 1), (MONITOR, 0x3b, 1));
    r.other("IDENTIFY", |m| m.apply_setting("post:Identify:/devices/0", &SettingOp::Press), (MONITOR, 0x1d, 1));
    r.mixer(
        "CUE 1 MONO",
        |m| m.apply_setting("toggle:/devices/0/outputs/14/MixToMono", &SettingOp::Press),
        (MONITOR, 0, 0x06, 1),
    );
    r.mixer("CUE 1 MIX → CUE", choose("map:cue_source:/devices/0/outputs/14/MixInSource", 1), (MONITOR, 0, 0x05, 0));
}

/// Launched read-only (no `--writes`), nothing reaches the Apollo; Settings' WRITES switch opens
/// the engine connection as well as the writer.
#[test]
fn writes_switch_opens_the_engine_connection() {
    let Some(d) = FakeDaemon::start(&[]) else { return };
    let mut m = d.console_with(false);
    let mark = d.mark();
    m.apply(&Action::Toggle(mon(Control::MonitorMute)));
    settle(&mut m, 400);
    assert!(d.hw_since(mark).is_empty(), "read-only: {:?}", d.hw_since(mark));
    m.apply(&Action::Toggle(id("settings", Control::WritesEnabled)));
    assert!(m.engine_settings().writes_enabled);
    let mark = d.mark();
    m.apply(&Action::Toggle(mon(Control::MonitorMono)));
    d.wait_hw(&mut m, mark, "MONO after WRITES on", |n, a| {
        mixer_param(n, a).is_some_and(|(t, _, p, _)| (t, p) == (MONITOR, 0x03))
    });
}
