mod common;

use common::{device_map, live_x4, tree};
use ua_engine::{InputKind, PluginParameter, SubscriptionPlan, Topology, Value};

/// The live capture: Apollo x4 on engine 12.0.0 with renamed strips and four stereo pairs
/// (inputs 2/3, 12/13, 14/15 and 16/17).
#[test]
fn live_x4_topology() {
    let tree = live_x4();
    let t = Topology::from_tree(&tree);
    assert_eq!(t.device_name.as_deref(), Some("Apollo x4"));
    assert_eq!(
        t.inputs.iter().map(|s| s.index).collect::<Vec<_>>(),
        [0, 1, 2, 12, 14, 16]
    );
    assert_eq!(
        t.inputs.iter().map(|s| s.is_stereo()).collect::<Vec<_>>(),
        [false, false, true, true, true, true]
    );
    assert_eq!(
        t.inputs.iter().map(|s| s.right_index).collect::<Vec<_>>(),
        [None, None, Some(3), Some(13), Some(15), Some(17)]
    );
    // A pair shows StereoName; a mono strip shows Name (even if StereoName was edited).
    assert_eq!(t.inputs[0].name, "Vocal Mic");
    assert_eq!(
        tree.value("/devices/0/inputs/0/StereoName"),
        Some(&Value::from("Talk HP"))
    );
    assert_eq!(
        Some(&Value::from(t.inputs[2].name.as_str())),
        tree.value("/devices/0/inputs/2/StereoName")
    );
    assert_eq!(t.inputs[0].default_name.as_deref(), Some("ANALOG 1"));
    assert_eq!(t.inputs[0].display_name_path(), "/devices/0/inputs/0/Name");
    assert_eq!(
        t.inputs[2].display_name_path(),
        "/devices/0/inputs/2/StereoName"
    );
    assert_eq!(t.all_inputs.len(), 24 - 4 - 1); // 4 right legs folded, talkback split out
    assert_eq!(t.talkback.strip.as_ref().map(|s| s.index), Some(22));
    assert_eq!(t.talkback.on(), "/TalkbackOn");
    assert!(tree.has_property(&t.talkback.on()));

    let a1 = &t.inputs[0];
    assert_eq!(a1.kind, InputKind::Analog);
    let pre = a1.preamp.as_ref().unwrap();
    assert!(pre.has_hi_z);
    assert_eq!(pre.phantom(), "/devices/0/inputs/0/preamps/0/48V");
    assert_eq!(
        pre.unison.effect_name(),
        "/devices/0/inputs/0/preamps/0/effects/0/EffectName"
    );
    assert_eq!(a1.io_type_choices, ["Line", "Mic", "Dante"]);
    assert_eq!(a1.inserts.len(), 8);
    assert_eq!(
        a1.sends.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["aux1", "aux2", "cue1", "cue2", "cue3", "cue4"]
    );
    assert_eq!(a1.send("cue2").unwrap().bus_number(), Some(2));
    // CueBusCount is 2: cue3/cue4 sends are hidden.
    assert_eq!(t.cue_bus_count, 2);
    assert_eq!(
        a1.sends.iter().map(|s| s.is_available).collect::<Vec<_>>(),
        [true, true, true, true, false, false]
    );
    assert_eq!(t.inputs[1].preamp.as_ref().map(|p| p.has_hi_z), Some(true));
    assert_eq!(t.inputs[2].preamp.as_ref().map(|p| p.has_hi_z), Some(false));
    assert!(t.inputs[3].preamp.is_none());
    assert_eq!(t.inputs[3].kind, InputKind::Spdif);
    assert_eq!(t.inputs[4].kind, InputKind::Virtual);
    assert_eq!(a1.meters.len(), 1);
    assert_eq!(
        t.inputs[2]
            .meters
            .iter()
            .map(|m| m.base.as_str())
            .collect::<Vec<_>>(),
        [
            "/devices/0/inputs/2/meters/0",
            "/devices/0/inputs/2/meters/1"
        ]
    );
    assert_eq!(
        a1.preamp_meter.as_deref(),
        Some("/devices/0/inputs/0/CustomIntegratedPreampInputMeter")
    );

    assert_eq!(
        t.auxes.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["AUX 1", "AUX 2"]
    );
    assert_eq!(
        t.auxes[0]
            .sends
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["cue1", "cue2", "cue3", "cue4"]
    );
    assert_eq!(t.auxes[0].inserts.len(), 4);
    assert_eq!(t.monitor.as_ref().map(|m| m.index), Some(18));
    assert_eq!(
        t.cues.iter().map(|c| c.index).collect::<Vec<_>>(),
        [14, 15, 16, 17]
    );
    assert_eq!(
        t.cues.iter().map(|c| c.is_active).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    assert_eq!(
        t.headphones.iter().map(|h| h.index).collect::<Vec<_>>(),
        [19, 20]
    );
    assert!(t.headphones.iter().all(|h| h.level.is_none()));
}

/// Every path the topology hands out exists in the tree (no invented names).
#[test]
fn topology_paths_exist() {
    for source in ["live-x4", "x4", "x8p"] {
        let tree = tree(source);
        let t = Topology::from_tree(&tree);
        let mut paths: Vec<String> = Vec::new();
        for s in t.all_inputs.iter().chain(t.talkback.strip.iter()) {
            paths.extend([
                s.fader(),
                s.fader_db(),
                s.pan(),
                s.pan2(),
                s.solo(),
                s.mute(),
                s.solo_safe(),
                s.isolate(),
                s.stereo(),
                s.io_type_path(),
            ]);
            paths.extend([
                s.name_path(),
                s.stereo_name_path(),
                s.record_pre_effects(),
                s.channel_hidden(),
                s.talkback_enabled(),
                s.preset(),
            ]);
            paths.extend(s.meters.iter().flat_map(|m| m.all()));
            for send in &s.sends {
                paths.extend([
                    send.gain(),
                    send.gain_db(),
                    send.bypass(),
                    send.pan(),
                    send.post_fader_override(),
                ]);
                paths.extend(send.meters.iter().flat_map(|m| m.all()));
            }
            for ins in &s.inserts {
                paths.extend([ins.effect_name(), ins.power(), ins.preset(), ins.status()]);
            }
            if let Some(p) = &s.preamp {
                paths.extend([
                    p.gain(),
                    p.gain_db(),
                    p.phantom(),
                    p.pad(),
                    p.low_cut(),
                    p.phase(),
                    p.unison.effect_name(),
                ]);
                if p.has_hi_z {
                    paths.push(p.hi_z());
                }
            }
            paths.extend(s.preamp_meter.clone());
        }
        for a in &t.auxes {
            paths.extend([
                a.fader(),
                a.fader_db(),
                a.mute(),
                a.mono(),
                a.send_post_fader(),
            ]);
            paths.extend(a.meters.iter().flat_map(|m| m.all()));
            for send in &a.sends {
                paths.extend([
                    send.gain(),
                    send.gain_db(),
                    send.bypass(),
                    send.post_fader_override(),
                ]);
            }
        }
        let m = t.monitor.as_ref().expect("monitor");
        paths.extend([
            m.level(),
            m.level_db(),
            m.mute(),
            m.dim(),
            m.dim_attenuation(),
            m.mono(),
            m.source(),
            m.mirrors_to_digital(),
            m.alt_mon_selection(),
        ]);
        paths.extend(m.meters.iter().flat_map(|x| x.all()));
        for c in &t.cues {
            paths.extend([c.mix_in_source(), c.mono(), c.output_destination()]);
        }
        for h in &t.headphones {
            paths.push(h.mix_in_source());
        }
        paths.extend([
            t.talkback.on(),
            t.talkback.in_physical_cr(),
            t.talkback.master(),
            t.talkback.device_master(),
        ]);
        let missing: Vec<&String> = paths
            .iter()
            .filter(|p| !tree.has_property(p))
            .take(10)
            .collect();
        assert!(missing.is_empty(), "{source}: missing {missing:?}");
        // Names the old ConsoleLink client used do not exist.
        assert!(!tree.has_property("/devices/0/inputs/0/sends/0/Mute"));
        assert!(!tree.has_property("/devices/0/inputs/0/meters/0/Clip"));
        assert!(!tree.has_property("/devices/0/outputs/18/TalkbackOn"));
    }
}

#[test]
fn x4_device_map_topology() {
    let t = Topology::from_tree(&device_map("x4"));
    assert_eq!(t.device_name.as_deref(), Some("Apollo x4"));
    // Factory layout: four analog monos, S/PDIF pair, four active virtual monos.
    assert_eq!(
        t.inputs.iter().map(|s| s.index).collect::<Vec<_>>(),
        [0, 1, 2, 3, 12, 14, 15, 16, 17]
    );
    assert_eq!(
        t.inputs
            .iter()
            .find(|s| s.index == 12)
            .map(|s| s.is_stereo()),
        Some(true)
    );
    assert_eq!(t.inputs.iter().filter(|s| s.preamp.is_some()).count(), 4);
    assert_eq!(t.monitor.as_ref().map(|m| m.index), Some(18));
    assert_eq!(
        t.cues.iter().map(|c| c.index).collect::<Vec<_>>(),
        [14, 15, 16, 17]
    );
    assert_eq!(
        t.headphones.iter().map(|h| h.index).collect::<Vec<_>>(),
        [19, 20]
    );
    assert_eq!(
        t.talkback.strip.as_ref().map(|s| (s.index, s.is_active)),
        Some((22, false))
    );
}

/// x8p output indices differ from x4: found by property and IOType, not hard-coded.
#[test]
fn x8p_topology() {
    let t = Topology::from_tree(&device_map("x8p"));
    assert_eq!(t.device_name.as_deref(), Some("Apollo x8p"));
    assert_eq!(t.inputs.len(), 20); // 8 analog + 8 ADAT + 4 virtual; S/PDIF pair inactive
    assert_eq!(
        t.inputs
            .iter()
            .filter(|s| s.kind == InputKind::Analog)
            .count(),
        8
    );
    assert_eq!(
        t.inputs
            .iter()
            .filter(|s| s.kind == InputKind::Adat)
            .count(),
        8
    );
    assert_eq!(
        t.inputs
            .iter()
            .filter(|s| s.preamp.as_ref().is_some_and(|p| p.has_hi_z))
            .map(|s| s.index)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    let spdif = t.all_inputs.iter().find(|s| s.index == 16).unwrap();
    assert!(spdif.is_stereo() && !spdif.is_visible());
    assert_eq!(t.monitor.as_ref().map(|m| m.index), Some(22));
    assert_eq!(
        t.monitor.as_ref().unwrap().level(),
        "/devices/0/outputs/22/CRMonitorLevelTapered"
    );
    assert_eq!(
        t.cues.iter().map(|c| c.index).collect::<Vec<_>>(),
        [18, 19, 20, 21]
    );
    assert_eq!(
        t.headphones.iter().map(|h| h.index).collect::<Vec<_>>(),
        [23, 24]
    );
    assert_eq!(
        t.talkback.strip.as_ref().map(|s| (s.index, s.is_active)),
        Some((26, true))
    );
    assert_eq!(t.auxes.len(), 2);
}

#[test]
fn stereo_link_folds_pairs() {
    let mut tree = device_map("x4");
    assert!(
        Topology::from_tree(&tree)
            .inputs
            .iter()
            .any(|s| s.index == 1)
    );
    tree.set_value("/devices/0/inputs/0/Stereo", Value::Bool(true));
    tree.set_value("/devices/0/inputs/1/Stereo", Value::Bool(true));
    let t = Topology::from_tree(&tree);
    let pair = &t.inputs[0];
    assert_eq!((pair.index, pair.right_index), (0, Some(1)));
    assert_eq!(pair.name, "ANALOG 1/2");
    assert_eq!(
        pair.meters
            .iter()
            .map(|m| m.base.as_str())
            .collect::<Vec<_>>(),
        [
            "/devices/0/inputs/0/meters/0",
            "/devices/0/inputs/0/meters/1"
        ]
    );
    assert_eq!(pair.pan2(), "/devices/0/inputs/0/Pan2");
    assert!(!t.inputs.iter().any(|s| s.index == 1));
}

/// The leader's `Stereo` alone folds the pair (no one-frame duplicate of the right leg while
/// the daemon has not pushed its flag yet); the title is the pair's `StereoName/default`.
#[test]
fn leader_stereo_alone_folds_the_pair() {
    let mut tree = device_map("x4");
    tree.set_value("/devices/0/inputs/0/Stereo", Value::Bool(true));
    let t = Topology::from_tree(&tree);
    assert_eq!((t.inputs[0].index, t.inputs[0].right_index), (0, Some(1)));
    assert!(!t.inputs.iter().any(|s| s.index == 1));
    // Only the right leg linked: nothing folds.
    let mut tree = device_map("x4");
    tree.set_value("/devices/0/inputs/1/Stereo", Value::Bool(true));
    let t = Topology::from_tree(&tree);
    assert_eq!(t.inputs[0].right_index, None);
    assert!(t.inputs.iter().any(|s| s.index == 1));
    // A pair is titled by StereoName/default; a mono strip by Name/default.
    let mut tree = device_map("x4");
    tree.set_value("/devices/0/inputs/0/Stereo", Value::Bool(true));
    let json = |v: &str, d: &str| serde_json::json!({"type": "string", "value": v, "default": d});
    tree.set_property("/devices/0/inputs/0/Name", ua_engine::Property::from_json(&json("ANALOG 1", "ANALOG 1")));
    tree.set_property("/devices/0/inputs/0/StereoName", ua_engine::Property::from_json(&json("Keys", "ANALOG 1/2")));
    let t = Topology::from_tree(&tree);
    assert_eq!(t.inputs[0].default_name.as_deref(), Some("ANALOG 1/2"));
    assert_eq!(t.inputs[0].name, "Keys");
    tree.set_value("/devices/0/inputs/0/Stereo", Value::Bool(false));
    assert_eq!(Topology::from_tree(&tree).inputs[0].default_name.as_deref(), Some("ANALOG 1"));
}

#[test]
fn subscription_plan_follows_console() {
    let tree = live_x4();
    let t = Topology::from_tree(&tree);
    let plan = SubscriptionPlan::new(&t, &tree);
    let input0 = plan.batches.iter().find(|b| b.label == "input 0").unwrap();
    // Console's per-strip batch is ~203 paths for an analog input.
    assert!(
        (180..=230).contains(&input0.paths.len()),
        "{}",
        input0.paths.len()
    );
    assert_eq!(input0.paths[0], "/devices/0/inputs/0/Stereo/value");
    for p in [
        "/devices/0/inputs/0/preamps/0/48V/value",
        "/devices/0/inputs/0/sends/0/Bypass/value",
        "/devices/0/inputs/0/meters/0/MeterClip/value",
    ] {
        assert!(input0.paths.iter().any(|x| x == p), "{p}");
    }
    assert!(
        !input0
            .paths
            .iter()
            .any(|x| x == "/devices/0/inputs/0/meters/1/MeterLevel/value")
    ); // mono
    let input2 = plan.batches.iter().find(|b| b.label == "input 2").unwrap();
    assert!(
        input2
            .paths
            .iter()
            .any(|x| x == "/devices/0/inputs/2/meters/1/MeterLevel/value")
    ); // stereo
    assert!(
        input2
            .paths
            .iter()
            .any(|x| x == "/devices/0/inputs/2/sends/0/meters/1/MeterLevel/value")
    );
    let global = plan.batches.iter().find(|b| b.label == "global").unwrap();
    assert!(global.paths.iter().any(|x| x == "/TalkbackOn/value"));
    assert!(global.paths.iter().any(|x| x == "/MeterPulse/value"));
    assert!(plan.batches.iter().any(|b| {
        b.label == "output 18"
            && b.paths
                .iter()
                .any(|x| x == "/devices/0/outputs/18/meters/1/MeterLevel/value")
    }));
    // Every path is in the tree and none repeats.
    let all: Vec<&str> = plan.all_paths().collect();
    let unique: std::collections::HashSet<&str> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len());
    assert!(all.iter().all(|p| tree.has_property(p)));
    assert!((4000..5200).contains(&all.len()), "{}", all.len());
}

/// Plug-in parameter nodes under a loaded slot (live capture: inputs 0-2 carry plug-ins).
#[test]
fn plugin_parameters() {
    let tree = live_x4();
    let t = Topology::from_tree(&tree);
    let slots = t.loaded_slots(&tree);
    assert!(!slots.is_empty());
    let slot = t.inputs[1].insert(0).unwrap();
    assert!(slots.contains(slot));
    let params = PluginParameter::list(&tree, slot);
    assert!(params.len() > 30, "{}", params.len());
    assert_eq!(
        params[0].paths.normalized(),
        "/devices/0/inputs/1/effects/0/parameters/0/NormalizedValue"
    );
    assert_eq!(params[0].name, "Link Channels");
    assert!(tree.has_property(&params[0].paths.normalized()));
    assert!(params.windows(2).all(|w| w[0].index() < w[1].index())); // numeric order
    let stepped = params.iter().find(|p| p.is_stepped());
    if let Some(p) = stepped {
        let n = p.steps.len() - 1;
        assert_eq!(p.step_of(p.normalized_of_step(n)), n);
        assert_eq!(p.step_of(0.0), 0);
    }
    assert!(
        slot.parameter(3)
            .subscribed()
            .iter()
            .all(|p| p.starts_with("/devices/0/inputs/1/effects/0/parameters/3/"))
    );
    assert!(t.inputs[0].preamp.as_ref().unwrap().unison.is_unison());
    assert!(!slot.is_unison());
    assert_eq!(
        ua_engine::InsertPaths::from_slot("/devices/0/auxs/1/effects/3").index,
        3
    );
}
