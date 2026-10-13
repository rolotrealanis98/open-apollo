use std::path::PathBuf;
use std::process::ExitCode;

use oa_assets::detect::{self, Install};
use oa_assets::discover::{self, Root, Source};
use oa_assets::firmware::{self, Event, Pins};
use oa_assets::import;

const USAGE: &str = "\
oa-assets: use your own UAD install with open-apollo

USAGE:
  oa-assets scan   [--from PATH]... [--json]   find UAD installs (Windows volumes, Wine prefixes)
  oa-assets mount  DEVICE                      mount a Windows partition read-only, then scan it
  oa-assets attach IMAGE                       attach a (shut down) VM's raw disk image read-only, then scan it
  oa-assets detach IMAGE|LOOPDEVICE            unmount an attached image and release it
  oa-assets import [--from PATH] [--dry-run]   copy UAD Console's resources for console-rs
  oa-assets status                             show what has been imported
  oa-assets firmware [--from PATH] [--out DIR] [--accept-unknown] [--verify-golden] [--json]
                                               extract the DSP modules and USB boot images the Apollo
                                               needs (default --out /lib/firmware/universal-audio)

PATH is the folder that holds Windows' C:\\ (e.g. /run/media/you/Windows) or a Wine prefix.
Nothing is written to the source, and nothing leaves this machine.
Options: --data DIR  (default ~/.local/share/open-apollo)
firmware: only versions this oa-assets knows are written; --accept-unknown writes others too.
  --verify-golden re-extracts into a temp dir and checks every pinned hash (without --out it
  writes nothing else). Exit: 0 done, 1 a required module missing, 2 an unknown module version.";

struct Args {
    cmd: String,
    from: Vec<PathBuf>,
    json: bool,
    dry_run: bool,
    data: PathBuf,
    device: Option<String>,
    out: Option<PathBuf>,
    accept_unknown: bool,
    verify_golden: bool,
}

impl Args {
    fn parse(mut it: impl Iterator<Item = String>) -> Result<Self, String> {
        let cmd = it.next().ok_or("missing command")?;
        let mut a = Args {
            cmd,
            from: Vec::new(),
            json: false,
            dry_run: false,
            data: oa_assets::data_dir(),
            device: None,
            out: None,
            accept_unknown: false,
            verify_golden: false,
        };
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--from" => a.from.push(it.next().ok_or("--from needs a path")?.into()),
                "--data" => a.data = it.next().ok_or("--data needs a path")?.into(),
                "--out" => a.out = Some(it.next().ok_or("--out needs a path")?.into()),
                "--accept-unknown" => a.accept_unknown = true,
                "--verify-golden" => a.verify_golden = true,
                "--json" => a.json = true,
                "--dry-run" => a.dry_run = true,
                "-h" | "--help" => a.cmd = "help".into(),
                s if !s.starts_with('-') && a.device.is_none() => a.device = Some(s.to_string()),
                s => return Err(format!("unknown option {s}")),
            }
        }
        Ok(a)
    }
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args.cmd.as_str() {
        "scan" => scan(&args),
        "mount" => mount(&args),
        "attach" => attach(&args),
        "detach" => detach(&args),
        "import" => run_import(&args),
        "status" => status(&args),
        "firmware" => return run_firmware(&args),
        "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oa-assets: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--from` roots, or every mounted Windows volume and Wine prefix.
fn roots(args: &Args) -> Vec<Root> {
    if !args.from.is_empty() {
        return args.from.iter().map(|p| discover::given(p)).collect();
    }
    let mut r = discover::mounted_volumes();
    r.extend(discover::wine_prefixes());
    r
}

fn scan(args: &Args) -> Result<(), String> {
    let installs: Vec<Install> = roots(args).iter().map(detect::inspect).collect();
    let unmounted = if args.from.is_empty() { discover::unmounted_partitions() } else { Vec::new() };
    if args.json {
        let v = serde_json::json!({
            "installs": installs.iter().map(Install::to_json).collect::<Vec<_>>(),
            "unmounted": unmounted.iter().map(|u| serde_json::json!({
                "device": u.device, "fstype": u.fstype, "label": u.label, "size": u.size,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let mut any = false;
    for i in &installs {
        print_install(i);
        any |= i.found_anything();
    }
    if !unmounted.is_empty() {
        println!("Windows partitions that aren't mounted:");
        for u in &unmounted {
            let label = u.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default();
            let size = u.size.as_deref().unwrap_or("?");
            if u.is_bitlocker() {
                println!("  {}{label} ({size}): BitLocker-encrypted; unlock it in Windows first", u.device);
            } else {
                println!("  {}{label} ({size}): run `oa-assets mount {}` to look inside", u.device, u.device);
            }
        }
    }
    if !any && unmounted.is_empty() {
        println!("No UAD install found. Mount your Windows partition, or pass --from PATH.");
    }
    Ok(())
}

fn print_install(i: &Install) {
    let src = match &i.root.source {
        Source::Given => "given path".to_string(),
        Source::Volume { device, fstype } => format!("{device}, {fstype}"),
        Source::WinePrefix => "Wine prefix".to_string(),
    };
    println!("{} ({src})", i.root.path.display());
    if !i.found_anything() {
        println!("  no UAD software\n");
        return;
    }
    match &i.console {
        Some(c) => {
            println!("  UAD Console   {}  ({})", c.version.as_deref().unwrap_or("version unknown"), c.dir.display())
        }
        None => println!("  UAD Console   not installed"),
    }
    if let Some((_, v)) = &i.mixer_engine {
        println!("  Mixer Engine  {}", v.as_deref().unwrap_or("version unknown"));
    }
    if let Some(v) = &i.uad_version {
        println!("  UAD software      {v}");
    }
    println!("  Powered plug-ins  {} DLLs", i.powered_plugins.len());
    println!("  VST3 wrappers     {} bundles", i.vst3.len());
    println!("  DSP programs      {}", if i.sdk_dll.is_some() { "UAD2SDK.dll found" } else { "UAD2SDK.dll missing" });
    println!("  Firmware          {} files", i.firmware.len());
    match &i.plugin_icons {
        Some(oa_assets::detect::PluginIcons::Folder(_)) => println!("  Plug-in icons     installed"),
        Some(oa_assets::detect::PluginIcons::CachedMsi(_)) => println!("  Plug-in icons     in UA's installer cache"),
        None => println!("  Plug-in icons     not found"),
    }
    println!(
        "  Drivers           {}",
        if i.drivers.is_empty() { "none".to_string() } else { format!("{} files", i.drivers.len()) }
    );
    println!();
}

fn mount(args: &Args) -> Result<(), String> {
    let dev = args.device.as_deref().ok_or("mount needs a device, e.g. /dev/nvme0n1p3")?;
    let at = discover::mount_readonly(dev)?;
    println!("Mounted {dev} read-only at {}\n", at.display());
    print_install(&detect::inspect(&Root { path: at, source: Source::Given }));
    Ok(())
}

fn attach(args: &Args) -> Result<(), String> {
    let image = args.device.as_deref().ok_or("attach needs a disk image, e.g. ~/.windows/data.img")?;
    let a = discover::attach_image(std::path::Path::new(image))?;
    println!("Attached {image} read-only as {}\n", a.loop_device);
    if a.roots.is_empty() {
        println!("No Windows partition found in the image.");
    }
    for r in &a.roots {
        print_install(&detect::inspect(r));
    }
    println!("When done: oa-assets detach {image}");
    Ok(())
}

fn detach(args: &Args) -> Result<(), String> {
    let what = args.device.as_deref().ok_or("detach needs the image or its loop device")?;
    let dev = if what.starts_with("/dev/") {
        what.to_string()
    } else {
        discover::existing_loop(std::path::Path::new(what)).ok_or(format!("{what} isn't attached"))?
    };
    discover::detach(&dev)?;
    println!("Detached {dev}");
    Ok(())
}

fn run_import(args: &Args) -> Result<(), String> {
    let installs: Vec<Install> = roots(args).iter().map(detect::inspect).collect();
    let with_console: Vec<&Install> = installs.iter().filter(|i| i.console.is_some()).collect();
    let Some(chosen) = with_console.first() else {
        return Err("no UAD Console install found; run `oa-assets scan` to see what was checked".into());
    };
    if with_console.len() > 1 && args.from.is_empty() {
        println!("Several installs have UAD Console; using the first. Pick one with --from:");
        for i in &with_console {
            println!("  {}", i.root.path.display());
        }
        println!();
    }
    let console = chosen.console.as_ref().expect("filtered on console");
    let version = console.version.as_deref().unwrap_or("version unknown");
    if args.dry_run {
        let (files, bytes) = import::measure(&console.resources).map_err(|e| e.to_string())?;
        println!("Would copy UAD Console {version} resources: {files} files, {:.1} MB", bytes as f64 / 1e6);
        println!("  from {}\n  to   {}", console.resources.display(), args.data.join(import::CONSOLE_DIR).display());
        return Ok(());
    }
    let done = import::import_console(console, &args.data).map_err(|e| format!("copy failed: {e}"))?;
    println!(
        "Imported UAD Console {version} resources: {} files, {:.1} MB -> {}",
        done.files,
        done.bytes as f64 / 1e6,
        done.dest.display()
    );
    Ok(())
}

fn status(args: &Args) -> Result<(), String> {
    let m = import::read_manifest(&args.data);
    match m.get("console") {
        Some(c) => println!(
            "UAD Console resources: {} ({} files) from {}",
            c["version"].as_str().unwrap_or("version unknown"),
            c["files"],
            c["from"].as_str().unwrap_or("?")
        ),
        None => println!("Nothing imported yet. Run `oa-assets scan`, then `oa-assets import`."),
    }
    Ok(())
}

/// `oa-assets firmware`: exit 0 done, 1 something required missing (or an
/// error), 2 a required module isn't a known version.
fn run_firmware(args: &Args) -> ExitCode {
    match firmware_cmd(args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            if args.json {
                println!("{}", serde_json::json!({"exit": 1, "error": e}));
            }
            eprintln!("[FAIL]  {e}");
            ExitCode::from(1)
        }
    }
}

fn firmware_cmd(args: &Args) -> Result<u8, String> {
    if let Some(stray) = &args.device {
        return Err(format!("unexpected {stray}; pass the install with --from PATH"));
    }
    let pins = Pins::builtin();
    let mut log = |e: Event| {
        if !args.json {
            println!("{}", e.render());
        }
    };
    let root = firmware_root(args, &pins, &mut log)?;
    let out = args.out.clone().or_else(|| (!args.verify_golden).then(|| PathBuf::from(firmware::DEFAULT_OUT)));
    if let Some(o) = &out
        && firmware::inside(o, &root.path)
    {
        return Err(format!("--out {} is inside the UAD install; pick a folder outside it", o.display()));
    }
    let opts = firmware::Options { accept_unknown: args.accept_unknown };
    let report = match &out {
        Some(o) => Some(firmware::extract(&root.path, o, &pins, opts, &mut log).map_err(|e| e.to_string())?),
        None => None,
    };
    let golden = if args.verify_golden {
        Some(firmware::verify_golden(&root.path, &pins, out.as_deref(), &mut log).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let exit = report.as_ref().map_or(0, |r| r.exit).max(golden.as_ref().map_or(0, |g| g.exit));
    if args.json {
        let v = serde_json::json!({
            "exit": exit,
            "from": root.path,
            "out": out,
            "files": report.as_ref().map(|r| r.files.iter().map(|f| f.to_json()).collect::<Vec<_>>()),
            "manifest": report.as_ref().map(|r| &r.manifest),
            "golden": golden.as_ref().map(|g| g.to_json()),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    Ok(exit)
}

/// `--from`, or the first scanned install that has the strip plug-ins or UAD2SDK.dll.
fn firmware_root(args: &Args, pins: &Pins, log: &mut dyn FnMut(Event)) -> Result<Root, String> {
    match args.from.as_slice() {
        [] => {}
        [one] if one.is_dir() => return Ok(discover::given(one)),
        [one] => return Err(format!("{} isn't a folder", one.display())),
        _ => return Err("firmware takes one --from".into()),
    }
    let mut candidates = discover::mounted_volumes();
    candidates.extend(discover::wine_prefixes());
    let chosen = candidates.into_iter().find(|r| firmware::has_inputs(&r.path, pins)).ok_or(
        "no UAD install with the Console strip plug-ins or UAD2SDK.dll found; mount your Windows \
         partition (oa-assets scan) or pass --from PATH",
    )?;
    log(Event::Info(format!("Using {} (pick another with --from PATH)", chosen.path.display())));
    Ok(chosen)
}
