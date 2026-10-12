//! `ua-engine-probe`: read-only inspection of a UA Mixer Engine or open-apollo daemon.
//!
//! Every live command connects with the default (read-only) options: the connection refuses
//! `set`/`post`, and this tool has no switch to change that. It sends identity, `get`,
//! `subscribe` and `unsubscribe` only.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ua_engine::{
    ConnectionState, Endpoint, Engine, MockEngine, MockOptions, Options, Snapshot, Status, Store,
    SubscriptionPlan, Topology, Tree, Update, Value, tapers,
};

const USAGE: &str = "\
ua-engine-probe [--host H] [--port P | --endpoint HOST:PORT] <command> [options]

Live commands (read-only: identity, get, subscribe, unsubscribe):
  dump --out FILE [--trim] [--catalogs] [--pretty]   recursive tree snapshot
  topology [--snapshot FILE] [--all] [--paths]       strips (live, or from a snapshot/device map)
  meters [--seconds N]                              pulse cadence, meter pushes, readings
  subs [--snapshot FILE]                            subscription plan sizes
  get PATH [--recursive]                            one get
  browse [--seconds N]                              Bonjour _uamixer._tcp (feature mdns)

Offline:
  compare A.json B.json                             tree shape differences
  convert --in FILE --out FILE [--trim] [--catalogs]
  mock-serve --snapshot FILE [--port N] [--meters]  loopback MockEngine until killed

Defaults: --host 127.0.0.1 --port 4710.";

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }

    fn opt(&mut self, name: &str) -> Option<String> {
        let i = self.rest.iter().position(|a| a == name)?;
        if i + 1 >= self.rest.len() {
            return None;
        }
        let v = self.rest.remove(i + 1);
        self.rest.remove(i);
        Some(v)
    }

    fn positional(&mut self) -> Option<String> {
        let i = self.rest.iter().position(|a| !a.starts_with("--"))?;
        Some(self.rest.remove(i))
    }
}

fn main() -> ExitCode {
    let mut args = Args {
        rest: std::env::args().skip(1).collect(),
    };
    if args.flag("--help") || args.flag("-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let endpoint = match args.opt("--endpoint") {
        Some(e) => Endpoint::parse(&e),
        None => {
            let host = args.opt("--host").unwrap_or_else(|| "127.0.0.1".into());
            let port = args
                .opt("--port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(ua_engine::DEFAULT_PORT);
            Endpoint::new(host, port)
        }
    };
    let Some(cmd) = args.positional() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match cmd.as_str() {
        "dump" => dump(&endpoint, &mut args),
        "topology" => topology(&endpoint, &mut args),
        "meters" => meters(&endpoint, &mut args),
        "subs" => subs(&endpoint, &mut args),
        "get" => get(&endpoint, &mut args),
        "browse" => browse(&mut args),
        "compare" => compare(&mut args),
        "convert" => convert(&mut args),
        "mock-serve" => mock_serve(&mut args),
        _ => Err(format!("unknown command {cmd}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type R = Result<(), String>;

/// Read-only options for every live command.
fn read_only() -> Options {
    Options {
        reconnect: false,
        ..Options::default()
    }
}

/// Connects, fetches the tree and returns it with timing.
fn fetch_tree(endpoint: &Endpoint) -> Result<(Tree, Duration, u64), String> {
    let handle = Engine::connect(endpoint.clone(), read_only());
    assert!(!handle.options().allow_writes);
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut loading_at = None;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .ok_or("timed out waiting for the tree")?;
        match handle.updates().recv_timeout(left) {
            Ok(Update::State(ConnectionState::Loading)) => loading_at = Some(Instant::now()),
            Ok(Update::State(ConnectionState::Failed(r))) => {
                return Err(format!("{endpoint}: {r}"));
            }
            Ok(Update::Tree { tree, .. }) => {
                let took = loading_at.map(|t| t.elapsed()).unwrap_or_default();
                return Ok((*tree, took, handle.counters().bytes_in));
            }
            Ok(_) => {}
            Err(_) => return Err("timed out waiting for the tree".into()),
        }
    }
}

fn load_or_fetch(endpoint: &Endpoint, snapshot: Option<String>) -> Result<(Tree, String), String> {
    match snapshot {
        Some(f) => {
            let s = Snapshot::load(&f).map_err(|e| format!("{f}: {e}"))?;
            Ok((s.tree, f))
        }
        None => fetch_tree(endpoint).map(|(t, _, _)| (t, format!("live {endpoint}"))),
    }
}

fn dump(endpoint: &Endpoint, args: &mut Args) -> R {
    let out = args.opt("--out").ok_or("dump needs --out FILE")?;
    let (trim, catalogs, pretty) = (
        args.flag("--trim"),
        args.flag("--catalogs"),
        args.flag("--pretty"),
    );
    let (tree, took, bytes) = fetch_tree(endpoint)?;
    println!(
        "{endpoint}: tree {:.2} MB in {:.2} s, {} nodes, {} properties, {} stubs",
        bytes as f64 / 1e6,
        took.as_secs_f64(),
        tree.node_count(),
        tree.property_count(),
        tree.stub_paths().len()
    );
    let tree = if trim || catalogs {
        tree.trimmed(catalogs)
    } else {
        tree
    };
    let info = ua_engine::SnapshotInfo {
        source: Some(format!("live {endpoint} via ua-engine-probe dump")),
        captured: Some(ua_engine::iso8601_now()),
        ..Default::default()
    };
    let snap = Snapshot::new(tree, info);
    snap.save(&out, pretty).map_err(|e| e.to_string())?;
    println!(
        "device {} · engine {} · wrote {out}",
        snap.info.device.as_deref().unwrap_or("?"),
        snap.info.engine_version.as_deref().unwrap_or("?")
    );
    Ok(())
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn topology(endpoint: &Endpoint, args: &mut Args) -> R {
    let (all, paths) = (args.flag("--all"), args.flag("--paths"));
    let (tree, source) = load_or_fetch(endpoint, args.opt("--snapshot"))?;
    print_topology(&tree, &source, all, paths);
    Ok(())
}

fn print_topology(tree: &Tree, source: &str, all: bool, paths: bool) {
    let t = Topology::from_tree(tree);
    let s = |p: &str| {
        tree.value(p)
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned()
    };
    println!("{source}");
    println!(
        "device {} · engine {} · protocol {} · online {}",
        t.device_name.as_deref().unwrap_or("?"),
        s("/Version"),
        s("/ProtocolVersion"),
        tree.value("/devices/0/DeviceOnline")
            .map(|v| v.to_string())
            .unwrap_or("?".into())
    );
    let inputs: usize = tree.indices("/devices/0/inputs").len();
    println!(
        "inputs: {} channels → {} strips ({} stereo), {} visible · auxes {} · cues {} (CueBusCount {}, {} active) · headphones {} · monitor {} · talkback mic {}",
        inputs,
        t.all_inputs.len(),
        t.all_inputs.iter().filter(|s| s.is_stereo()).count(),
        t.inputs.len(),
        t.auxes.len(),
        t.cues.len(),
        t.cue_bus_count,
        t.cues.iter().filter(|c| c.is_active).count(),
        t.headphones.len(),
        t.monitor
            .as_ref()
            .map(|m| format!("outputs/{}", m.index))
            .unwrap_or("none".into()),
        t.talkback
            .strip
            .as_ref()
            .map(|s| format!("inputs/{} (active {})", s.index, yn(s.is_active)))
            .unwrap_or("none".into()),
    );
    println!(
        "  {:<4} {:<18} {:<8} {:<7} {:<8} {:<26} fader",
        "in", "name", "kind", "stereo", "visible", "preamp"
    );
    for strip in t.all_inputs.iter().filter(|s| all || s.is_visible()) {
        let pre = strip
            .preamp
            .as_ref()
            .map(|p| {
                let gain = tree
                    .value(&p.gain_db())
                    .and_then(Value::as_f64)
                    .map(|g| format!("{g:.0} dB"))
                    .unwrap_or_default();
                let phantom = if p.phantom_enabled { "" } else { "(off)" };
                let pad = if p.pad_enabled { "" } else { "(off)" };
                format!(
                    "{gain} 48V{phantom} pad{pad}{}",
                    if p.has_hi_z { " HiZ" } else { "" }
                )
            })
            .unwrap_or_default();
        let fader = tree
            .value(&strip.fader_db())
            .and_then(Value::as_f64)
            .map(tapers::format_db)
            .unwrap_or_default();
        let stereo = strip
            .right_index
            .map(|r| format!("{}/{r}", strip.index))
            .unwrap_or_else(|| "-".into());
        let kind = format!("{:?}", strip.kind).to_lowercase();
        println!(
            "  {:<4} {:<18} {:<8} {:<7} {:<8} {:<26} {}",
            strip.index,
            strip.name,
            kind,
            stereo,
            yn(strip.is_visible()),
            pre,
            fader
        );
    }
    for a in &t.auxes {
        let fader = tree
            .value(&a.fader_db())
            .and_then(Value::as_f64)
            .map(tapers::format_db)
            .unwrap_or_default();
        let sends: Vec<&str> = a.sends.iter().map(|s| s.id.as_str()).collect();
        println!(
            "  aux {} {:<12} {fader} sends {}",
            a.index,
            a.name,
            sends.join(",")
        );
    }
    for c in &t.cues {
        println!(
            "  cue {} = outputs/{} {:<10} active {}",
            c.number,
            c.index,
            c.name,
            yn(c.is_active)
        );
    }
    for h in &t.headphones {
        println!(
            "  hp {} = outputs/{} {:<10} level {}",
            h.number,
            h.index,
            h.name,
            h.level.as_deref().unwrap_or("hardware")
        );
    }
    if let Some(m) = &t.monitor {
        let db = tree
            .value(&m.level_db())
            .and_then(Value::as_f64)
            .map(|d| format!("{d} dB"))
            .unwrap_or_default();
        println!("  monitor = outputs/{} {} {db}", m.index, m.name);
    }
    println!(
        "  talkback {} present {}",
        t.talkback.on(),
        yn(tree.has_property(&t.talkback.on()))
    );
    if paths && let Some(s) = t.inputs.first() {
        println!("\npaths for {}:", s.name);
        for p in [s.fader(), s.fader_db(), s.pan(), s.solo(), s.mute()] {
            println!(
                "  {p} = {}",
                tree.value(&p).map(|v| v.to_string()).unwrap_or("-".into())
            );
        }
        for send in &s.sends {
            println!(
                "  {} ({}) gain {} bypass {}",
                send.id,
                send.name,
                send.gain(),
                send.bypass()
            );
        }
        for m in &s.meters {
            println!("  meter {}", m.base);
        }
        if let Some(p) = &s.preamp {
            println!(
                "  preamp {} 48V {} pad {} unison {}",
                p.gain(),
                p.phantom(),
                p.pad(),
                p.unison.effect_name()
            );
        }
    }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn meters(endpoint: &Endpoint, args: &mut Args) -> R {
    let seconds: f64 = args
        .opt("--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let mut store = Store::new();
    store.connect(endpoint.clone(), read_only());
    let deadline = Instant::now() + Duration::from_secs(90);
    while *store.status() != Status::Live {
        store.drain();
        if let Status::Failed(r) = store.status() {
            return Err(r.clone());
        }
        if Instant::now() > deadline {
            return Err(format!("not live: {:?}", store.status()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let plan = SubscriptionPlan::new(store.topology(), store.tree());
    println!(
        "{endpoint}: live · {} · engine {} · {} batches, {} paths subscribed",
        store.device_name().unwrap_or("?"),
        store.engine_version().unwrap_or("?"),
        plan.batches.len(),
        store.subscribed_paths().len()
    );
    // Let the subscribe replies (current values) arrive before measuring.
    let settle = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < settle {
        store.drain();
        std::thread::sleep(Duration::from_millis(16));
    }
    let meter_paths = store
        .subscribed_paths()
        .iter()
        .filter(|p| ua_engine::MeterStore::is_meter_path(p))
        .count();
    store.meters().reset_stats();
    let c0 = store.connection().map(|h| h.counters()).unwrap_or_default();
    let rev0 = store.revision();
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < seconds {
        store.drain();
        std::thread::sleep(Duration::from_millis(16));
    }
    let elapsed = start.elapsed().as_secs_f64();
    let c1 = store.connection().map(|h| h.counters()).unwrap_or_default();
    let stats = store.meters().stats();
    let mut iv: Vec<f64> = stats.pulse_intervals.iter().map(|s| s * 1000.0).collect();
    iv.sort_by(f64::total_cmp);
    let frames = c1.messages_in - c0.messages_in;
    let multi = c1.multi_frames - c0.multi_frames;
    println!(
        "{elapsed:.1} s: /MeterPulse {} pushes = {:.1} Hz · interval ms p50 {:.1} min {:.1} p99 {:.1} max {:.1}",
        stats.pulses,
        stats.pulses as f64 / elapsed,
        percentile(&iv, 0.5),
        iv.first().copied().unwrap_or(f64::NAN),
        percentile(&iv, 0.99),
        iv.last().copied().unwrap_or(f64::NAN)
    );
    println!(
        "meter paths subscribed {meter_paths} · meter pushes {} ({:.0}/s) from {} meters · frames {frames} ({multi} multi envelopes, {} plain) · {:.1} kB/s · non-meter value changes {}",
        stats.pushes,
        stats.pushes as f64 / elapsed,
        stats.active_meters,
        frames - multi,
        (c1.bytes_in - c0.bytes_in) as f64 / elapsed / 1000.0,
        store.revision() - rev0
    );
    let t = store.topology();
    for s in &t.inputs {
        let keys: Vec<&str> = s.meters.iter().map(|m| m.base.as_str()).collect();
        let r = store.meters().readings(&keys);
        let txt: Vec<String> = r
            .iter()
            .map(|m| {
                format!(
                    "{:.1}/{:.1}{}",
                    m.level_db,
                    m.peak_db,
                    if m.clip { " CLIP" } else { "" }
                )
            })
            .collect();
        println!(
            "  {:<18} {}  ({} updates)",
            s.name,
            txt.join("  "),
            r.iter().map(|m| m.updates).sum::<u32>()
        );
    }
    if let Some(m) = &t.monitor {
        let r: Vec<String> = m
            .meters
            .iter()
            .map(|x| store.meters().reading(&x.base))
            .map(|m| format!("{:.1}/{:.1}", m.level_db, m.peak_db))
            .collect();
        println!("  {:<18} {}", "monitor", r.join("  "));
    }
    let refused = store
        .connection()
        .map(|h| h.counters().refused_writes)
        .unwrap_or(0);
    println!(
        "writes refused by the connection: {refused} · writer sent {} suppressed {}",
        store.writer().sent_count(),
        store.writer().suppressed_count()
    );
    store.disconnect();
    Ok(())
}

fn subs(endpoint: &Endpoint, args: &mut Args) -> R {
    let (tree, source) = load_or_fetch(endpoint, args.opt("--snapshot"))?;
    let t = Topology::from_tree(&tree);
    let plan = SubscriptionPlan::new(&t, &tree);
    println!(
        "{source}: {} batches, {} paths",
        plan.batches.len(),
        plan.path_count()
    );
    for b in &plan.batches {
        println!("  {:<10} {}", b.label, b.paths.len());
    }
    Ok(())
}

fn get(endpoint: &Endpoint, args: &mut Args) -> R {
    let recursive = args.flag("--recursive");
    let path = args.positional().ok_or("get needs a PATH")?;
    let handle = Engine::connect(
        endpoint.clone(),
        Options {
            load_tree: false,
            ..read_only()
        },
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !handle.status().is_connected() {
        if Instant::now() > deadline {
            return Err(format!("not connected: {:?}", handle.status()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let m = handle.get(&path, recursive).map_err(|e| e.to_string())?;
    let text = m.data.map(|d| d.to_string()).unwrap_or_default();
    if text.len() > 4000 {
        println!(
            "{} {}… ({} bytes)",
            m.path,
            text.chars().take(4000).collect::<String>(),
            text.len()
        );
    } else {
        println!("{} {text}", m.path);
    }
    Ok(())
}

#[cfg(feature = "mdns")]
fn browse(args: &mut Args) -> R {
    let seconds: f64 = args
        .opt("--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3.0);
    let found = ua_engine::discovery::browse(Duration::from_secs_f64(seconds))
        .map_err(|e| e.to_string())?;
    for d in &found {
        println!("{} → {} ({}:{})", d.name, d.endpoint(), d.host, d.port);
    }
    if found.is_empty() {
        println!("no _uamixer._tcp services found");
    }
    Ok(())
}

#[cfg(not(feature = "mdns"))]
fn browse(_: &mut Args) -> R {
    Err("built without the mdns feature (cargo run --features mdns)".into())
}

fn convert(args: &mut Args) -> R {
    let input = args.opt("--in").ok_or("convert needs --in")?;
    let out = args.opt("--out").ok_or("convert needs --out")?;
    let (trim, catalogs) = (args.flag("--trim"), args.flag("--catalogs"));
    let s = Snapshot::load(&input).map_err(|e| format!("{input}: {e}"))?;
    let tree = if trim || catalogs {
        s.tree.trimmed(catalogs)
    } else {
        s.tree
    };
    let snap = Snapshot::new(tree, s.info);
    snap.save(&out, false).map_err(|e| e.to_string())?;
    println!("{input} → {out}: {} properties", snap.tree.property_count());
    Ok(())
}

/// `/devices/0/inputs/12/sends/3/Gain` → `/devices/N/inputs/N/sends/N/Gain`.
fn shape(path: &str) -> String {
    path.split('/')
        .map(|c| {
            if !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()) {
                "N"
            } else {
                c
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn compare(args: &mut Args) -> R {
    let a = args.positional().ok_or("compare needs two files")?;
    let b = args.positional().ok_or("compare needs two files")?;
    let ta = Snapshot::load(&a).map_err(|e| format!("{a}: {e}"))?;
    let tb = Snapshot::load(&b).map_err(|e| format!("{b}: {e}"))?;
    for (name, s) in [(&a, &ta), (&b, &tb)] {
        let count = |f: &dyn Fn(&ua_engine::Property) -> bool| {
            s.tree.properties().filter(|(_, p)| f(p)).count()
        };
        let commands = s
            .tree
            .node("/")
            .and_then(|n| n.commands.as_ref())
            .and_then(|c| c.as_object())
            .map(|o| o.len())
            .unwrap_or(0);
        println!(
            "{name}: {} · engine {} · {} nodes · {} properties · readonly {} · default {} · enabled {} · root commands {commands}",
            s.info.device.as_deref().unwrap_or("?"),
            s.info.engine_version.as_deref().unwrap_or("?"),
            s.tree.node_count(),
            s.tree.property_count(),
            count(&|p| p.meta.readonly),
            count(&|p| p.meta.default.is_some()),
            count(&|p| p.meta.enabled.is_some()),
        );
    }
    // Exact paths, grouped by shape so a whole collection shows as one line.
    let pa: BTreeSet<&str> = ta.tree.properties().map(|(p, _)| p).collect();
    let pb: BTreeSet<&str> = tb.tree.properties().map(|(p, _)| p).collect();
    let group = |only: Vec<&&str>| {
        let mut g: BTreeMap<String, usize> = BTreeMap::new();
        for p in only {
            *g.entry(shape(p)).or_default() += 1;
        }
        g
    };
    for (x, y, only) in [
        (&a, &b, group(pa.difference(&pb).collect())),
        (&b, &a, group(pb.difference(&pa).collect())),
    ] {
        println!(
            "\nproperties in {x} but not {y}: {} ({} shapes)",
            only.values().sum::<usize>(),
            only.len()
        );
        for (s, n) in only.iter().take(80) {
            println!("  {n:>5} {s}");
        }
    }
    let mut type_diffs: BTreeMap<String, (String, String, usize)> = BTreeMap::new();
    for p in pa.intersection(&pb) {
        let x = ta
            .tree
            .meta(p)
            .and_then(|m| m.ty.clone())
            .unwrap_or_default();
        let y = tb
            .tree
            .meta(p)
            .and_then(|m| m.ty.clone())
            .unwrap_or_default();
        if x != y {
            type_diffs.entry(shape(p)).or_insert((x, y, 0)).2 += 1;
        }
    }
    println!("\ntype differences on shared paths: {}", type_diffs.len());
    for (s, (x, y, n)) in type_diffs.iter().take(30) {
        println!("  {n:>5} {s}: {x} vs {y}");
    }
    println!();
    print_topology(&ta.tree, &a, false, false);
    println!();
    print_topology(&tb.tree, &b, false, false);
    Ok(())
}

fn mock_serve(args: &mut Args) -> R {
    let port: u16 = args
        .opt("--port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(14711);
    let meters = args.flag("--meters");
    let file = args
        .opt("--snapshot")
        .ok_or("mock-serve needs --snapshot FILE (a snapshot or device map)")?;
    let snap = Snapshot::load(&file).map_err(|e| format!("{file}: {e}"))?;
    let mock = MockEngine::start(
        snap.tree,
        MockOptions {
            simulate_meters: meters,
            port,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    println!(
        "MockEngine serving {file} on 127.0.0.1:{} (meters {}); Ctrl-C to stop",
        mock.port(),
        yn(meters)
    );
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
