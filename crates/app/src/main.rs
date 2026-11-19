mod cli;
#[cfg(feature = "desktop")]
mod desktop;
mod serve;

use std::process::ExitCode;
use std::sync::Arc;
#[cfg(feature = "desktop")]
use std::time::Duration;

use clap::Parser;
use cm_core::{AbsPath, Shell};
use cm_host::{Host, HostConfig};

use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();

    #[cfg(not(feature = "desktop"))]
    if !cli.serve {
        eprintln!(
            "context-manager: this build has no window. Run it with --serve to use the web UI."
        );
        return ExitCode::from(2);
    }

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("context-manager: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let launch_cwd = launch_directory(cli.path.as_deref())?;
    let shell = if cli.serve {
        Shell::Serve
    } else {
        Shell::Desktop
    };
    let config =
        HostConfig::from_process(shell, cli.read_only, launch_cwd).map_err(|e| e.to_string())?;

    if cli.serve {
        let host = Arc::new(Host::new(config));
        let token = serve::token(std::env::var("CONTEXT_MANAGER_TOKEN").ok());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        return runtime
            .block_on(serve::run(host, &cli.host, cli.port, token))
            .map_err(|e| format!("cannot serve on {}:{}: {e}", cli.host, cli.port));
    }

    run_desktop(config)
}

#[cfg(feature = "desktop")]
fn run_desktop(mut config: HostConfig) -> Result<(), String> {
    let selftest = std::env::var_os("CM_SELFTEST").map(std::path::PathBuf::from);
    if selftest.is_some() {
        // The self-test needs a `watch` that waits, not one that waits 25 seconds.
        config.watch_timeout = Duration::from_secs(2);
    }
    desktop::run(Arc::new(Host::new(config)), selftest)
}

#[cfg(not(feature = "desktop"))]
fn run_desktop(_: HostConfig) -> Result<(), String> {
    Err("this build has no window".to_owned())
}

/// `--path`, or the current directory, as an absolute and existing directory.
fn launch_directory(path: Option<&std::path::Path>) -> Result<AbsPath, String> {
    let raw = match path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?,
    };
    let real = std::fs::canonicalize(&raw).map_err(|e| format!("{}: {e}", raw.display()))?;
    if !real.is_dir() {
        return Err(format!("{} is not a directory", raw.display()));
    }
    let text = real
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", real.display()))?;
    AbsPath::parse(text).map_err(|e| e.to_string())
}
