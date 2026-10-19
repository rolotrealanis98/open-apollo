use std::sync::Arc;
use std::time::{Duration, Instant};

use engine_model::{EngineMixer, UNISON_SLOT, fader_readout};
use rack::geometry::StripKind;
use rack::model::{InsertState, MixerModel, PreviewMixer, StripInfo};
use rack::{Action, Control, ControlId, scales};
use ua_engine::{MeterReading, MockEngine, MockOptions, Snapshot, Status, Value};

fn fixture() -> String {
    format!(
        "{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn offline() -> EngineMixer {
    EngineMixer::from_snapshot(fixture()).expect("fixture")
}

fn id(strip: &StripInfo, control: Control) -> ControlId {
    ControlId {
        strip: strip.id.clone(),
        control,
    }
}

fn preamp(name: &str) -> Control {
    Control::Preamp(Arc::from(name))
}

/// The live x4 capture (sanitized names) maps onto the same strips as rack's preview of
/// console-main-1.png.
#[test]
fn strips_match_the_console_layout() {
    let m = offline();
    let preview = PreviewMixer::default();
    let titles = |v: Vec<StripInfo>| {
        v.iter()
            .map(|s| {
                (
                    s.title.clone(),
                    s.stereo,
                    s.preamp_layout.is_some(),
                    s.shows_sr_convert,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(m.inputs()), titles(preview.inputs()));
    assert_eq!(titles(m.auxes()), titles(preview.auxes()));
    assert_eq!(m.inputs()[0].id.as_ref(), "/devices/0/inputs/0");
    assert_eq!(
        m.inputs()[0].preamp_layout.as_deref(),
        Some("preamp_channel_strip_controls")
    );
    assert!(m.inputs().iter().all(|s| s.kind == StripKind::Input));
    assert!(m.auxes().iter().all(|s| s.kind == StripKind::Aux));
    assert_eq!(m.session_name(), "Session*"); // `/Dirty` is true in the capture
}

#[test]
fn main_strip_state() {
    let m = offline();
    let inputs = m.inputs();
    let a1 = m.main(&inputs[0]);
    assert_eq!(a1.name, "Vocal Mic"); // mono: Name, not StereoName
    assert!(a1.mute && !a1.solo);
    assert_eq!((a1.pan_l, a1.pan_r), (0.5, 0.5));
    assert_eq!(a1.readout, "-0.0"); // FaderLevel −0.01: Console prints the sign (p1 SM7B)
    let pair = m.main(&inputs[2]);
    assert_eq!(pair.name, "Keys"); // pair: StereoName
    assert_eq!((pair.pan_l, pair.pan_r), (0.0, 1.0));
    let aux = m.main(&m.auxes()[0]);
    assert_eq!(aux.name, "AUX 1");
    assert_eq!(aux.readout, "-9.4");
    assert!(!aux.pre && !aux.mono); // SendPostFader true → POST
    assert!((aux.fader - scales::fader_tapered(-9.4)).abs() < 0.01);
}

#[test]
fn preamp_state() {
    let m = offline();
    let inputs = m.inputs();
    let line = m.preamp(&inputs[0]).unwrap();
    assert_eq!(line.source, "LINE");
    assert!(!line.phantom_enabled && !line.pad_enabled); // Line mode: no 48V/pad
    assert_eq!(line.gain_text, "27 dB"); // custom display text "---" has no digits
    assert!((line.gain - 0.3148148).abs() < 1e-6);
    let mic = m.preamp(&inputs[1]).unwrap();
    assert_eq!(mic.source, "MIC");
    assert!(mic.phantom_enabled && mic.pad_enabled);
    assert_eq!(mic.gain_text, "10 dB");
    assert!(m.preamp(&inputs[3]).is_none()); // S/PDIF
    assert_eq!(line.input_led, 0.0);
}

#[test]
fn inserts_unison_sends_and_flags() {
    let m = offline();
    let inputs = m.inputs();
    assert_eq!(
        m.inserts(&inputs[0]),
        [
            InsertState::Empty,
            InsertState::Loaded {
                name: "UAD SPL Vitalizer MK2-T".into(),
                power: true
            },
            InsertState::Empty,
            InsertState::Empty
        ]
    );
    assert_eq!(
        m.inserts(&inputs[1])
            .iter()
            .filter(|s| matches!(s, InsertState::Loaded { .. }))
            .count(),
        2
    );
    assert_eq!(m.inserts(&m.auxes()[0]).len(), 4);
    assert_eq!(m.unison(&inputs[0]), InsertState::Empty);
    assert_eq!(m.unison(&inputs[3]), InsertState::Empty);
    // CueBusCount 2: the aux shows CUE 1 and CUE 2 only.
    let sends = m.sends(&m.auxes()[0]);
    assert_eq!(
        sends.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["CUE 1", "CUE 2"]
    );
    assert!(sends.iter().all(|s| s.level == 0.0 && !s.muted));
    assert!(m.sends(&inputs[0]).is_empty());
    assert!(!m.records_pre_effects(&inputs[0]));
    let mon = m.monitor();
    assert_eq!(mon.readout, "-52.0 dB");
    assert!((mon.level - 0.0925926).abs() < 1e-6);
    assert!(!mon.mono && !mon.mute);
}

#[test]
fn meters_use_the_console_scale() {
    let mut m = offline();
    let inputs = m.inputs();
    assert_eq!(m.meters(&inputs[0]).left, 0.0); // −77 dBFS (unit offline at capture)
    let base = m.store().topology().inputs[2].meters.clone();
    let gen0 = m.meter_generation();
    m.store().meters().set(
        &base[0].base,
        MeterReading {
            level_db: -6.0,
            peak_db: -3.0,
            clip: false,
            updates: 1,
        },
    );
    m.store().meters().set(
        &base[1].base,
        MeterReading {
            level_db: -18.0,
            peak_db: -12.0,
            clip: true,
            updates: 1,
        },
    );
    assert!(m.meter_generation() > gen0);
    let f = m.meters(&inputs[2]);
    assert!((f.left - scales::meter_fraction(-6.0)).abs() < 1e-9);
    assert!((f.right - scales::meter_fraction(-18.0)).abs() < 1e-9);
    assert!(!f.clip_left && f.clip_right);
    // The engine-held peaks ride along on the same scale.
    assert!((f.peak_left - scales::meter_fraction(-3.0)).abs() < 1e-9);
    assert!((f.peak_right - scales::meter_fraction(-12.0)).abs() < 1e-9);
    // Mono strips mirror the left meter.
    let mono = m.store().topology().inputs[0].meters[0].base.clone();
    m.store().meters().set(
        &mono,
        MeterReading {
            level_db: -12.0,
            peak_db: -12.0,
            clip: false,
            updates: 1,
        },
    );
    let f = m.meters(&inputs[0]);
    assert_eq!(f.left, f.right);
    m.set_live_meters(false);
    assert_eq!(m.meters(&inputs[2]).left, 0.0);
}

#[test]
fn aux_and_monitor_meters_carry_peaks() {
    let m = offline();
    let reading = |level_db, peak_db| MeterReading { level_db, peak_db, clip: false, updates: 1 };
    let topology = m.store().topology();
    let aux = topology.auxes[0].meters[0].base.clone();
    let mon = topology.monitor.as_ref().expect("monitor").meters[0].base.clone();
    m.store().meters().set(&aux, reading(-30.0, -9.0));
    m.store().meters().set(&mon, reading(-30.0, -6.0));
    let aux_frame = m.meters(&m.auxes()[0]);
    assert!((aux_frame.peak_left - scales::meter_fraction(-9.0)).abs() < 1e-9);
    assert!((m.monitor_meters().peak_left - scales::meter_fraction(-6.0)).abs() < 1e-9);
    assert!(m.monitor_meters().peak_left > m.monitor_meters().left);
}

#[test]
fn send_meters_reach_the_send_states() {
    let m = offline();
    let reading = |level_db, peak_db| MeterReading { level_db, peak_db, clip: false, updates: 1 };
    let inputs = m.inputs();
    let topology = m.store().topology();
    // An input send (post-fader meter under `sends/S/meters/0`) and an aux's cue send.
    let strip = topology.inputs.iter().position(|s| !s.sends.is_empty() && !s.sends[0].meters.is_empty()).expect("an input with send meters");
    let key = topology.inputs[strip].sends[0].meters[0].base.clone();
    let cue = topology.auxes[0].sends.iter().find(|s| s.is_cue() && !s.meters.is_empty()).expect("aux cue send meter").meters[0].base.clone();
    m.set_meter_time(0.0);
    assert_eq!(m.input_sends(&inputs[strip])[0].meter, rack::model::MeterFrame::default());
    m.store().meters().set(&key, reading(-12.0, -6.0));
    m.store().meters().set(&cue, reading(-24.0, -18.0));
    // Console eases the bar up over a few 16.7 ms steps; a second later it is there.
    m.set_meter_time(1.0);
    let send = &m.input_sends(&inputs[strip])[0];
    assert!((send.meter.left - scales::meter_fraction(-12.0)).abs() < 1e-9);
    assert!((send.meter.peak_left - scales::meter_fraction(-6.0)).abs() < 1e-9);
    let aux = m.sends(&m.auxes()[0]);
    assert!(aux.iter().any(|s| (s.meter.left - scales::meter_fraction(-24.0)).abs() < 1e-9));
    // The Sends window asks by node.
    assert_eq!(m.send_meter(key.trim_end_matches("/meters/0")).left, send.meter.left);
}

/// Offline (no connection, writer gate closed): actions move local values only.
#[test]
fn offline_actions_move_local_values() {
    let mut m = offline();
    let s = m.inputs()[1].clone();
    m.apply(&Action::GestureBegin(id(&s, Control::Fader)));
    m.apply(&Action::Set(id(&s, Control::Fader), 0.6));
    m.apply(&Action::GestureEnd(id(&s, Control::Fader)));
    let main = m.main(&s);
    assert_eq!(main.fader, 0.6);
    assert_eq!(main.readout, "-10.0"); // local dB companion
    m.apply(&Action::Toggle(id(&s, Control::Mute)));
    assert!(!m.main(&s).mute);
    // 48V is not offered in Line mode, so the toggle does nothing.
    let line = m.inputs()[0].clone();
    m.apply(&Action::Toggle(id(&line, preamp("48v"))));
    assert!(!m.preamp(&line).unwrap().phantom);
    assert_eq!(m.store().writer().sent_count(), 0);
    assert!(m.store().writer().suppressed_count() >= 6);
    // Taps are queued; insert controls resolve to slots.
    m.apply(&Action::Tap(id(&s, Control::Insert(0))));
    m.apply(&Action::Tap(id(&s, Control::Insert(UNISON_SLOT))));
    let taps = m.take_taps();
    assert_eq!(taps.len(), 2);
    assert_eq!(
        m.insert_slot(&taps[0]).unwrap().base,
        "/devices/0/inputs/1/effects/0"
    );
    assert_eq!(
        m.insert_slot(&taps[1]).unwrap().base,
        "/devices/0/inputs/1/preamps/0/effects/0"
    );
    assert!(
        m.insert_slot(&taps[0])
            .unwrap()
            .parameter(3)
            .normalized()
            .ends_with("/effects/0/parameters/3/NormalizedValue")
    );
    assert!(m.take_taps().is_empty());
}

#[test]
fn readout_format() {
    assert_eq!(fader_readout(-144.0), "-∞");
    assert_eq!(fader_readout(f64::NEG_INFINITY), "-∞");
    // Console keeps the sign of the unrounded value (p1: SM7B −0.01 → "-0.0").
    assert_eq!(fader_readout(-0.01), "-0.0");
    assert_eq!(fader_readout(0.047), "+0.0");
    assert_eq!(fader_readout(0.0), "0.0");
    assert_eq!(fader_readout(3.04), "+3.0");
}

fn live(server: &MockEngine, writes: bool) -> EngineMixer {
    let mut m = EngineMixer::connect(server.endpoint(), writes);
    let deadline = Instant::now() + Duration::from_secs(10);
    while *m.store().status() != Status::Live {
        m.pump();
        assert!(
            Instant::now() < deadline,
            "not live: {:?}",
            m.store().status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    m
}

fn wait_sets(server: &MockEngine, n: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.set_commands().len() < n {
        assert!(Instant::now() < deadline, "got {:?}", server.set_commands());
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    server.set_commands()
}

/// `apply(Action)` → the engine `set` commands Console would send.
#[test]
fn actions_write_to_the_engine() {
    let tree = Snapshot::load(fixture()).unwrap().tree;
    let server = MockEngine::start(tree, MockOptions::default()).unwrap();
    let mut m = live(&server, true);
    let inputs = m.inputs();
    let (line, mic) = (inputs[0].clone(), inputs[1].clone());
    let aux = m.auxes()[0].clone();
    let monitor = ControlId {
        strip: Arc::from("monitor"),
        control: Control::MonitorLevel,
    };
    let drag = |m: &mut EngineMixer, cid: ControlId, values: &[f64]| {
        m.apply(&Action::GestureBegin(cid.clone()));
        for v in values {
            m.apply(&Action::Set(cid.clone(), *v));
            m.pump();
        }
        m.apply(&Action::GestureEnd(cid));
    };
    drag(&mut m, id(&mic, Control::Fader), &[0.5, 0.55, 0.6]);
    m.apply(&Action::Toggle(id(&mic, Control::Mute)));
    drag(&mut m, id(&mic, Control::PanL), &[0.25]);
    m.apply(&Action::Toggle(id(&mic, preamp("48v"))));
    m.apply(&Action::Toggle(id(&line, preamp("48v")))); // Line mode: not offered, nothing sent
    drag(&mut m, id(&mic, preamp("preamp_gain")), &[1.0]);
    m.apply(&Action::Toggle(id(&mic, Control::RecMon)));
    m.apply(&Action::Toggle(id(&aux, Control::PrePost)));
    drag(&mut m, id(&aux, Control::SendLevel(1)), &[0.7]);
    m.apply(&Action::Toggle(id(&aux, Control::SendMute(0))));
    drag(&mut m, monitor.clone(), &[0.5]);
    m.apply(&Action::Toggle(ControlId {
        strip: Arc::from("monitor"),
        control: Control::MonitorMute,
    }));

    let i1 = "/devices/0/inputs/1";
    let a0 = "/devices/0/auxs/0";
    let o18 = "/devices/0/outputs/18";
    let step = |s: &str| {
        vec![
            "set /UndoRecording/value true".to_owned(),
            s.to_owned(),
            "set /UndoRecording/value false".to_owned(),
        ]
    };
    let want: Vec<String> = [
        step(&format!("set {i1}/FaderLevelTapered/value 0.6")),
        step(&format!("set {i1}/Mute/value false")),
        step(&format!("set {i1}/Pan/value -0.5")),
        step(&format!("set {i1}/preamps/0/48V/value true")),
        step(&format!("set {i1}/preamps/0/GainTapered/value 1")),
        step(&format!("set {i1}/RecordPreEffects/value true")),
        step(&format!("set {a0}/SendPostFader/value false")),
        step(&format!("set {a0}/sends/1/GainTapered/value 0.7")),
        step(&format!("set {a0}/sends/0/Bypass/value true")),
        step(&format!("set {o18}/CRMonitorLevelTapered/value 0.5")),
        step(&format!("set {o18}/Mute/value true")),
    ]
    .concat();
    assert_eq!(wait_sets(&server, want.len()), want);
    assert_eq!(
        server.value(&format!("{i1}/FaderLevelTapered")),
        Some(Value::Number(0.6))
    );
    // The engine's dB companion comes back and drives the readout.
    let deadline = Instant::now() + Duration::from_secs(5);
    while m.main(&mic).readout != "-10.0" {
        m.pump();
        assert!(
            Instant::now() < deadline,
            "readout {}",
            m.main(&mic).readout
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(m.main(&aux).pre);
    assert_eq!(
        m.monitor().readout,
        format!("{:.1} dB", ua_engine::tapers::monitor::db_from_tapered(0.5))
    ); // -13.5
}

/// `connect(_, false)`: the connection refuses writes; controls move locally only.
#[test]
fn read_only_connection_sends_nothing() {
    let tree = Snapshot::load(fixture()).unwrap().tree;
    let server = MockEngine::start(tree, MockOptions::default()).unwrap();
    let mut m = live(&server, false);
    let mic = m.inputs()[1].clone();
    m.apply(&Action::Toggle(id(&mic, Control::Solo)));
    m.apply(&Action::GestureBegin(id(&mic, Control::Fader)));
    m.apply(&Action::Set(id(&mic, Control::Fader), 0.2));
    m.apply(&Action::GestureEnd(id(&mic, Control::Fader)));
    std::thread::sleep(Duration::from_millis(100));
    m.pump();
    assert!(server.write_commands().is_empty());
    assert!(m.main(&mic).solo);
    assert_eq!(m.main(&mic).fader, 0.2);
    assert_eq!(m.store().writer().sent_count(), 0);
}

/// The UI-surface methods read the x4 capture (2 active cues, HP 1/2, monitor with dim).
#[test]
fn surface_data_from_the_capture() {
    let m = offline();
    let a1 = m.inputs()[0].clone();
    let sends = m.input_sends(&a1);
    let names: Vec<&str> = sends.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["AUX 1", "AUX 2", "CUE 1", "CUE 2"], "CueBusCount 2 hides CUE 3/4");
    assert!(m.send_pre(&a1, 0).is_some());
    assert_eq!(m.input_sources(&a1), ["LINE", "MIC", "DANTE"]);
    assert!(m.input_sources(&m.inputs()[3]).is_empty(), "no preamp, no picker");

    let st = m.status();
    assert_eq!(st.sample_rate, Some(48_000));
    assert_eq!(st.clock_source.as_deref(), Some("Internal"));
    assert_eq!(st.tempo, 120.0);
    let set = m.engine_settings();
    assert_eq!(set.sample_rates.len(), 6);
    assert_eq!(set.clock_sources, ["Internal", "S/PDIF"]);
    assert!(!set.writes_enabled);

    let cues = m.cue_outputs();
    assert_eq!(cues.len(), 2);
    assert_eq!(cues[0].name, "CUE 1");
    assert!(!cues[0].source_cue, "MixInSource mon");
    let hps = m.headphone_outputs();
    assert_eq!(hps[0].name, "HP 1");
    assert_eq!(hps[0].options[hps[0].source], "CUE 1");
    let mon = m.monitor_options();
    assert_eq!(mon.dim, Some(false));
    assert_eq!(mon.dim_readout, "-17 dB");
    assert_eq!(mon.talkback, Some(false));
}

/// Surface controls write the paths in ui-to-path.md (offline: local values move).
#[test]
fn surface_actions_move_local_values() {
    let mut m = offline();
    let a1 = m.inputs()[0].clone();
    let tap = |c: Control| Action::Tap(id(&a1, c));
    m.apply(&tap(Control::InputSource(1)));
    assert_eq!(m.preamp(&a1).unwrap().source, "MIC");
    m.apply(&Action::Toggle(id(&a1, Control::Isolate)));
    assert!(m.isolated(&a1));
    let pre = m.send_pre(&a1, 0).unwrap();
    m.apply(&Action::Toggle(id(&a1, Control::SendPre(0))));
    assert_eq!(m.send_pre(&a1, 0), Some(!pre));
    m.apply(&Action::Toggle(id(&a1, Control::SendMute(2))));
    assert!(m.input_sends(&a1)[2].muted);
    m.apply(&Action::Set(id(&a1, Control::SendLevel(1)), 0.5));
    assert_eq!(m.input_sends(&a1)[1].level, 0.5);

    m.set_text(&id(&a1, Control::Insert(0)), "UAD Pultec EQP-1A");
    // Loading writes EffectName only (as Console does); Power keeps the slot's value.
    let InsertState::Loaded { name, power } = m.inserts(&a1)[0].clone() else { panic!("not loaded") };
    assert_eq!(name, "UAD Pultec EQP-1A");
    // Offline nothing commits the load (the slot shows a load in flight, active); the engine's
    // commit pushes Power true, then the slot's power control bypasses it.
    assert!(power, "a load in flight is not drawn bypassed");
    m.store_mut().apply([("/devices/0/inputs/0/effects/0/Power".to_string(), ua_engine::Value::Bool(true))]);
    m.apply(&tap(Control::InsertPower(0)));
    assert!(matches!(m.inserts(&a1)[0], InsertState::Loaded { power: false, .. }));
    let empty = 1 + m.inserts(&a1)[1..].iter().position(|s| *s == InsertState::Empty).expect("an empty slot");
    m.apply(&tap(Control::InsertCopy(0)));
    m.apply(&tap(Control::InsertCopy(empty as u8)));
    // Copy / Paste are the engine's (`/CopyInsert`, `/PasteInsert`): offline nothing lands, the
    // clipboard keeps the name for `Paste "<name>"` (tests/inserts.rs checks the wire).
    assert_eq!(m.inserts(&a1)[empty], InsertState::Empty);
    assert_eq!(m.menu_state().clipboard.as_deref(), Some("UAD Pultec EQP-1A"));
    m.apply(&tap(Control::InsertRemove(0)));
    assert_eq!(m.inserts(&a1)[0], InsertState::Empty);
    assert!(m.take_taps().is_empty(), "surface taps are not plug-in opens");

    m.set_text(&id(&a1, Control::Name), "Vox");
    assert_eq!(m.main(&a1).name, "Vox");
    let g = |c: Control| ControlId { strip: "global".into(), control: c };
    m.apply(&Action::Tap(g(Control::SampleRate(3))));
    assert_eq!(m.status().sample_rate, Some(96_000));
    m.apply(&Action::Tap(g(Control::ClockSource(1))));
    assert_eq!(m.status().clock_source.as_deref(), Some("S/PDIF"));
    m.apply(&Action::Tap(g(Control::AllRecordPreEffects(true))));
    assert!(m.inputs().iter().all(|s| m.records_pre_effects(s)));
    let mon = |c: Control| ControlId { strip: "monitor".into(), control: c };
    m.apply(&Action::Toggle(mon(Control::MonitorDim)));
    assert_eq!(m.monitor_options().dim, Some(true));
    m.apply(&Action::Toggle(mon(Control::CueSource(0))));
    assert!(m.cue_outputs()[0].source_cue);
    m.apply(&Action::Tap(mon(Control::HeadphoneSource { output: 0, option: 0 })));
    assert_eq!(m.headphone_outputs()[0].options[m.headphone_outputs()[0].source], "MON");
    m.apply(&Action::Toggle(ControlId { strip: "settings".into(), control: Control::WritesEnabled }));
    assert!(m.engine_settings().writes_enabled);
}

/// Rename on a linked pair writes `StereoName` (the label) and leaves `Name`; a mono strip
/// writes `Name`. The pair's title is its `StereoName/default`.
#[test]
fn rename_of_a_stereo_pair_writes_stereo_name() {
    let mut m = offline();
    let pair = m.inputs().into_iter().find(|s| s.stereo).expect("a linked pair");
    assert_eq!(pair.id.as_ref(), "/devices/0/inputs/2");
    assert_eq!(pair.title, "ANALOG 3/4");
    m.set_text(&id(&pair, Control::Name), "Synth");
    assert_eq!(m.main(&pair).name, "Synth");
    let v = |p: &str| m.store().value(p).and_then(Value::as_str).map(str::to_owned);
    assert_eq!(v("/devices/0/inputs/2/StereoName").as_deref(), Some("Synth"));
    assert_eq!(v("/devices/0/inputs/2/Name").as_deref(), Some("ANALOG 3"));
    let mono = m.inputs()[1].clone();
    m.set_text(&id(&mono, Control::Name), "Bass");
    assert_eq!(m.store().value("/devices/0/inputs/1/Name").and_then(Value::as_str), Some("Bass"));
    assert_eq!(m.main(&mono).name, "Bass");
}

/// CTRL ROOM SOURCE CUE 3 writes `MixInSource` "cue3" (in the engine's list) and reads back.
#[test]
fn control_room_source_cue_3() {
    let mut m = offline();
    let mon = |c: Control| ControlId { strip: "monitor".into(), control: c };
    m.apply(&Action::Tap(mon(Control::CrSource(3))));
    assert_eq!(m.store().value("/devices/0/outputs/18/MixInSource").and_then(Value::as_str), Some("cue3"));
    assert_eq!(m.monitor_options().cr_source, 3);
    m.apply(&Action::Tap(mon(Control::CrSource(0))));
    assert_eq!(m.monitor_options().cr_source, 0);
}

/// Settings › ALT COUNT 1 enables one pair (both legs): the monitor column shows one ALT
/// button and one ALT TRIM knob; the knob writes `AltMonTrim` (±30 dB) on both legs.
#[test]
fn alt_count_counts_pairs_and_trims_write_alt_mon_trim() {
    let mut m = offline();
    let s = |c: Control| ControlId { strip: "settings".into(), control: c };
    let mon = |c: Control| ControlId { strip: "monitor".into(), control: c };
    m.apply(&Action::Tap(s(Control::SettingChoose("alt:/devices/0".into(), 1))));
    for o in [0, 1] {
        assert_eq!(m.store().value(&format!("/devices/0/outputs/{o}/AltMonEnabled")).and_then(Value::as_bool), Some(true));
    }
    let opts = m.monitor_options();
    assert_eq!(opts.alt_count, 1);
    assert_eq!(opts.alt_trim, [0.5, 0.5], "0 dB");
    m.apply(&Action::Set(mon(Control::AltTrim(0)), 0.75));
    for o in [0, 1] {
        assert_eq!(m.store().value(&format!("/devices/0/outputs/{o}/AltMonTrim")).and_then(Value::as_f64), Some(15.0));
    }
    assert_eq!(m.store().value("/devices/0/outputs/2/AltMonTrim").and_then(Value::as_f64), Some(0.0));
    assert_eq!(m.monitor_options().alt_trim[0], 0.75);
    m.apply(&Action::Tap(s(Control::SettingChoose("alt:/devices/0".into(), 2))));
    assert_eq!(m.monitor_options().alt_count, 2);
    m.apply(&Action::Set(mon(Control::AltTrim(1)), 0.0));
    assert_eq!(m.store().value("/devices/0/outputs/3/AltMonTrim").and_then(Value::as_f64), Some(-30.0));
    assert_eq!(m.monitor_options().alt_trim, [0.75, 0.0]);
}

/// `/plugins/P`: engine (numeric) order, every category, active vs. inactive and the badge of
/// inactive entries.
#[test]
fn plugin_catalog_maps_licence_states() {
    let plugin = |name: &str, cats: &str, auth: bool, demo_available: bool, extra: &str| {
        format!(
            r#"{{"properties":{{"Name":{{"type":"string","value":"{name}"}},"Categories":{{"type":"string","value":"{cats}"}},"Authorized":{{"type":"bool","value":{auth}}},"Demoing":{{"type":"bool","value":false}},"DemoAvailable":{{"type":"bool","value":{demo_available}}},"AuthHardware":{{"type":"bool","value":true}},"AuthUpdateRequired":{{"type":"bool","value":false}}{extra}}}}}"#
        )
    };
    let mut entries = vec![
        plugin("UAD A", "Delay,Modulation", true, false, ""),
        plugin("UAD B", "Equalization", false, true, ""),
        plugin("UAD C", "Equalization", false, false, ""),
        plugin("UAD D", "Delay", false, true, r#","AuthUpdateRequired":{"type":"bool","value":true}"#),
    ];
    for i in entries.len()..11 {
        entries.push(plugin(&format!("UAD P{i}"), "Delay", true, false, ""));
    }
    let children: Vec<String> = entries.iter().enumerate().map(|(i, e)| format!(r#""{i}":{e}"#)).collect();
    let json = format!(
        r#"{{"format":"consolelab-engine-snapshot","info":{{}},"tree":{{"children":{{"plugins":{{"children":{{{}}}}}}}}},"version":1}}"#,
        children.join(",")
    );
    let path = std::env::temp_dir().join(format!("engine-model-catalog-{}.json", std::process::id()));
    std::fs::write(&path, json).unwrap();
    let m = EngineMixer::from_snapshot(&path).expect("snapshot");
    std::fs::remove_file(&path).ok();
    let cat = m.plugin_catalog();
    let names: Vec<&str> = cat.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(&names[..5], ["UAD A", "UAD B", "UAD C", "UAD D", "UAD P4"]);
    assert_eq!(names[10], "UAD P10");
    assert_eq!(cat[0].categories, ["Delay", "Modulation"]);
    assert!(cat[0].in_category("Modulation") && cat[0].enabled && cat[0].badge.is_none());
    use rack::PluginBadge;
    assert_eq!((cat[1].enabled, cat[1].badge), (false, Some(PluginBadge::Demo)));
    assert_eq!((cat[2].enabled, cat[2].badge), (false, Some(PluginBadge::Buy)));
    assert_eq!((cat[3].enabled, cat[3].badge), (false, Some(PluginBadge::Activate)));
}

/// open-apollo's catalogue publishes no licence state (`Authorized` absent): every entry is
/// usable and draggable, with no badge.
#[test]
fn plugin_catalog_without_licence_state_is_enabled() {
    let json = r#"{"format":"consolelab-engine-snapshot","info":{},"tree":{"children":{"plugins":{"children":{"0":{"properties":{"Name":{"type":"string","value":"UAD A"},"Categories":{"type":"string","value":"Delay"}}}}}}},"version":1}"#;
    let path = std::env::temp_dir().join(format!("engine-model-catalog-nolic-{}.json", std::process::id()));
    std::fs::write(&path, json).unwrap();
    let m = EngineMixer::from_snapshot(&path).expect("snapshot");
    std::fs::remove_file(&path).ok();
    let cat = m.plugin_catalog();
    assert!(cat[0].enabled && cat[0].badge.is_none());
}

/// `/plugins/P/Unison` reaches the browser: assign mode for the Unison slot lists only the
/// Unison entries, a strip insert's lists them all.
#[test]
fn plugin_catalog_unison_flag_filters_the_unison_assign_browser() {
    let plugin = |name: &str, unison: &str| format!(r#"{{"properties":{{"Name":{{"type":"string","value":"{name}"}},"Categories":{{"type":"string","value":"Delay"}}{unison}}}}}"#);
    let entries = [
        plugin("UAD Comp", r#","Unison":{"type":"bool","value":false}"#),
        plugin("UAD Pre", r#","Unison":{"type":"bool","value":true}"#),
        plugin("UAD Unknown", ""),
    ];
    let children: Vec<String> = entries.iter().enumerate().map(|(i, e)| format!(r#""{i}":{e}"#)).collect();
    let json = format!(r#"{{"format":"consolelab-engine-snapshot","info":{{}},"tree":{{"children":{{"plugins":{{"children":{{{}}}}}}}}},"version":1}}"#, children.join(","));
    let path = std::env::temp_dir().join(format!("engine-model-catalog-unison-{}.json", std::process::id()));
    std::fs::write(&path, json).unwrap();
    let m = EngineMixer::from_snapshot(&path).expect("snapshot");
    std::fs::remove_file(&path).ok();
    let cat = m.plugin_catalog();
    assert_eq!(cat.iter().map(|p| p.unison).collect::<Vec<_>>(), [Some(false), Some(true), None]);
    assert_eq!(m.plugin_is_unison("UAD Comp"), Some(false));
    let mut view = rack::ViewState::default();
    view.begin_assign("/devices/0/inputs/0".into(), UNISON_SLOT);
    assert_eq!(rack::surfaces::browser::filtered(&cat, &m.plugin_categories(), &view), [1, 2], "unknown stays allowed");
    view.begin_assign("/devices/0/inputs/0".into(), 0);
    assert_eq!(rack::surfaces::browser::filtered(&cat, &m.plugin_categories(), &view), [0, 1, 2]);
}

/// Meters are drawn with the engine's release and peak hold and Console's easing, against the
/// frame clock: a step and a burst read the same whether the engine pushes at 50 Hz or 20 Hz and
/// whether we draw at 30, 60 or 120 Hz.
#[test]
fn meters_have_the_engine_ballistics_at_any_rate() {
    use rack::ballistics::decay;
    // `scales::meter_fraction` inverted by bisection (−60…0 dB).
    let db_of = |f: f64| {
        let (mut lo, mut hi) = (-60.0, 0.0);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            if scales::meter_fraction(mid) < f { lo = mid } else { hi = mid }
        }
        (lo + hi) / 2.0
    };
    // 0 dBFS for 1 s, then a −6 dB burst at 1.0…1.1 s over silence. Peak hold: 3 SEC (default).
    let signal = |t: f64| if t < 1.0 { 0.0 } else if t < 1.1 { -6.0 } else { -77.0 };
    let run = |push_hz: f64, fps: f64| {
        let m = offline();
        let monitor = m.store().topology().monitor.as_ref().expect("monitor").meters[0].base.clone();
        let (mut t, mut next) = (0.0f64, 0.0f64);
        let mut seen = Vec::new();
        for at in [0.9, 1.6, 2.6, 4.0, 4.6] {
            while t < at {
                if t >= next {
                    let db = signal(t) as f32;
                    m.store().meters().set(&monitor, MeterReading { level_db: db, peak_db: db, clip: false, updates: 1 });
                    next += 1.0 / push_hz;
                }
                m.set_meter_time(t);
                let _ = m.monitor_meters();
                t += 1.0 / fps;
            }
            m.set_meter_time(at);
            let f = m.monitor_meters();
            seen.push((db_of(f.left), db_of(f.peak_left)));
        }
        seen
    };
    let base = run(1000.0, 1000.0);
    // Full scale, then the engine's fall (about 1.3 s from 0 to −20 dB), not a drop to silence.
    assert!(base[0].0 > -0.01, "{base:?}");
    assert!((base[1].0 - decay(0.0, 0.6)).abs() < 1.0, "bar 0.6 s after the step: {base:?}");
    // The 0 dBFS peak is held 3 s after the level last reached it (1.0 s), then decays.
    assert!(base[1].1 > -0.01 && base[2].1 > -0.01, "held: {base:?}");
    assert!(base[3].1 > -0.5, "hold ends at 4.0 s: {base:?}");
    assert!((base[4].1 - decay(0.0, 0.6)).abs() < 0.5, "then falls at the engine rate: {base:?}");
    for (push_hz, fps) in [(50.0, 60.0), (20.0, 30.0), (32.6, 120.0)] {
        let s = run(push_hz, fps);
        for ((bl, bp), (l, p)) in base.iter().zip(&s) {
            assert!((bl - l).abs() < 1.0 && (bp - p).abs() < 1.0, "{push_hz} Hz / {fps} fps {s:?} vs {base:?}");
        }
    }
}
