//! `/ClockSourceListChanged` and `/SampleRateListChanged` pulses (true) make the client re-read
//! `/ClockSource/values` and `/SampleRate/values`; the status-bar RATE menu greys out the rates
//! the engine marks `enabled: false`.

use std::time::{Duration, Instant};

use engine_model::EngineMixer;
use rack::menu::PopupEntry;
use rack::model::MixerModel;
use rack::surfaces::popover::sample_rate_menu;
use serde_json::json;
use ua_engine::{MockEngine, MockOptions, Snapshot, Status};

#[test]
fn list_changed_pulses_reread_the_lists_and_grey_disabled_rates() {
    let p = format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"));
    let server = MockEngine::start(Snapshot::load(p).unwrap().tree, MockOptions::default()).unwrap();
    let mut m = EngineMixer::connect(server.endpoint(), false);
    let wait = |m: &mut EngineMixer, what: &str, ok: &dyn Fn(&EngineMixer) -> bool| {
        let end = Instant::now() + Duration::from_secs(10);
        while !ok(m) {
            assert!(Instant::now() < end, "timed out: {what}");
            m.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait(&mut m, "live", &|m| *m.store().status() == Status::Live);
    let enabled = |m: &EngineMixer| {
        sample_rate_menu(m)
            .entries
            .iter()
            .map(|e| match e {
                PopupEntry::Item(i) => i.enabled,
                _ => panic!("rate menu has only items"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(enabled(&m), [true; 6]);

    let rates = |last: bool| {
        json!([44100, 48000, 88200, 96000, 176400, 192000]
            .iter()
            .map(|r| json!({"value": r, "string": format!("{r} Hz"), "enabled": *r < 176400 || last}))
            .collect::<Vec<_>>())
    };
    server.set_values("/SampleRate", rates(false));
    server.set_values("/ClockSource", json!(["Internal", "S/PDIF", "ADAT"]));
    server.push("/ClockSourceListChanged", true);
    server.push("/SampleRateListChanged", true);
    wait(&mut m, "lists re-read", &|m| m.engine_settings().clock_sources.len() == 3 && enabled(m) == [true, true, true, true, false, false]);
    assert_eq!(m.engine_settings().sample_rates.len(), 6, "disabled rates stay listed");

    // A pulse back to false re-reads nothing; the next true does.
    server.set_values("/SampleRate", rates(true));
    server.push("/SampleRateListChanged", false);
    std::thread::sleep(Duration::from_millis(150));
    m.pump();
    assert_eq!(enabled(&m), [true, true, true, true, false, false]);
    server.push("/SampleRateListChanged", true);
    wait(&mut m, "rates enabled again", &|m| enabled(m) == [true; 6]);
}
