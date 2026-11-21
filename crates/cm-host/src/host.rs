use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cm_core::agents::{Agent, ScanCx, find_in, registry};
use cm_core::env::ScanEnv;
use cm_core::probe_fs::{EntryKind, ProbeFs, fingerprint_of};
use cm_core::scan::{Scanned, scan_with_parked};
use cm_core::*;
use cm_core::{search, simulate, texts};

use crate::apply::{Ctx, OsTrash, Trasher};
use crate::history::History;
use crate::recovery;
use crate::runner;
use crate::traces::{self, TraceMemo};

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

/// The host with `CONTEXT_MANAGER_NO_OS_TRASH` set: the OS trash is not available. `apply` then
/// takes the path of design section 4, step 4: a path the history holds completely is removed
/// and reported as not trashed, any other path stays and the operation fails.
struct NoOsTrash;

impl Trasher for NoOsTrash {
    fn trash(&self, _: &std::path::Path) -> Result<(), String> {
        Err("the OS trash is switched off".into())
    }
}

/// `no_os_trash` is `CONTEXT_MANAGER_NO_OS_TRASH`, read once by [`Host::new`].
pub(crate) fn trasher(no_os_trash: bool) -> Box<dyn Trasher> {
    if no_os_trash {
        Box::new(NoOsTrash)
    } else {
        Box::new(OsTrash)
    }
}

pub struct Host {
    config: HostConfig,
    /// Per `cwd`, the paths the last scan probed.
    probes: Mutex<HashMap<AbsPath, Vec<AbsPath>>>,
    /// What `plan`, `apply`, `history` and `change` read and write.
    pub(crate) ctx: Arc<Ctx>,
    /// The two traces opened last (design section 14).
    traces: Arc<TraceMemo>,
    /// A test stops the apply pipeline after this step, as a crash would.
    #[cfg(test)]
    pub(crate) fail_after: Arc<Mutex<Option<crate::apply::Step>>>,
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
        // `tools/e2e` sets this so that its throwaway run never reaches the OS trash.
        let no_os_trash = std::env::var_os("CONTEXT_MANAGER_NO_OS_TRASH").is_some();
        let trasher = trasher(no_os_trash);
        Host::with_parts(config, registry(), trasher)
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
            traces: Arc::new(TraceMemo::default()),
            #[cfg(test)]
            fail_after: Arc::new(Mutex::new(None)),
        };
        host.recover_now();
        host
    }

    /// Classifies the changes a crash left pending (design section 4). A read-only host leaves
    /// their records as they are. Errors are not fatal: the next history read tries again.
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

    fn agent(&self, id: &AgentId) -> Result<&'static dyn Agent, ApiError> {
        find_in(self.ctx.agents, id).ok_or(ApiError::NotFound)
    }

    async fn probed_paths(&self, cwd: &AbsPath) -> Result<Vec<AbsPath>, ApiError> {
        if let Some(paths) = self.probes.lock().expect("probe table").get(cwd) {
            return Ok(paths.clone());
        }
        Ok(self.scanned(cwd.clone()).await?.probed)
    }
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
            core_rules: cm_core::agents::core_rules().to_vec(),
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
                why: ReadOnlyReason::Host,
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

    /// Newest first. Recovery runs first, so a record a crash left behind is classified before
    /// the list is read.
    async fn history(&self, req: HistoryReq) -> Result<Vec<ChangeRecord>, ApiError> {
        let ctx = self.ctx.clone();
        let write = !self.config.read_only;
        tokio::task::spawn_blocking(move || {
            if ctx.data_dir.as_path().exists() {
                recovery::recover(&ctx, write);
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
        let write = !self.config.read_only;
        tokio::task::spawn_blocking(move || {
            if ctx.data_dir.as_path().exists() {
                recovery::recover(&ctx, write);
            }
            let record = ctx.history.record(&req.id).ok_or(ApiError::NotFound)?;
            ctx.history.detail_of(record)
        })
        .await
        .map_err(join_error)?
    }

    async fn projects(&self, _: ()) -> Result<Vec<KnownProject>, ApiError> {
        let env = self.config.env.clone();
        let agents = self.ctx.agents;
        tokio::task::spawn_blocking(move || {
            let fs = ProbeFs::new();
            let cx = ScanCx::new(&env, &fs);
            let mut known: BTreeMap<AbsPath, Vec<AgentId>> = BTreeMap::new();
            for agent in agents {
                for path in agent.known_projects(&cx) {
                    let who = known.entry(path).or_default();
                    if !who.contains(&agent.id()) {
                        who.push(agent.id());
                    }
                }
            }
            known
                .into_iter()
                .map(|(path, agents)| KnownProject {
                    exists: fs.stat(&path).is_ok_and(|stat| stat.kind == EntryKind::Dir),
                    path,
                    agents,
                })
                .collect()
        })
        .await
        .map_err(join_error)
    }

    async fn render(&self, req: RenderReq) -> Result<Vec<RenderedBlock>, ApiError> {
        let agent = self.agent(&req.agent)?;
        let scanned = self.scanned(req.cwd.clone()).await?;
        let env = self.config.env.with_cwd(req.cwd);
        tokio::task::spawn_blocking(move || {
            let view = scanned
                .snapshot
                .agents
                .iter()
                .find(|view| view.agent == agent.id())
                .ok_or(ApiError::NotFound)?;
            let cx = ScanCx::new(&env, scanned.fs());
            Ok(agent.render(&cx, view))
        })
        .await
        .map_err(join_error)?
    }

    /// Answers `Stale` when a fresh scan's fingerprint differs from the one the client shows,
    /// so a text never belongs to a different load than the one the client asked about.
    async fn load_texts(&self, req: LoadTextsReq) -> Result<LoadTexts, ApiError> {
        let agent = self.agent(&req.agent)?;
        let scanned = self.scanned(req.cwd.clone()).await?;
        if scanned.snapshot.fingerprint != req.fingerprint {
            return Ok(LoadTexts::Stale);
        }
        let env = self.config.env.with_cwd(req.cwd);
        tokio::task::spawn_blocking(move || {
            let view = scanned
                .snapshot
                .agents
                .iter()
                .find(|view| view.agent == agent.id())
                .ok_or(ApiError::NotFound)?;
            let cx = ScanCx::new(&env, scanned.fs());
            Ok(LoadTexts::Current {
                texts: texts::load_texts(agent, &cx, view, &req.file),
            })
        })
        .await
        .map_err(join_error)?
    }

    async fn simulate(&self, req: SimReq) -> Result<Vec<Simulation>, ApiError> {
        let scanned = self.scanned(req.cwd.clone()).await?;
        let env = self.config.env.with_cwd(req.cwd);
        let agents = self.ctx.agents;
        tokio::task::spawn_blocking(move || {
            let cx = ScanCx::new(&env, scanned.fs());
            simulate::simulate(agents, &cx, &scanned.snapshot, &req.path)
        })
        .await
        .map_err(join_error)
    }

    async fn search(&self, req: SearchReq) -> Result<SearchResult, ApiError> {
        let scanned = self.scanned(req.cwd.clone()).await?;
        tokio::task::spawn_blocking(move || search::search(&scanned, &req))
            .await
            .map_err(join_error)?
    }

    async fn hook_sample(&self, req: SampleReq) -> Result<serde_json::Value, ApiError> {
        let agent = self.agent(&req.agent)?;
        let env = self.config.env.with_cwd(req.cwd);
        tokio::task::spawn_blocking(move || {
            let fs = ProbeFs::new();
            agent.sample_input(&req.event, &ScanCx::new(&env, &fs))
        })
        .await
        .map_err(join_error)
    }

    /// Runs a hook of the current snapshot with the invocation its agent would use. A read-only
    /// host refuses, because a hook can do anything.
    async fn run_hook(&self, req: RunReq) -> Result<HookRun, ApiError> {
        if self.config.read_only {
            return Err(ApiError::ReadOnly {
                why: ReadOnlyReason::Host,
            });
        }
        let scanned = self.scanned(req.cwd.clone()).await?;
        let env = self.config.env.with_cwd(req.cwd);
        let agents = self.ctx.agents;
        tokio::task::spawn_blocking(move || {
            let (view, hook) = scanned.snapshot.hook(&req.hook).ok_or(ApiError::NotFound)?;
            let agent = find_in(agents, &view.agent).ok_or(ApiError::NotFound)?;
            let cx = ScanCx::new(&env, scanned.fs());
            let spec = agent
                .exec_spec(hook, &req.input, &cx)
                .map_err(|why| ApiError::Forbidden { why })?;
            runner::run(
                spec,
                &env,
                hook.handler.timeout_ms.map(Duration::from_millis),
            )
        })
        .await
        .map_err(join_error)?
    }

    async fn traces(&self, req: TracesReq) -> Result<Vec<TraceInfo>, ApiError> {
        let env = self.config.env.with_cwd(req.cwd);
        let agents = self.ctx.agents;
        tokio::task::spawn_blocking(move || traces::traces(agents, &env))
            .await
            .map_err(join_error)
    }

    async fn trace(&self, req: TraceReq) -> Result<Trace, ApiError> {
        let env = self.config.env.with_cwd(req.cwd);
        let (agents, memo) = (self.ctx.agents, self.traces.clone());
        tokio::task::spawn_blocking(move || Ok(memo.open(agents, &env, &req.key)?.trace().clone()))
            .await
            .map_err(join_error)?
    }

    /// `Stale` when the record no longer has the length the client's trace was built from.
    async fn trace_step(&self, req: TraceStepReq) -> Result<StepPieces, ApiError> {
        let env = self.config.env.with_cwd(req.cwd);
        let (agents, memo) = (self.ctx.agents, self.traces.clone());
        tokio::task::spawn_blocking(move || {
            let open = memo.open(agents, &env, &req.key)?;
            traces::step_pieces(&open, req.read, req.step)
        })
        .await
        .map_err(join_error)?
    }

    async fn trace_piece(&self, req: TracePieceReq) -> Result<PieceText, ApiError> {
        let env = self.config.env.with_cwd(req.cwd);
        let (agents, memo) = (self.ctx.agents, self.traces.clone());
        tokio::task::spawn_blocking(move || {
            let open = memo.open(agents, &env, &req.key)?;
            traces::piece_text(&open, req.read, req.step, req.piece)
        })
        .await
        .map_err(join_error)?
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

    /// A Claude Code record in the fixture home: one prompt, one request, one reply. The hashes
    /// are stand-ins, so the request resolves but is not exact.
    fn write_record(fx: &Fixture) -> AbsPath {
        let cwd = fx.project();
        let key: String = cwd
            .as_str()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let (shape, blob) = ("a".repeat(64), "b".repeat(64));
        let lines = [
            serde_json::json!({"type": "user", "cwd": cwd.as_str(), "timestamp": "2026-11-20T10:00:01.000Z",
                "message": {"role": "user", "content": "hi"}}),
            serde_json::json!({"type": "api-request-shape", "shapeHash": shape,
                "shape": {"system": [{"type": "text", "text": "a stand-in system block"}], "tools": []}}),
            serde_json::json!({"type": "api-request-blob", "hash": blob,
                "message": {"role": "user", "content": "hi"}}),
            serde_json::json!({"type": "api-request", "id": "r1", "timestamp": "2026-11-20T10:00:02.000Z",
                "shapeHash": shape, "params": {"model": "m"}, "keep": 0, "tail": [blob],
                "requestDigest": "c".repeat(64)}),
            serde_json::json!({"type": "assistant", "requestRef": "r1", "cwd": cwd.as_str(),
                "timestamp": "2026-11-20T10:00:03.000Z",
                "message": {"id": "m1", "role": "assistant", "model": "m",
                    "content": [{"type": "text", "text": "hello"}],
                    "usage": {"input_tokens": 1, "cache_read_input_tokens": 2,
                              "cache_creation_input_tokens": 3, "output_tokens": 4}}}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let file = fx.home_path(&format!(".claude/projects/{key}/s1.jsonl"));
        fx.write(&file, &text);
        file
    }

    #[tokio::test]
    async fn the_trace_operations_answer_and_a_changed_record_is_stale() {
        let fx = Fixture::new();
        let file = write_record(&fx);
        let host = host(&fx, Duration::from_millis(100));
        let cwd = serde_json::to_string(&fx.project()).unwrap();
        let key = r#"{"agent":"claude","id":"s1","part":null}"#;

        let list = call(
            &host,
            &format!(r#"{{"op":"traces","args":{{"cwd":{cwd}}}}}"#),
        )
        .await
        .unwrap();
        let list: Vec<TraceInfo> = serde_json::from_value(list).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, TraceId("s1".into()));
        assert_eq!(list[0].title.as_deref(), Some("hi"));

        let trace = call(
            &host,
            &format!(r#"{{"op":"trace","args":{{"cwd":{cwd},"key":{key}}}}}"#),
        )
        .await
        .unwrap();
        let trace: Trace = serde_json::from_value(trace).unwrap();
        let read = std::fs::metadata(file.as_path()).unwrap().len();
        assert_eq!(trace.read, read);
        assert_eq!(trace.steps.len(), 1);
        assert_eq!(trace.problems, vec![TraceProblem::Inexact { steps: 1 }]);

        let step = |read: u64| {
            format!(
                r#"{{"op":"trace_step","args":{{"cwd":{cwd},"key":{key},"read":{read},"step":0}}}}"#
            )
        };
        let piece = |read: u64, piece: &str| {
            format!(
                r#"{{"op":"trace_piece","args":{{"cwd":{cwd},"key":{key},"read":{read},"step":0,"piece":{piece}}}}}"#
            )
        };
        let pieces: StepPieces =
            serde_json::from_value(call(&host, &step(read)).await.unwrap()).unwrap();
        let StepPieces::Current { pieces } = pieces else {
            panic!("the record did not change");
        };
        let kinds: Vec<PieceKind> = pieces.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, [PieceKind::System, PieceKind::UserText]);
        let sent: PieceText = serde_json::from_value(
            call(&host, &piece(read, r#"{"kind":"sent","piece":1}"#))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            sent,
            PieceText::Current {
                text: "hi".into(),
                bytes: 2,
                cut: false
            }
        );
        let reply: PieceText = serde_json::from_value(
            call(&host, &piece(read, r#"{"kind":"reply","piece":0}"#))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reply,
            PieceText::Current {
                text: "hello".into(),
                bytes: 5,
                cut: false
            }
        );
        let missing = call(&host, &piece(read, r#"{"kind":"sent","piece":9}"#)).await;
        assert_eq!(missing.err(), Some(ApiError::NotFound));

        // The session goes on: the record grows, and a request naming the old length is stale.
        let mut bytes = std::fs::read(file.as_path()).unwrap();
        bytes.extend_from_slice(
            b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"more\"}}\n",
        );
        std::fs::write(file.as_path(), &bytes).unwrap();
        let stale: StepPieces =
            serde_json::from_value(call(&host, &step(read)).await.unwrap()).unwrap();
        assert_eq!(stale, StepPieces::Stale);
        let stale: PieceText = serde_json::from_value(
            call(&host, &piece(read, r#"{"kind":"reply","piece":0}"#))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stale, PieceText::Stale);
        let fresh: Trace = serde_json::from_value(
            call(
                &host,
                &format!(r#"{{"op":"trace","args":{{"cwd":{cwd},"key":{key}}}}}"#),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fresh.read, bytes.len() as u64);

        let unknown = r#"{"agent":"claude","id":"../s1","part":null}"#;
        let missing = call(
            &host,
            &format!(r#"{{"op":"trace","args":{{"cwd":{cwd},"key":{unknown}}}}}"#),
        )
        .await;
        assert_eq!(missing.err(), Some(ApiError::NotFound));
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

    /// A fixture with a Claude project: instructions, a nested file, and two command hooks.
    fn hooked_fixture() -> Fixture {
        let fx = Fixture::new();
        fx.write(
            &fx.project_path("CLAUDE.md"),
            "# Rules\nSee [a](docs/a.md)\n",
        );
        fx.write(&fx.project_path("docs/a.md"), "about a\n");
        fx.write(&fx.project_path("sub/CLAUDE.md"), "nested rules\n");
        let hook = |command: &str, timeout: Option<u64>| {
            let mut handler = serde_json::json!({"type": "command", "command": command});
            if let Some(seconds) = timeout {
                handler["timeout"] = seconds.into();
            }
            serde_json::json!({"hooks": [handler]})
        };
        let settings = serde_json::json!({"hooks": {"Stop": [
            hook("cat; echo failed >&2; exit 2", None),
            hook("sleep 30 & echo $! > child.pid; wait", Some(1)),
        ]}});
        fx.write(
            &fx.project_path(".claude/settings.json"),
            &settings.to_string(),
        );
        fx
    }

    fn host_with_path(fx: &Fixture, read_only: bool) -> Host {
        Host::new(HostConfig {
            shell: Shell::Serve,
            read_only,
            env: fx.env().with_var("PATH", "/usr/bin:/bin"),
            watch_timeout: Duration::from_millis(100),
            data_dir: fx.root().join("data"),
        })
    }

    #[tokio::test]
    async fn no_operation_answers_not_implemented() {
        let fx = hooked_fixture();
        let host = host_with_path(&fx, false);
        let (cwd, nested) = (fx.project(), fx.project_path("sub/CLAUDE.md"));
        let snapshot = host.scan(ScanReq { cwd: cwd.clone() }).await.unwrap();
        let hook = snapshot.agents[0].hooks[0].id.clone();
        let claude = AgentId::parse("claude").unwrap();
        let key = TraceKey {
            agent: claude.clone(),
            id: TraceId("none".into()),
            part: None,
        };
        let requests = vec![
            Request::Session(()),
            Request::Agents(()),
            Request::Projects(()),
            Request::Scan(ScanReq { cwd: cwd.clone() }),
            Request::ReadFile(ReadReq {
                cwd: cwd.clone(),
                path: nested.clone(),
            }),
            Request::Render(RenderReq {
                cwd: cwd.clone(),
                agent: claude.clone(),
            }),
            Request::LoadTexts(LoadTextsReq {
                cwd: cwd.clone(),
                agent: claude.clone(),
                file: nested.clone(),
                fingerprint: snapshot.fingerprint.clone(),
            }),
            Request::Simulate(SimReq {
                cwd: cwd.clone(),
                path: nested.clone(),
            }),
            Request::Search(SearchReq {
                cwd: cwd.clone(),
                query: "rules".into(),
                regex: false,
                case_sensitive: false,
            }),
            Request::Watch(WatchReq {
                cwd: cwd.clone(),
                since: snapshot.fingerprint.clone(),
            }),
            Request::Plan(PlanReq {
                cwd: cwd.clone(),
                edit: Edit::Save {
                    path: nested.clone(),
                    base: Base::Absent,
                    text: String::new(),
                },
            }),
            Request::Apply(ApplyReq {
                cwd: cwd.clone(),
                edit: Edit::Revert {
                    change: ChangeId("none".into()),
                },
                digest: String::new(),
            }),
            Request::History(HistoryReq {
                cwd: None,
                before: None,
                limit: 1,
            }),
            Request::Change(ChangeReq {
                id: ChangeId("none".into()),
            }),
            Request::HookSample(SampleReq {
                cwd: cwd.clone(),
                agent: claude,
                event: "Stop".into(),
            }),
            Request::RunHook(RunReq {
                cwd: cwd.clone(),
                hook,
                input: serde_json::json!({}),
            }),
            Request::Traces(TracesReq { cwd: cwd.clone() }),
            Request::Trace(TraceReq {
                cwd: cwd.clone(),
                key: key.clone(),
            }),
            Request::TraceStep(TraceStepReq {
                cwd: cwd.clone(),
                key: key.clone(),
                read: 0,
                step: 0,
            }),
            Request::TracePiece(TracePieceReq {
                cwd,
                key,
                read: 0,
                step: 0,
                piece: PieceRef::Sent { piece: 0 },
            }),
        ];
        assert_eq!(
            requests.len(),
            cm_core::api::operations().len(),
            "one request per operation"
        );
        for request in requests {
            let name = serde_json::to_value(&request).unwrap()["op"].clone();
            let outcome = dispatch(&host, request).await;
            assert_ne!(
                outcome,
                Err(ApiError::Io {
                    detail: "not implemented".to_owned()
                }),
                "{name} is not implemented"
            );
        }
    }

    #[tokio::test]
    async fn load_texts_answers_for_the_snapshot_it_names_or_says_stale() {
        let fx = hooked_fixture();
        let host = host_with_path(&fx, false);
        let cwd = fx.project();
        let file = fx.project_path("CLAUDE.md");
        let snapshot = host.scan(ScanReq { cwd: cwd.clone() }).await.unwrap();
        let req = |fingerprint: Fingerprint| LoadTextsReq {
            cwd: cwd.clone(),
            agent: AgentId::parse("claude").unwrap(),
            file: file.clone(),
            fingerprint,
        };
        let LoadTexts::Current { texts } = host
            .load_texts(req(snapshot.fingerprint.clone()))
            .await
            .unwrap()
        else {
            panic!("the snapshot is current");
        };
        assert_eq!(texts.len(), 1);
        assert_eq!(
            texts[0].text.as_deref(),
            Some("# Rules\nSee [a](docs/a.md)")
        );
        assert!(texts[0].whole_file);

        fx.write(&file, "# Rules, changed\n");
        assert_eq!(
            host.load_texts(req(snapshot.fingerprint)).await.unwrap(),
            LoadTexts::Stale
        );
    }

    #[tokio::test]
    async fn render_search_projects_and_sample_answer_from_a_fixture() {
        let fx = hooked_fixture();
        fx.write(
            &fx.home_path(".claude.json"),
            &serde_json::json!({"projects": {fx.project().as_str(): {}, "/no/such/dir": {}}})
                .to_string(),
        );
        let host = host_with_path(&fx, false);
        let cwd = fx.project();

        let blocks = host
            .render(RenderReq {
                cwd: cwd.clone(),
                agent: AgentId::parse("claude").unwrap(),
            })
            .await
            .unwrap();
        assert!(blocks.iter().any(|b| b.text().contains("# Rules")));

        let found = host
            .search(SearchReq {
                cwd: cwd.clone(),
                query: "NESTED".into(),
                regex: false,
                case_sensitive: false,
            })
            .await
            .unwrap();
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].file, fx.project_path("sub/CLAUDE.md"));

        let projects = host.projects(()).await.unwrap();
        let known: Vec<_> = projects
            .iter()
            .map(|p| (p.path.as_str().to_owned(), p.exists))
            .collect();
        assert_eq!(
            known,
            [
                ("/no/such/dir".to_owned(), false),
                (fx.project().as_str().to_owned(), true)
            ]
        );
        assert_eq!(projects[1].agents[0].as_str(), "claude");

        let sample = host
            .hook_sample(SampleReq {
                cwd,
                agent: AgentId::parse("claude").unwrap(),
                event: "Stop".into(),
            })
            .await
            .unwrap();
        assert_eq!(sample["hook_event_name"], "Stop");

        let all = host
            .simulate(SimReq {
                cwd: fx.project(),
                path: fx.project_path("sub/x.rs"),
            })
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].added_tokens > 0, "the nested CLAUDE.md fires");
    }

    #[tokio::test]
    async fn a_hook_that_echoes_stdin_and_exits_2_reports_both() {
        let fx = hooked_fixture();
        let host = host_with_path(&fx, false);
        let snapshot = host.scan(ScanReq { cwd: fx.project() }).await.unwrap();
        let echo = snapshot.agents[0]
            .hooks
            .iter()
            .find(|h| h.handler.summary.starts_with("cat"))
            .unwrap();
        let run = host
            .run_hook(RunReq {
                cwd: fx.project(),
                hook: echo.id.clone(),
                input: serde_json::json!({"hello": "world"}),
            })
            .await
            .unwrap();
        assert_eq!(run.exit_code, Some(2));
        assert_eq!(run.stdout, r#"{"hello":"world"}"#);
        assert_eq!(run.stderr.trim(), "failed");
        assert!(!run.timed_out);
    }

    #[tokio::test]
    async fn a_hook_past_its_timeout_is_killed_with_its_children() {
        let fx = hooked_fixture();
        let host = host_with_path(&fx, false);
        let snapshot = host.scan(ScanReq { cwd: fx.project() }).await.unwrap();
        let slow = snapshot.agents[0]
            .hooks
            .iter()
            .find(|h| h.handler.summary.starts_with("sleep"))
            .unwrap();
        let started = std::time::Instant::now();
        let run = host
            .run_hook(RunReq {
                cwd: fx.project(),
                hook: slow.id.clone(),
                input: serde_json::json!({}),
            })
            .await
            .unwrap();
        assert!(run.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );

        let pid: i32 = std::fs::read_to_string(fx.project_path("child.pid").as_path())
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = (0..100).any(|_| {
            // SAFETY: signal 0 only checks whether the process exists.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                std::thread::sleep(Duration::from_millis(20));
            }
            !alive
        });
        assert!(gone, "the sleeping child {pid} outlived the timeout");
    }

    #[tokio::test]
    async fn a_read_only_host_refuses_to_run_a_hook() {
        let fx = hooked_fixture();
        let host = host_with_path(&fx, true);
        let err = host
            .run_hook(RunReq {
                cwd: fx.project(),
                hook: HookId("x".into()),
                input: serde_json::json!({}),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::ReadOnly { .. }));
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
