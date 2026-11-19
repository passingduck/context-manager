use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cm_core::agents::{Agent, registry};
use cm_core::env::ScanEnv;
use cm_core::probe_fs::fingerprint_of;
use cm_core::scan::{Scanned, scan_with_parked};
use cm_core::*;

use crate::apply::{Ctx, OsTrash, Trasher};
use crate::history::History;
use crate::recovery;

/// How long `watch` waits before it answers with the unchanged fingerprint (design section 8).
pub const WATCH_TIMEOUT: Duration = Duration::from_secs(25);
const WATCH_TICK: Duration = Duration::from_secs(1);

pub struct HostConfig {
    pub shell: Shell,
    pub read_only: bool,
    /// The environment a scan starts from. Its `cwd` is the launch directory.
    pub env: ScanEnv,
    pub watch_timeout: Duration,
    /// The tool's own data directory: history and parked hooks. Created with mode 0700.
    pub data_dir: AbsPath,
}

/// `CONTEXT_MANAGER_DATA_DIR`, else the platform data directory:
/// `~/Library/Application Support/context-manager` on macOS, `$XDG_DATA_HOME/context-manager`
/// (default `~/.local/share`) elsewhere. The process environment is read here, once.
pub fn default_data_dir(env: &ScanEnv) -> Result<AbsPath, PathError> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let Some(dir) = var("CONTEXT_MANAGER_DATA_DIR") {
        return AbsPath::parse(&dir);
    }
    if env.os == Os::Macos {
        return Ok(env.home.join("Library/Application Support/context-manager"));
    }
    match var("XDG_DATA_HOME") {
        Some(base) => Ok(AbsPath::parse(&base)?.join("context-manager")),
        None => Ok(env.home.join(".local/share/context-manager")),
    }
}

impl HostConfig {
    /// Reads the process environment, once.
    pub fn from_process(
        shell: Shell,
        read_only: bool,
        launch_cwd: AbsPath,
    ) -> Result<HostConfig, PathError> {
        let env = ScanEnv::from_process(launch_cwd)?;
        Ok(HostConfig {
            shell,
            read_only,
            data_dir: default_data_dir(&env)?,
            env,
            watch_timeout: WATCH_TIMEOUT,
        })
    }
}

pub struct Host {
    config: HostConfig,
    /// Per `cwd`, the paths the last scan probed.
    probes: Mutex<HashMap<AbsPath, Vec<AbsPath>>>,
    /// What `plan`, `apply`, `history` and `change` read and write.
    pub(crate) ctx: Arc<Ctx>,
    /// A test stops the apply pipeline after this step, as a crash would.
    #[cfg(test)]
    pub(crate) fail_after: Arc<Mutex<Option<crate::apply::Step>>>,
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

/// How many records `history` returns when the request asks for none.
const DEFAULT_HISTORY_LIMIT: usize = 50;
const MAX_HISTORY_LIMIT: usize = 500;

impl Host {
    pub fn new(config: HostConfig) -> Host {
        Host::with_parts(config, registry(), Box::new(OsTrash))
    }

    /// The agents and the trash are the two things a test replaces.
    pub(crate) fn with_parts(
        config: HostConfig,
        agents: &'static [&'static dyn Agent],
        trasher: Box<dyn Trasher>,
    ) -> Host {
        let data_dir = config.data_dir.clone();
        let ctx = Arc::new(Ctx {
            agents,
            env: config.env.clone(),
            history: History::open(data_dir.as_path()),
            parked_dir: data_dir.join("parked"),
            data_dir,
            trasher,
        });
        let host = Host {
            config,
            probes: Mutex::new(HashMap::new()),
            ctx,
            #[cfg(test)]
            fail_after: Arc::new(Mutex::new(None)),
        };
        host.recover_now();
        host
    }

    /// Finishes or flags the changes a crash left pending (design section 4). A read-only host
    /// only classifies them. Errors are not fatal: the next history read tries again.
    fn recover_now(&self) {
        if self.ctx.data_dir.as_path().exists() {
            recovery::recover(&self.ctx, !self.config.read_only);
        }
    }

    /// Runs a scan off the async threads and remembers what it probed.
    async fn scanned(&self, cwd: AbsPath) -> Result<Scanned, ApiError> {
        let env = self.config.env.with_cwd(cwd.clone());
        let agents = self.ctx.agents;
        let parked = self.ctx.parked_dir.clone();
        let scanned =
            tokio::task::spawn_blocking(move || scan_with_parked(agents, &env, Some(&parked)))
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
            pending: self
                .ctx
                .history
                .pending()
                .into_iter()
                .map(|record| record.id)
                .collect(),
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

    async fn plan(&self, req: PlanReq) -> Result<Preview, ApiError> {
        let ctx = self.ctx.clone();
        tokio::task::spawn_blocking(move || ctx.plan(&req.cwd, &req.edit))
            .await
            .map_err(join_error)?
    }

    /// Step 1 of the pipeline, the policy: a read-only host refuses here and nowhere else.
    async fn apply(&self, req: ApplyReq) -> Result<ApplyResult, ApiError> {
        if self.config.read_only {
            return Err(ApiError::ReadOnly {
                why: RuleRef::new("host.read_only"),
            });
        }
        let ctx = self.ctx.clone();
        #[cfg(test)]
        let fail_after = self.fail_after.clone();
        tokio::task::spawn_blocking(move || {
            ctx.apply(&req.cwd, &req.edit, &req.digest, &|step| {
                #[cfg(test)]
                if *fail_after.lock().expect("failure point") == Some(step) {
                    return Err(ApiError::Io {
                        detail: format!("injected failure after {step:?}"),
                    });
                }
                let _ = step;
                Ok(())
            })
        })
        .await
        .map_err(join_error)?
    }

    /// Newest first. Recovery runs first, so a record a crash left behind is finished or flagged
    /// before the list is read.
    async fn history(&self, req: HistoryReq) -> Result<Vec<ChangeRecord>, ApiError> {
        let ctx = self.ctx.clone();
        let roll_forward = !self.config.read_only;
        tokio::task::spawn_blocking(move || {
            if ctx.data_dir.as_path().exists() {
                recovery::recover(&ctx, roll_forward);
            }
            let limit = match req.limit as usize {
                0 => DEFAULT_HISTORY_LIMIT,
                n => n.min(MAX_HISTORY_LIMIT),
            };
            ctx.history
                .all()
                .into_iter()
                .filter(|r| req.cwd.is_none() || r.cwd == req.cwd)
                .filter(|r| req.before.as_ref().is_none_or(|before| r.id < *before))
                .take(limit)
                .collect()
        })
        .await
        .map_err(join_error)
    }

    async fn change(&self, req: ChangeReq) -> Result<ChangeDetail, ApiError> {
        let ctx = self.ctx.clone();
        let roll_forward = !self.config.read_only;
        tokio::task::spawn_blocking(move || {
            if ctx.data_dir.as_path().exists() {
                recovery::recover(&ctx, roll_forward);
            }
            let record = ctx.history.record(&req.id).ok_or(ApiError::NotFound)?;
            ctx.history.detail_of(record)
        })
        .await
        .map_err(join_error)?
    }

    not_implemented! {
        projects(()) -> Vec<KnownProject>;
        render(RenderReq) -> Vec<RenderedBlock>;
        simulate(SimReq) -> Vec<Simulation>;
        search(SearchReq) -> SearchResult;
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
            data_dir: fx.root().join("data"),
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
