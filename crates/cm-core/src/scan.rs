//! `scan(&ScanEnv) -> Snapshot`: runs the adapters over one `ProbeFs`, interns files, and sets
//! the fingerprint. A scan is a function of the filesystem and the environment.

use std::collections::BTreeMap;

use crate::agents::{Agent, FileClaim, ScanCx, registry};
use crate::env::ScanEnv;
use crate::model::*;
pub use crate::probe_fs::MAX_READ_BYTES;
use crate::probe_fs::{ProbeErrorKind, ProbeFs};
use crate::{doctor, passes, walk};

/// A snapshot plus what the host needs to answer follow-up calls about the same scan.
pub struct Scanned {
    pub snapshot: Snapshot,
    /// Every path the scan looked at. `watch` re-stats these.
    pub probed: Vec<AbsPath>,
    fs: ProbeFs,
}

impl Scanned {
    /// The cache this scan read through. A plan reads the same bytes the snapshot describes.
    pub fn fs(&self) -> &ProbeFs {
        &self.fs
    }

    /// The text of a file in the snapshot, as the scan saw it. `None` when the path is not in it.
    pub fn file_content(&self, path: &AbsPath) -> Option<FileContent> {
        let entry = self.snapshot.files.get(path)?;
        let text = match entry.state {
            FileState::Present { .. } => self
                .fs
                .read(path)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes.as_ref().clone()).ok()),
            _ => None,
        };
        Some(FileContent {
            path: path.clone(),
            state: entry.state.clone(),
            text,
        })
    }
}

pub fn scan(env: &ScanEnv) -> Snapshot {
    scan_full(env).snapshot
}

pub fn scan_full(env: &ScanEnv) -> Scanned {
    scan_with(registry(), env)
}

pub fn scan_with(agents: &[&dyn Agent], env: &ScanEnv) -> Scanned {
    scan_with_parked(agents, env, None)
}

/// [`scan_with`] that also reads the parked hook records in `parked_dir`.
pub fn scan_with_parked(
    agents: &[&dyn Agent],
    env: &ScanEnv,
    parked_dir: Option<&AbsPath>,
) -> Scanned {
    let fs = ProbeFs::new();
    let cx = ScanCx {
        env,
        fs: &fs,
        parked_dir,
    };
    let git_root = find_git_root(&cx);
    let project_root = git_root.clone().unwrap_or_else(|| env.cwd.clone());

    let mut views = Vec::new();
    let mut claimed = Vec::new();
    let mut diagnostics = Vec::new();
    for agent in agents {
        let scan = agent.scan(&cx);
        views.push(scan.view);
        claimed.push(scan.files);
        diagnostics.extend(scan.diagnostics);
    }

    // The passes that run after the adapters (design section 5).
    diagnostics.extend(passes::reference_graph(&cx, &project_root, &mut views));
    for view in &mut views {
        passes::budget(view);
        passes::unsized_gaps(view);
    }
    let index = walk::walk(&env.cwd, &fs);
    let files = intern(&cx, &views, &claimed);
    let found = doctor::check(&doctor::Input {
        cx: &cx,
        agents,
        views: &views,
        files: &files,
        project_root: &project_root,
        walk_capped: index.capped,
        existing: &diagnostics,
    });
    diagnostics.extend(found);

    let snapshot = Snapshot {
        cwd: env.cwd.clone(),
        git_root,
        fingerprint: fs.fingerprint(),
        files,
        agents: views,
        diagnostics,
        walk: WalkSummary {
            dirs: index.dirs().count() as u32,
            files: index.files().count() as u32,
            capped: index.capped,
        },
    };
    let probed = fs.probed();
    Scanned {
        snapshot,
        probed,
        fs,
    }
}

/// The nearest ancestor of cwd (cwd included) that holds a `.git` entry.
fn find_git_root(cx: &ScanCx) -> Option<AbsPath> {
    let mut dir = Some(cx.env.cwd.clone());
    while let Some(current) = dir {
        if cx.fs.stat(&current.join(".git")).is_ok() {
            return Some(current);
        }
        dir = current.parent();
    }
    None
}

/// Builds the file table: every claimed file, hook file and loaded file, once.
fn intern(
    cx: &ScanCx,
    views: &[AgentView],
    claimed: &[Vec<FileClaim>],
) -> BTreeMap<AbsPath, FileEntry> {
    struct Claim {
        agents: Vec<AgentId>,
        policy: EditPolicy,
    }
    let mut claims: BTreeMap<AbsPath, Claim> = BTreeMap::new();
    let mut claim = |path: &AbsPath, agent: &AgentId, policy: EditPolicy| {
        let entry = claims.entry(path.clone()).or_insert(Claim {
            agents: Vec::new(),
            policy: EditPolicy::Editable,
        });
        if !entry.agents.contains(agent) {
            entry.agents.push(agent.clone());
        }
        if entry.policy == EditPolicy::Editable {
            entry.policy = policy;
        }
    };
    for (view, files) in views.iter().zip(claimed) {
        for file in files {
            claim(&file.path, &view.agent, file.policy.clone());
        }
        // A hook file carries the policy of the extension or settings layer it belongs to, so
        // the planner sees it as any other file.
        for file in &view.hook_files {
            claim(&file.file, &view.agent, file.policy.clone());
        }
        for load in &view.loads {
            claim(&load.file, &view.agent, EditPolicy::Editable);
        }
    }

    claims
        .into_iter()
        .map(|(path, claim)| {
            let mut entry = read_entry(cx, &path, claim.policy);
            entry.agents = claim.agents;
            (path, entry)
        })
        .collect()
}

fn read_entry(cx: &ScanCx, path: &AbsPath, claimed: EditPolicy) -> FileEntry {
    let format = format_of(path);
    let mut policy = claimed;
    let mut format = format;
    let stat = cx.fs.stat(path);
    let real_path = match &stat {
        Ok(stat) if stat.is_symlink => cx.fs.real_path(path).ok(),
        _ => None,
    };
    let state = match stat {
        Err(err) if err.is_not_found() => FileState::Absent,
        Err(err) => FileState::Unreadable { error: err.detail },
        Ok(stat) => match cx.fs.read(path) {
            Err(err) => match err.kind {
                ProbeErrorKind::TooLarge { bytes } => FileState::TooBig { bytes },
                _ => FileState::Unreadable { error: err.detail },
            },
            Ok(bytes) => {
                if std::str::from_utf8(&bytes).is_err() {
                    format = Format::Binary;
                    if policy == EditPolicy::Editable {
                        policy = EditPolicy::ReadOnly {
                            why: ReadOnlyWhy::NotUtf8,
                        };
                    }
                }
                let rev = Revision::of(&bytes);
                FileState::Present {
                    tokens: crate::tokens::count_cached(&rev, &bytes),
                    rev,
                    bytes: bytes.len() as u64,
                    lines: count_lines(&bytes),
                    mtime_ms: stat.mtime_ms,
                }
            }
        },
    };
    FileEntry {
        display: display_of(cx.env, path),
        real_path,
        format,
        state,
        policy,
        agents: Vec::new(),
    }
}

fn count_lines(bytes: &[u8]) -> u32 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count();
    let unterminated = !bytes.is_empty() && bytes.last() != Some(&b'\n');
    (newlines + usize::from(unterminated)) as u32
}

pub(crate) fn format_of(path: &AbsPath) -> Format {
    let name = path.file_name().unwrap_or("");
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("md" | "markdown") => Format::Markdown,
        Some("json") => Format::Json,
        Some("jsonc") => Format::Jsonc,
        Some("toml") => Format::Toml,
        Some("yaml" | "yml") => Format::Yaml,
        _ => Format::Text,
    }
}

/// `./docs/a.md` under cwd, `~/.claude/CLAUDE.md` under home, else the absolute path.
fn display_of(env: &ScanEnv, path: &AbsPath) -> String {
    for (base, prefix) in [(&env.cwd, "."), (&env.home, "~")] {
        if path.starts_with(base) {
            let rest = &path.as_str()[base.as_str().trim_end_matches('/').len()..];
            return format!("{prefix}{rest}");
        }
    }
    path.as_str().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentScan;
    use crate::testing::Fixture;

    #[test]
    fn a_scan_of_a_fixture_yields_three_agents_whose_homes_are_inside_it() {
        let fx = Fixture::new();
        fx.write(&fx.home_path(".claude/CLAUDE.md"), "hi");
        let snapshot = scan(&fx.env());

        let ids: Vec<_> = snapshot
            .agents
            .iter()
            .map(|view| view.agent.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["claude", "codex", "gemini"]);
        for view in &snapshot.agents {
            assert_eq!(view.homes.len(), 1);
            assert!(
                view.homes[0].path.starts_with(&fx.home()),
                "{} is outside the fixture",
                view.homes[0].path
            );
        }
        assert!(snapshot.agents[0].homes[0].exists);
        assert!(!snapshot.agents[1].homes[0].exists);
        assert_eq!(snapshot.agents[0].presence, Presence::ConfigOnly);
        assert_eq!(snapshot.agents[1].presence, Presence::Absent);
        assert_eq!(snapshot.cwd, fx.project());
    }

    #[test]
    fn writing_into_a_probed_directory_changes_the_fingerprint() {
        let fx = Fixture::new();
        fx.mkdir(&fx.home_path(".codex"));
        let first = scan(&fx.env());
        assert_eq!(
            scan(&fx.env()).fingerprint,
            first.fingerprint,
            "a scan of an idle tree is stable"
        );

        fx.write(&fx.home_path(".codex/AGENTS.md"), "rules");
        let second = scan(&fx.env());
        assert_ne!(second.fingerprint, first.fingerprint);
    }

    #[test]
    fn a_present_file_carries_its_o200k_token_count() {
        let fx = Fixture::new();
        let path = fx.project_path("CLAUDE.md");
        fx.write(&path, "hello world");
        let env = fx.env();
        let fs = ProbeFs::new();
        let entry = read_entry(
            &ScanCx {
                env: &env,
                fs: &fs,
                parked_dir: None,
            },
            &path,
            EditPolicy::Editable,
        );
        let FileState::Present { tokens, .. } = entry.state else {
            panic!("expected a present file");
        };
        assert_eq!(tokens, 2);
    }

    #[test]
    fn environment_variables_move_a_home_and_are_reported() {
        let fx = Fixture::new();
        let moved = fx.root().join("elsewhere");
        let env = fx.env().with_var("CODEX_HOME", moved.as_str());
        let snapshot = scan(&env);
        let codex = &snapshot.agents[1].homes[0];
        assert_eq!(codex.path, moved);
        assert_eq!(codex.from_env.as_deref(), Some("CODEX_HOME"));
    }

    #[test]
    fn a_found_binary_makes_an_agent_installed() {
        let fx = Fixture::new();
        let bin_dir = fx.root().join("bin");
        fx.write(&bin_dir.join("gemini"), "#!/bin/sh\n");
        let env = fx
            .env()
            .with_var("PATH", &format!("/nonexistent:{bin_dir}"));
        let snapshot = scan(&env);
        assert_eq!(
            snapshot.agents[2].presence,
            Presence::Installed {
                bin: bin_dir.join("gemini")
            }
        );
    }

    struct Claims(AbsPath);
    impl Agent for Claims {
        fn id(&self) -> AgentId {
            AgentId::parse("claude").unwrap()
        }
        fn display_name(&self) -> &'static str {
            "test"
        }
        fn rules(&self) -> &'static [RuleSpec] {
            &[]
        }
        fn hook_schema(&self) -> &'static HookSchema {
            &crate::agents::NO_HOOKS
        }
        fn scan(&self, cx: &ScanCx) -> AgentScan {
            let mut scan = crate::agents::home_only_scan(
                cx,
                self.id(),
                "claude",
                "claude.home",
                crate::agents::home_with_env(cx, "CLAUDE_CONFIG_DIR", ".claude", false),
            );
            scan.files.push(crate::agents::FileClaim {
                path: self.0.clone(),
                policy: EditPolicy::Editable,
            });
            scan.files.push(crate::agents::FileClaim {
                path: self.0.join("../missing.md"),
                policy: EditPolicy::Editable,
            });
            scan
        }
    }

    #[test]
    fn interning_reads_each_claimed_file_once_and_records_absent_ones() {
        let fx = Fixture::new();
        let file = fx.project_path("a.md");
        fx.write(&file, "one\ntwo");
        fx.symlink(&fx.project_path("link.md"), &file);
        let scanned = scan_with(&[&Claims(file.clone())], &fx.env());

        let entry = &scanned.snapshot.files[&file];
        assert_eq!(entry.display, "./a.md");
        assert_eq!(entry.format, Format::Markdown);
        match &entry.state {
            FileState::Present {
                rev, bytes, lines, ..
            } => {
                assert_eq!(*rev, Revision::of(b"one\ntwo"));
                assert_eq!((*bytes, *lines), (7, 2));
            }
            other => panic!("expected Present, got {other:?}"),
        }
        assert_eq!(
            scanned.snapshot.files[&fx.project_path("missing.md")].state,
            FileState::Absent
        );
        assert_eq!(
            scanned.file_content(&file).unwrap().text.as_deref(),
            Some("one\ntwo")
        );
        assert!(
            scanned
                .file_content(&fx.project_path("elsewhere.md"))
                .is_none()
        );
    }
}
