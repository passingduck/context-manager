use std::path::PathBuf;

use clap::Parser;

/// Shows what Claude Code, Codex CLI and Gemini CLI put in front of the model for a project
/// directory, and lets you edit it.
#[derive(Debug, Parser, PartialEq)]
#[command(name = "context-manager", version, about)]
pub struct Cli {
    /// The project directory. Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,

    /// Serve the UI over HTTP instead of opening a window.
    #[arg(long)]
    pub serve: bool,

    /// Address to bind with --serve. A non-loopback address exposes command execution to the network.
    #[arg(long, requires = "serve", default_value = "127.0.0.1")]
    pub host: String,

    /// Port to bind with --serve. 0 picks a free port.
    #[arg(long, requires = "serve", default_value_t = 7457)]
    pub port: u16,

    /// Refuse every operation that writes a file or runs a hook.
    #[arg(long)]
    pub read_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("context-manager").chain(args.iter().copied()))
    }

    #[test]
    fn defaults() {
        let cli = parse(&[]).unwrap();
        assert!(!cli.serve && !cli.read_only);
        assert_eq!((cli.host.as_str(), cli.port), ("127.0.0.1", 7457));
        assert_eq!(cli.path, None);
    }

    #[test]
    fn serve_options() {
        let cli = parse(&[
            "--serve",
            "--host",
            "0.0.0.0",
            "--port",
            "0",
            "--path",
            "/p",
            "--read-only",
        ])
        .unwrap();
        assert!(cli.serve && cli.read_only);
        assert_eq!((cli.host.as_str(), cli.port), ("0.0.0.0", 0));
        assert_eq!(cli.path, Some(PathBuf::from("/p")));
    }

    #[test]
    fn host_and_port_need_serve() {
        assert!(parse(&["--port", "9"]).is_err());
        assert!(parse(&["--host", "::1"]).is_err());
    }
}
