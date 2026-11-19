use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use cm_core::agents::registry;
use cm_core::env::ScanEnv;
use cm_core::probe_fs::fingerprint_of;
use cm_core::scan::{Scanned, scan_full};
use cm_core::*;

/// How long `watch` waits before it answers with the unchanged fingerprint (design section 8).
pub const WATCH_TIMEOUT: Duration = Duration::from_secs(25);
const WATCH_TICK: Duration = Duration::from_secs(1);

pub struct HostConfig {
    pub shell: Shell,
    pub read_only: bool,
    /// The environment a scan starts from. Its `cwd` is the launch directory.
    pub env: ScanEnv,
    pub watch_timeout: Duration,
}

impl HostConfig {
    /// Reads the process environment, once.
    pub fn from_process(
        shell: Shell,
        read_only: bool,
        launch_cwd: AbsPath,
    ) -> Result<HostConfig, PathError> {
        Ok(HostConfig {
            shell,
            read_only,
            env: ScanEnv::from_process(launch_cwd)?,
            watch_timeout: WATCH_TIMEOUT,
        })
    }
}

pub struct Host {
    config: HostConfig,
    /// Per `cwd`, the paths the last scan probed.
    probes: Mutex<HashMap<AbsPath, Vec<AbsPath>>>,
}

fn not_implemented() -> ApiError {
    ApiError::Io {
        detail: "not implemented".to_owned(),
    }
}

fn join_error(err: tokio::task::JoinError) -> ApiError {
    ApiError::Io {
        detail: err.to_string(),
    }
}

impl Host {
    pub fn new(config: HostConfig) -> Host {
        Host {
            config,
            probes: Mutex::new(HashMap::new()),
        }
    }

    /// Runs a scan off the async threads and remembers what it probed.
    async fn scanned(&self, cwd: AbsPath) -> Result<Scanned, ApiError> {
        let env = self.config.env.with_cwd(cwd.clone());
        let scanned = tokio::task::spawn_blocking(move || scan_full(&env))
            .await
            .map_err(join_error)?;
        self.probes
            .lock()
            .expect("probe table")
            .insert(cwd, scanned.probed.clone());
        Ok(scanned)
    }

    async fn probed_paths(&self, cwd: &AbsPath) -> Result<Vec<AbsPath>, ApiError> {
        if let Some(paths) = self.probes.lock().expect("probe table").get(cwd) {
            return Ok(paths.clone());
        }
        Ok(self.scanned(cwd.clone()).await?.probed)
    }
}

macro_rules! not_implemented {
    ($($method:ident($args:ty) -> $ret:ty;)*) => {$(
        async fn $method(&self, _: $args) -> Result<$ret, ApiError> {
            Err(not_implemented())
        }
    )*};
}

impl Api for Host {
    async fn session(&self, _: ()) -> Result<Session, ApiError> {
        Ok(Session {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            shell: self.config.shell,
            read_only: self.config.read_only,
            launch_cwd: self.config.env.cwd.clone(),
            home: self.config.env.home.clone(),
            os: self.config.env.os,
            pending: Vec::new(),
        })
    }

    async fn agents(&self, _: ()) -> Result<Vec<AgentInfo>, ApiError> {
        Ok(registry()
            .iter()
            .map(|agent| AgentInfo {
                id: agent.id(),
                display_name: agent.display_name().to_owned(),
                rules: agent.rules().to_vec(),
                hook_schema: agent.hook_schema().clone(),
            })
            .collect())
    }

    async fn scan(&self, req: ScanReq) -> Result<Snapshot, ApiError> {
        Ok(self.scanned(req.cwd).await?.snapshot)
    }

    async fn read_file(&self, req: ReadReq) -> Result<FileContent, ApiError> {
        let scanned = self.scanned(req.cwd).await?;
        scanned
            .file_content(&req.path)
            .ok_or(ApiError::OutsideBoundary { path: req.path })
    }

    /// A long poll (design section 8): returns when the fingerprint of the probed paths differs
    /// from `since`, or after the timeout with the unchanged fingerprint.
    async fn watch(&self, req: WatchReq) -> Result<Fingerprint, ApiError> {
        let paths = self.probed_paths(&req.cwd).await?;
        let deadline = tokio::time::Instant::now() + self.config.watch_timeout;
        loop {
            let probe = paths.clone();
            let now = tokio::task::spawn_blocking(move || fingerprint_of(&probe))
                .await
                .map_err(join_error)?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if now != req.since || remaining.is_zero() {
                return Ok(now);
            }
            tokio::time::sleep(WATCH_TICK.min(remaining)).await;
        }
    }

    not_implemented! {
        projects(()) -> Vec<KnownProject>;
        render(RenderReq) -> Vec<RenderedBlock>;
        simulate(SimReq) -> Vec<Simulation>;
        search(SearchReq) -> SearchResult;
        plan(PlanReq) -> Preview;
        apply(ApplyReq) -> ApplyResult;
        history(HistoryReq) -> Vec<ChangeRecord>;
        change(ChangeReq) -> ChangeDetail;
        hook_sample(SampleReq) -> serde_json::Value;
        run_hook(RunReq) -> HookRun;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_core::testing::Fixture;

    fn host(fx: &Fixture, watch_timeout: Duration) -> Host {
        Host::new(HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: fx.env(),
            watch_timeout,
        })
    }

    async fn call(host: &Host, json: &str) -> Result<serde_json::Value, ApiError> {
        dispatch(host, serde_json::from_str(json).expect("request parses")).await
    }

    #[tokio::test]
    async fn session_agents_and_scan_answer_through_dispatch() {
        let fx = Fixture::new();
        let host = host(&fx, WATCH_TIMEOUT);

        let session = call(&host, r#"{"op":"session","args":null}"#)
            .await
            .unwrap();
        assert_eq!(session["shell"], "serve");
        assert_eq!(session["launchCwd"], fx.project().as_str());

        let agents = call(&host, r#"{"op":"agents","args":null}"#).await.unwrap();
        assert_eq!(agents.as_array().unwrap().len(), 3);
        assert_eq!(agents[1]["displayName"], "Codex CLI");

        let scan = call(
            &host,
            &format!(r#"{{"op":"scan","args":{{"cwd":"{}"}}}}"#, fx.project()),
        )
        .await
        .unwrap();
        assert_eq!(scan["agents"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn read_file_refuses_a_path_outside_the_snapshot() {
        let fx = Fixture::new();
        let host = host(&fx, WATCH_TIMEOUT);
        let outside = fx.home_path("secret.md");
        fx.write(&outside, "x");
        let err = call(
            &host,
            &format!(
                r#"{{"op":"read_file","args":{{"cwd":"{}","path":"{}"}}}}"#,
                fx.project(),
                outside
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApiError::OutsideBoundary { .. }));
    }

    #[tokio::test]
    async fn an_operation_without_an_implementation_says_so() {
        let fx = Fixture::new();
        let err = call(
            &host(&fx, WATCH_TIMEOUT),
            r#"{"op":"projects","args":null}"#,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            ApiError::Io {
                detail: "not implemented".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn watch_waits_for_a_change_and_times_out_unchanged() {
        let fx = Fixture::new();
        fx.mkdir(&fx.home_path(".claude"));
        let host = host(&fx, Duration::from_millis(1500));
        let since = host
            .scan(ScanReq { cwd: fx.project() })
            .await
            .unwrap()
            .fingerprint;

        let started = std::time::Instant::now();
        let quiet = host
            .watch(WatchReq {
                cwd: fx.project(),
                since: since.clone(),
            })
            .await
            .unwrap();
        assert_eq!(quiet, since);
        assert!(
            started.elapsed() >= Duration::from_millis(1400),
            "returned early: {:?}",
            started.elapsed()
        );

        let writer = fx.home_path(".claude/CLAUDE.md");
        let write = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            std::fs::write(writer.as_path(), "new").unwrap();
        });
        let started = std::time::Instant::now();
        let changed = host
            .watch(WatchReq {
                cwd: fx.project(),
                since: since.clone(),
            })
            .await
            .unwrap();
        write.await.unwrap();
        assert_ne!(changed, since);
        assert!(started.elapsed() < Duration::from_millis(1400));
    }
}
