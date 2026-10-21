mod caster;
mod config;
mod import;
mod receiver;
mod rtcm;
mod source;
mod ubx;

use std::io::IsTerminal;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage:
  rustk-caster [config.toml]
      Run the base station (default config: ./config.toml).
  rustk-caster import-rtkbase <settings.conf> <config.toml> [--force]
      Write a config from an RTKBase settings.conf.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let interactive = std::io::stdout().is_terminal();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(interactive)
        // journald already timestamps each line.
        .with_target(false)
        .without_time()
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.first().map(String::as_str) {
        Some("import-rtkbase") => return import::run(&args[1..]),
        Some("-V" | "--version") => {
            println!("rustk-caster {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(path) => path.to_string(),
        None => "config.toml".to_string(),
    };
    let cfg = config::load(&config_path)?;

    tracing::info!(casters = cfg.casters.len(), "starting rustk-caster");

    // ~1000 frames is several seconds of headroom at typical base station rates.
    let (tx, _) = tokio::sync::broadcast::channel::<Arc<rtcm::Frame>>(1024);

    let mut tasks = tokio::task::JoinSet::new();
    if let Some(rcv) = cfg.receiver {
        let station = Arc::new(AtomicU32::new(receiver::NO_STATION));
        tasks.spawn(receiver::generate(rcv.clone(), tx.clone(), station.clone()));
        let tx = tx.clone();
        // Plain thread: serial I/O is blocking, and it must not keep the runtime alive at exit.
        std::thread::Builder::new()
            .name("receiver".into())
            .spawn(move || receiver::run(rcv, tx, station))?;
    } else if let Some(src) = cfg.source {
        tasks.spawn(source::run(src, tx.clone()));
    }
    for caster_cfg in cfg.casters {
        tasks.spawn(caster::run(caster_cfg, tx.clone()));
    }

    tokio::select! {
        _ = tasks.join_next() => anyhow::bail!("a task exited unexpectedly"),
        _ = shutdown_signal() => tracing::info!("shutting down"),
    }
    Ok(())
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
