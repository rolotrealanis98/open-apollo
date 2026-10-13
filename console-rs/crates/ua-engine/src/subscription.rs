//! The `subscribe {"paths":[…]}` batches Console sends, one per strip, in its order
//! (control-tree.md "What Console subscribes per object"). Paths absent from the tree are left
//! out (Console also asks for `Gain`, `StickyDanteNodeIOType` and others the macOS engine does
//! not list).

use crate::topology::{InsertPaths, MeterPaths, Topology};
use crate::tree::Tree;
use crate::value::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub label: String,
    /// `<property>/value` paths.
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SubscriptionPlan {
    pub batches: Vec<Batch>,
}

const INPUT_PROPS: [&str; 33] = [
    "Stereo",
    "Active",
    "IOType",
    "Isolate",
    "Solo",
    "SoloSafe",
    "Mute",
    "RecordPreEffects",
    "SelectedOnFront",
    "SRConvert",
    "Name",
    "StereoName",
    "DelayCompGroup",
    "Latency",
    "GainTapered",
    "Gain",
    "FaderLevelTapered",
    "FaderLevel",
    "Preset",
    "Pan",
    "Pan2",
    "TalkbackEnabled",
    "ChannelHidden",
    "EnabledByUser",
    "LLMDiskEnabled",
    "LLMDiskChannel",
    "LLMDiskSwitch",
    "LLMDiskMix",
    "LLMDiskChannelR",
    "StickyDanteNodeIOType",
    // In the engine 12 tree but not in our first plan (protocol/v131/MAP.md item 9).
    "LLMDiskTrimLevelTapered",
    "LLMDiskTrimLevel",
    "AdatPreampState",
];
const PREAMP_PROPS: [&str; 9] = [
    "GainTapered",
    "Gain",
    "PreampGainCustomDisplayText",
    "HiZ",
    "48V",
    "Pad",
    "LowCut",
    "Phase",
    "PreampInputKnobSteps",
];
const SEND_PROPS: [&str; 7] = [
    "Bypass",
    "Name",
    "ID",
    "GainTapered",
    "Gain",
    "Pan",
    "PostFaderOverride",
];
const AUX_PROPS: [&str; 17] = [
    "Name",
    "Active",
    "FaderLevelTapered",
    "FaderLevel",
    "Mute",
    "Preset",
    "MixToMono",
    "SendPostFader",
    "LLMDiskEnabled",
    "LLMDiskChannel",
    "LLMDiskChannelR",
    "EnabledByUser",
    "GainTapered",
    "Gain",
    "Isolate",
    "LLMDiskTrimLevelTapered",
    "LLMDiskTrimLevel",
];
/// The plug-in catalogue properties Console subscribes for every `/plugins/N` (1.2.x traces:
/// 1,540 subscriptions on an x4), so licence, demo and status changes reach the browser.
const PLUGIN_PROPS: [&str; 13] = [
    "Name",
    "Status",
    "RemoteGUI",
    "Authorized",
    "AuthHardware",
    "AuthUpdateRequired",
    "Categories",
    "DefaultPreset",
    "DemoAvailable",
    "DemoDaysRemaining",
    "Demoing",
    "Unison",
    "VstID",
];
const TALKBACK_ROOT: [&str; 3] = ["/TalkbackOn", "/TalkbackInPhysicalCR", "/TalkbackMaster"];

impl SubscriptionPlan {
    /// Subscribe all units, but shared root/catalogue/route properties once.
    pub fn for_devices(topologies: &[Topology], tree: &Tree) -> Self {
        let mut batches = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for topology in topologies {
            for mut batch in Self::new(topology, tree).batches {
                batch.paths.retain(|p| seen.insert(p.clone()));
                if topologies.len() > 1 {
                    batch.label = format!("device {} / {}", topology.device, batch.label);
                }
                if !batch.paths.is_empty() { batches.push(batch); }
            }
        }
        if topologies.is_empty() { return Self::new(&Topology::default(), tree); }
        Self { batches }
    }
    pub fn new(topology: &Topology, tree: &Tree) -> SubscriptionPlan {
        let value = |property: &str| {
            tree.has_property(property)
                .then(|| format!("{property}/value"))
        };
        let mut batches = Vec::new();
        let dev = format!("/devices/{}", topology.device);

        // Every input node (right legs and inactive inputs too: pairing and activity change live).
        for n in tree.indices(&format!("{dev}/inputs")) {
            let base = format!("{dev}/inputs/{n}");
            // N/A inputs have a null IOType.
            if tree
                .value(&format!("{base}/IOType"))
                .and_then(Value::as_str)
                .is_none()
            {
                continue;
            }
            let stereo = topology.input(n).is_some_and(|s| s.is_stereo());
            let mut p: Vec<String> = INPUT_PROPS
                .iter()
                .filter_map(|k| value(&format!("{base}/{k}")))
                .collect();
            if tree.has_node(&format!("{base}/preamps/0")) {
                p.extend(
                    PREAMP_PROPS
                        .iter()
                        .filter_map(|k| value(&format!("{base}/preamps/0/{k}"))),
                );
            }
            p.extend(value(&format!("{base}/CustomIntegratedPreampInputMeter")));
            let sends = tree.children(&format!("{base}/sends"));
            let meter_ks: &[&str] = if stereo { &["0", "1"] } else { &["0"] };
            for s in sends {
                for k in meter_ks {
                    p.extend(
                        MeterPaths::new(format!("{base}/sends/{s}/meters/{k}"))
                            .all()
                            .iter()
                            .filter_map(|m| value(m)),
                    );
                }
            }
            for k in meter_ks {
                p.extend(
                    MeterPaths::new(format!("{base}/meters/{k}"))
                        .all()
                        .iter()
                        .filter_map(|m| value(m)),
                );
            }
            for e in tree.children(&format!("{base}/effects")) {
                p.extend(
                    InsertPaths::SUBSCRIBED
                        .iter()
                        .filter_map(|k| value(&format!("{base}/effects/{e}/{k}"))),
                );
            }
            if tree.has_node(&format!("{base}/preamps/0/effects/0")) {
                p.extend(
                    InsertPaths::SUBSCRIBED
                        .iter()
                        .filter_map(|k| value(&format!("{base}/preamps/0/effects/0/{k}"))),
                );
            }
            for s in sends {
                p.extend(
                    SEND_PROPS
                        .iter()
                        .filter_map(|k| value(&format!("{base}/sends/{s}/{k}"))),
                );
            }
            batches.push(Batch {
                label: format!("input {n}"),
                paths: p,
            });
        }

        for n in tree.indices(&format!("{dev}/outputs")) {
            let base = format!("{dev}/outputs/{n}");
            let mut p: Vec<String> = tree
                .node(&base)
                .map(|node| {
                    node.properties
                        .iter()
                        .filter(|k| *k != "Type")
                        .filter_map(|k| value(&format!("{base}/{k}")))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(m) = topology.monitor.as_ref().filter(|m| m.index == n) {
                for meter in &m.meters {
                    p.extend(meter.all().iter().filter_map(|x| value(x)));
                }
            }
            batches.push(Batch {
                label: format!("output {n}"),
                paths: p,
            });
        }

        for aux in &topology.auxes {
            let mut p: Vec<String> = AUX_PROPS
                .iter()
                .filter_map(|k| value(&format!("{}/{k}", aux.base)))
                .collect();
            for meter in &aux.meters {
                p.extend(meter.all().iter().filter_map(|x| value(x)));
            }
            for s in &aux.sends {
                p.extend(
                    SEND_PROPS
                        .iter()
                        .filter_map(|k| value(&format!("{}/{k}", s.base))),
                );
                for meter in &s.meters {
                    p.extend(meter.all().iter().filter_map(|x| value(x)));
                }
            }
            for e in &aux.inserts {
                p.extend(
                    InsertPaths::SUBSCRIBED
                        .iter()
                        .filter_map(|k| value(&format!("{}/{k}", e.base))),
                );
            }
            batches.push(Batch {
                label: format!("aux {}", aux.index),
                paths: p,
            });
        }

        // Global: root and device properties (includes /MeterPulse, /UndoRecording, talkback).
        let mut g: Vec<String> = tree
            .node("/")
            .map(|node| {
                node.properties
                    .iter()
                    .filter(|k| *k != "Type")
                    .filter_map(|k| value(&format!("/{k}")))
                    .collect()
            })
            .unwrap_or_default();
        for t in TALKBACK_ROOT {
            if let Some(v) = value(t)
                && !g.contains(&v)
            {
                g.push(v);
            }
        }
        if let Some(node) = tree.node(&dev) {
            g.extend(
                node.properties
                    .iter()
                    .filter(|k| *k != "Type")
                    .filter_map(|k| value(&format!("{dev}/{k}"))),
            );
        }
        batches.push(Batch {
            label: "global".into(),
            paths: g,
        });

        // Driver routes (the I/O MATRIX): Console subscribes `/inputroutes` and `/outputroutes`
        // recursively; here every route property.
        let mut r = Vec::new();
        for node in ["/inputroutes", "/outputroutes"] {
            for n in tree.indices(node) {
                let base = format!("{node}/{n}");
                if let Some(nd) = tree.node(&base) {
                    r.extend(nd.properties.iter().filter(|k| *k != "Type").filter_map(|k| value(&format!("{base}/{k}"))));
                }
            }
        }
        batches.push(Batch { label: "routes".into(), paths: r });

        // Plug-in scenes: the recalled index and every scene's name and tempo.
        let mut sc: Vec<String> = ["RecalledSceneIndex"].iter().filter_map(|k| value(&format!("/pluginscenes/0/{k}"))).collect();
        for n in tree.indices("/pluginscenes/0/scenes") {
            sc.extend(["SceneName", "SceneTempo"].iter().filter_map(|k| value(&format!("/pluginscenes/0/scenes/{n}/{k}"))));
        }
        batches.push(Batch { label: "scenes".into(), paths: sc });

        // The plug-in catalogue.
        let mut pl = Vec::new();
        for n in tree.indices("/plugins") {
            pl.extend(PLUGIN_PROPS.iter().filter_map(|k| value(&format!("/plugins/{n}/{k}"))));
        }
        batches.push(Batch { label: "plugins".into(), paths: pl });

        batches.retain(|b| !b.paths.is_empty());
        SubscriptionPlan { batches }
    }

    pub fn all_paths(&self) -> impl Iterator<Item = &str> {
        self.batches
            .iter()
            .flat_map(|b| b.paths.iter().map(String::as_str))
    }

    pub fn path_count(&self) -> usize {
        self.batches.iter().map(|b| b.paths.len()).sum()
    }
}
