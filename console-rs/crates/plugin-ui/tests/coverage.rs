//! Spec-contract coverage: every key the catalog's specs carry must land in a typed field of
//! [`plugin_ui::PluginSpec`] (PIPELINE.md's base schema and all its runtime additions).
//!
//! Each `PluginSpec_*.json` of the found spec folder is decoded, encoded again (every spec type
//! is `Serialize`) and compared with the original JSON: a key the round trip drops is a key the
//! runtime does not read. Only keys that are not part of the contract may be dropped:
//! informational metadata the Swift runtime ignores too.
//!
//! Skipped (with a note) when no spec folder is found (`$OA_PLUGIN_SPECS`, …).

use std::collections::BTreeMap;

use plugin_ui::{PluginSpec, PluginSpecLibrary};
use serde_json::Value;

/// Keys outside the contract, by normalised path (`*` = any map key, `[]` = any list item).
const IGNORED: &[(&str, &str)] = &[
    (".provenance", "where the spec came from (tools, agents)"),
    (".notes", "integrator / agent notes"),
    ("._images", "the generator's sprite inventory"),
    (".meters", "the generator's list of LUNA meter ids; the runtime reads meterSources"),
    (".meterSources.*.provenance", "where a calibration came from"),
];

/// Top-level objects whose keys are names (meter ids, state names), not fields.
const MAPS: &[&str] = &[".meterSources", ".states"];

fn walk(orig: &Value, round: &Value, path: &str, missing: &mut Vec<String>) {
    match (orig, round) {
        (Value::Object(o), Value::Object(r)) => {
            let is_map = MAPS.contains(&path);
            for (k, v) in o {
                if v.is_null() {
                    continue; // an explicit null is an absent optional
                }
                let p = if is_map { format!("{path}.*") } else { format!("{path}.{k}") };
                if IGNORED.iter().any(|(i, _)| *i == p) {
                    continue;
                }
                match r.get(k) {
                    Some(rv) => walk(v, rv, &p, missing),
                    None => missing.push(p),
                }
            }
        }
        (Value::Array(o), Value::Array(r)) => {
            for (a, b) in o.iter().zip(r) {
                walk(a, b, &format!("{path}[]"), missing);
            }
        }
        _ => {}
    }
}

/// Keys of `text` the typed round trip drops, by normalised path.
pub fn dropped_keys(text: &str) -> Result<Vec<String>, String> {
    let orig: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let spec = PluginSpec::from_json(text).map_err(|e| e.to_string())?;
    let round = serde_json::to_value(&spec).map_err(|e| e.to_string())?;
    let mut missing = Vec::new();
    walk(&orig, &round, "", &mut missing);
    Ok(missing)
}

#[test]
fn inline_round_trip_reports_unknown_keys() {
    let text = r#"{"schema": 1, "effectName": "UAD X", "namePattern": "X", "size": {"w": 1, "h": 1},
        "provenance": "tool", "parameters": [], "meterSources": {"m": {"calibration": [], "provenance": "B", "smoothing": 0}},
        "elements": [{"id": "e", "kind": "text", "x": 0, "y": 0, "param": null, "text": {"string": "hi", "sparkle": true},
                      "graph": {"type": "eq", "bands": [{"id": "b", "gain": {"min": 3, "param": 0}}]}}]}"#;
    let mut missing = dropped_keys(text).unwrap();
    missing.sort();
    assert_eq!(missing, vec![".elements[].graph.bands[].gain.min".to_string(), ".elements[].text.sparkle".to_string()], "unknown keys and Swift's dropped scalar min are reported; ignored metadata and nulls are not");
}

#[test]
fn every_contract_key_in_the_catalog_is_decoded() {
    let Some(lib) = PluginSpecLibrary::locate() else {
        eprintln!("skipped: no plug-in spec folder (set OA_PLUGIN_SPECS)");
        return;
    };
    let mut dropped: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut failures = Vec::new();
    for entry in &lib.entries {
        let text = std::fs::read_to_string(&entry.file).unwrap();
        match dropped_keys(&text) {
            Ok(keys) => {
                for k in keys {
                    let slot = dropped.entry(k).or_insert((0, entry.stem().to_string()));
                    slot.0 += 1;
                }
            }
            Err(e) => failures.push(format!("{}: {e}", entry.stem())),
        }
    }
    assert!(failures.is_empty(), "specs that do not decode: {failures:#?}");
    let report: Vec<String> = dropped.iter().map(|(k, (n, ex))| format!("{k} ×{n} (e.g. {ex})")).collect();
    assert!(dropped.is_empty(), "{} keys of {} specs are not decoded into typed fields:\n{}", dropped.len(), lib.entries.len(), report.join("\n"));
    eprintln!("{} specs: every contract key decoded (ignored metadata: {:?})", lib.entries.len(), IGNORED.iter().map(|(k, _)| *k).collect::<Vec<_>>());
}
