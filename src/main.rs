#[cfg(target_os = "linux")]
mod attach;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod herdr;
#[cfg(target_os = "linux")]
mod holder;
#[cfg(target_os = "linux")]
mod lifetime;
mod shell;
#[cfg(target_os = "linux")]
mod terminal;
#[cfg(any(test, not(target_os = "linux")))]
mod unsupported;

#[cfg(target_os = "linux")]
use std::io::{IsTerminal, Write};

#[cfg(target_os = "linux")]
use serde_json::json;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn fail<T>(message: &str) -> Result<T> {
    Err(message.to_owned().into())
}

#[cfg(target_os = "linux")]
const COMMANDS: &[(&str, &str)] = &[
    (
        "attach <holder-id>",
        "attach an existing holder; never create a replacement",
    ),
    (
        "reconcile",
        "enumerate coverage; retain ambiguous or missing panes",
    ),
    (
        "list [--json]",
        "show every pane, holder, orphan cleanup and coverage limitation",
    ),
    (
        "kill <holder-id> [--confirm <holder-id>]",
        "terminate one exact holder with explicit confirmation",
    ),
    (
        "doctor",
        "check platform, user manager, configuration and protocol without changing configuration",
    ),
    (
        "event",
        "bounded plugin event hint; never an automatic kill capability",
    ),
    (
        "action-list",
        "list coverage in the plugin log and show a notification",
    ),
];

#[cfg(target_os = "linux")]
fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-shield: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    let result = (|| -> Result<()> {
        if std::env::args_os().len() != 1 {
            return fail("UNPROTECTED: Linux with a verified systemd user manager is required");
        }
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
            })
            .ok_or("HOME or XDG_CONFIG_HOME is required to locate the configured plain shell")?;
        unsupported::run(&root.join("herdr-shield/config.json"))
    })();
    if let Err(error) = result {
        eprintln!("herdr-shield: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "help")
    {
        println!(
            "herdr-shield {} — Linux default-shell process continuity\nWithout arguments: attach/create for a validated Herdr pane.",
            env!("CARGO_PKG_VERSION")
        );
        for (name, description) in COMMANDS {
            println!("  {name:48} {description}");
        }
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--version") {
        println!(
            "herdr-shield {} wire 1.0 upstream 22",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let paths = config::Paths::load()?;
    match args.first().map(String::as_str) {
        None => attach::default_shell(&paths),
        Some("holder") if args.len() == 2 => holder::serve(paths, &args[1]),
        Some("attach") if args.len() == 2 => attach::existing(&paths, &args[1]),
        Some("reconcile") if args.len() == 1 => herdr::reconcile::run(&paths),
        Some("list")
            if args.len() == 1
                || (args.len() == 2 && args.get(1).is_some_and(|arg| arg == "--json")) =>
        {
            herdr::reconcile::list(&paths, false)
        }
        Some("action-list") if args.len() == 1 => herdr::reconcile::list(&paths, true),
        Some("event") if args.len() == 1 => herdr::reconcile::event(&paths),
        Some("doctor") if args.len() == 1 => doctor(&paths),
        Some("kill") if args.len() == 2 || args.len() == 4 => kill(&paths, &args),
        _ => fail("unknown command or arguments; use --help"),
    }
}

#[cfg(target_os = "linux")]
fn doctor(paths: &config::Paths) -> Result<()> {
    paths.config()?;
    lifetime::systemd::available()?;
    herdr::prerequisites(paths)?;
    let socket = herdr::endpoint()?;
    let pong = herdr::rpc(&socket, "ping", json!({}))?;
    if pong["protocol"] != 22 {
        return fail("incompatible upstream; protocol 22 required");
    }
    let holders = holder::registry::Metadata::all(paths)?;
    let last_error = paths.state.join("last-error.json");
    if last_error.exists() {
        let previous: serde_json::Value = config::read_json(&last_error)?;
        if !previous["error"].is_null() {
            return fail(&format!(
                "ERROR: unresolved reconciliation failure: {}; run reconcile",
                previous["error"]
            ));
        }
    }
    for meta in &holders {
        if meta
            .holder
            .as_ref()
            .is_some_and(|identity| identity.life() == lifetime::identity::Life::Live)
        {
            let _ = holder::client(paths, meta)?;
        }
    }
    println!(
        "OK: Linux/systemd user manager, private paths, explicit shell, native resume disabled, upstream v22; {} registry entries; {} pinned API policies. Actual holder cgroups are checked at bootstrap. Non-default-shell panes are UNPROTECTED.",
        holders.len(),
        herdr::policy::METHODS.len()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn kill(paths: &config::Paths, args: &[String]) -> Result<()> {
    let meta = holder::registry::Metadata::load(paths, &args[1])?;
    let confirmed = if args.len() == 4 {
        args[2] == "--confirm" && args[3] == meta.id
    } else {
        if !std::io::stdin().is_terminal() {
            return fail("noninteractive kill requires --confirm <exact-holder-id>");
        }
        print!(
            "Terminate holder {} and its running workload? Type the full holder ID: ",
            meta.id
        );
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim_end() == meta.id
    };
    if !confirmed {
        return fail("kill not confirmed; no process signalled");
    }
    let child = meta
        .child
        .as_ref()
        .ok_or("child identity unavailable; no process signalled")?;
    let mut stream = holder::client(paths, &meta)?;
    let frame = holder::protocol::Frame::new(
        &meta.id,
        "terminate",
        json!({"confirm":meta.id,"capability":meta.capability,"identity":child}),
    );
    holder::protocol::exchange(&mut stream, &frame)?;
    println!("terminated holder {}", meta.id);
    Ok(())
}
