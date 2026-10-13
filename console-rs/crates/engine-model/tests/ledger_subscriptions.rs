//! Ledger: what UAD Console subscribes to vs our subscription plan.
//!
//! `fixtures/console-trace/subscriptions.txt` is every property UAD Console 1.2.x subscribed in
//! the open-apollo daemon traces (`tools/ledger/extract_traces.py`). Paths are compared as
//! patterns (numeric segments → `#`), because the traced daemon tree and the x4 12.0.0 fixture
//! differ in counts. The report goes to `$LEDGER_OUT/subscriptions.json` (default
//! `target/ledger`); the test ratchets the number of Console patterns we do not subscribe.

use std::collections::BTreeSet;

use engine_model::EngineMixer;
use ua_engine::SubscriptionPlan;

fn fixture() -> String {
    format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR"))
}

/// `/devices/0/inputs/3/sends/1/Gain` → `/devices/#/inputs/#/sends/#/Gain`.
pub fn pattern(path: &str) -> String {
    let p = path.split('?').next().unwrap_or(path);
    let p = p.strip_suffix("/value").unwrap_or(p);
    p.split('/').map(|seg| if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) { "#" } else { seg }).collect::<Vec<_>>().join("/")
}

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("LEDGER_OUT").map(Into::into).unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ledger"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Console patterns we do not subscribe today (ratchet: lower it when the plan grows).
const MAX_MISSING: usize = 0;

#[test]
fn console_subscriptions_vs_ours() {
    let console: BTreeSet<String> = include_str!("fixtures/console-trace/subscriptions.txt").lines().filter(|l| !l.is_empty()).map(pattern).collect();
    let m = EngineMixer::from_snapshot(fixture()).expect("fixture");
    let plan = SubscriptionPlan::new(m.store().topology(), m.store().tree());
    let ours: BTreeSet<String> = plan.batches.iter().flat_map(|b| b.paths.iter()).map(|p| pattern(p)).collect();
    // Patterns the fixture tree has at all (Console asks for some the 12.0.0 engine lacks).
    let tree: BTreeSet<String> = m.store().tree().properties().map(|(p, _)| pattern(p)).collect();
    let missing: Vec<&String> = console.difference(&ours).collect();
    let missing_in_tree: Vec<&&String> = missing.iter().filter(|p| tree.contains(**p)).collect();
    let extra: Vec<&String> = ours.difference(&console).collect();
    let report = serde_json::json!({
        "console_patterns": console.len(),
        "our_patterns": ours.len(),
        "covered": console.intersection(&ours).count(),
        "missing": missing,
        "missing_but_in_x4_12_tree": missing_in_tree,
        "extra": extra,
        "ours": ours,
        "console": console,
    });
    std::fs::write(out_dir().join("subscriptions.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!(
        "subscriptions: Console {} patterns, ours {}, covered {}, missing {} ({} exist in the x4 12.0.0 tree), extra {}",
        console.len(),
        ours.len(),
        console.intersection(&ours).count(),
        missing.len(),
        missing_in_tree.len(),
        extra.len()
    );
    assert!(missing_in_tree.len() <= MAX_MISSING, "{} Console subscriptions present in the tree are not in our plan (max {MAX_MISSING})", missing_in_tree.len());
}
