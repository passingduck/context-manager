//! Instruction files at session start and on demand (spec 1.1 to 1.8): the walk of 1.2, the
//! imports of 1.5, `AGENTS.md` of 1.6, the skips of 1.8 and the on-demand rules of 1.3 and 1.4.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::setup::{SettingsKind, Setup};
use crate::agents::{Candidate, FileClaim, ScanCx};
use crate::glob::{ExcludeGlobs, RuleGlobs, split_paths_string};
use crate::md::frontmatter;
use crate::md::imports::{ImportCap, ImportNode, ImportOutcome, ImportRules, expand_with_seen};
use crate::md::strip::strip;
use crate::model::*;
use crate::probe_fs::{EntryKind, ProbeErrorKind};
use crate::tokens;
use crate::walk;

/// Spec 1.8: a file over 4 MiB is skipped whole.
pub(super) const MAX_FILE_BYTES: u64 = 4_194_304;
/// Spec 1.3: `max(40000, round(window * 0.05 * charsPerToken))`. The window is assumed.
pub(super) const ASSUMED_WINDOW_TOKENS: u32 = 200_000;
const ON_DEMAND_FLOOR_CHARS: u64 = 40_000;
const ON_DEMAND_FRACTION: f64 = 0.05;
/// Spec 1.3 observed 3 chars per token for the newest models. Both factors give the floor at a
/// 200K window.
const CHARS_PER_TOKEN: f64 = 4.0;

/// The plugin that reads `AGENTS.md` (spec 1.6).
const AGENTS_PLUGIN: &str = "cc-plugin-agents-md@builtin";
const AGENTS_PLUGIN_LEGACY: &str = "agents-md@builtin";

/// Spec 1.8: what Claude Code sends of an instruction file. Frontmatter and block comments are
/// removed and the ends are trimmed. The flag says whether anything was removed. The scan counts
/// this text and `render` sends it, so the two agree.
pub(super) fn delivered_text(raw: &str) -> (String, bool) {
    let stripped = strip(raw);
    (stripped.text.trim().to_owned(), stripped.transformed)
}

pub(super) fn on_demand_cap(window_tokens: u32) -> u64 {
    let scaled = (f64::from(window_tokens) * ON_DEMAND_FRACTION * CHARS_PER_TOKEN).round() as u64;
    scaled.max(ON_DEMAND_FLOOR_CHARS)
}

/// Spec 1.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentsMode {
    ClaudeMdOrAgentsMd,
    ClaudeMdAndAgentsMd,
    ClaudeMd,
    ManagedOnly,
}

impl AgentsMode {
    pub fn name(self) -> &'static str {
        match self {
            AgentsMode::ClaudeMdOrAgentsMd => "claude-md-or-agents-md",
            AgentsMode::ClaudeMdAndAgentsMd => "claude-md-and-agents-md",
            AgentsMode::ClaudeMd => "claude-md",
            AgentsMode::ManagedOnly => "managed-only",
        }
    }
}

/// Why nothing loads at all (spec 1.2, first lines). The name is the `FeatureOff` name.
pub(super) fn instructions_off(setup: &Setup, cx: &ScanCx) -> Option<&'static str> {
    [
        "CLAUDE_CODE_DISABLE_CLAUDE_MDS",
        "CLAUDE_CODE_SAFE_MODE",
        "CLAUDE_CODE_SIMPLE",
    ]
    .into_iter()
    .find(|name| setup.flag(cx, name) == Some(true))
}

/// Spec 1.6: the mode key is read from user, `--settings` and managed settings only.
pub(super) fn agents_mode(setup: &Setup) -> (AgentsMode, Option<AbsPath>) {
    let kinds = [
        SettingsKind::Remote,
        SettingsKind::Managed,
        SettingsKind::User,
    ];
    for file in setup.settings.iter().filter(|f| kinds.contains(&f.kind)) {
        let Some(configs) = file.get("pluginConfigs") else {
            continue;
        };
        for id in [AGENTS_PLUGIN, AGENTS_PLUGIN_LEGACY] {
            let value = configs
                .get(id)
                .and_then(|c| c.get("options"))
                .and_then(|o| o.get("instructionFiles"))
                .and_then(Value::as_str);
            let mode = match value {
                Some("claude-md") => Some(AgentsMode::ClaudeMd),
                Some("claude-md-or-agents-md") => Some(AgentsMode::ClaudeMdOrAgentsMd),
                Some("claude-md-and-agents-md") => Some(AgentsMode::ClaudeMdAndAgentsMd),
                Some("managed-only") => Some(AgentsMode::ManagedOnly),
                _ => None,
            };
            if let Some(mode) = mode {
                return (mode, Some(file.path.clone()));
            }
        }
    }
    (AgentsMode::ClaudeMdOrAgentsMd, None)
}

pub(super) struct InstructionsOut {
    pub loads: Vec<Load>,
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<FileClaim>,
    pub mode: AgentsMode,
    pub mode_set_in: Option<AbsPath>,
    pub off: Option<&'static str>,
}

struct Pending {
    file: AbsPath,
    kind: DocKind,
    scope: Scope,
    rule: RuleRef,
    access: AccessRule,
}

/// Everything session start loads from instruction files, then every on-demand file the walk
/// of the project tree found.
pub(super) fn collect(
    cx: &ScanCx,
    setup: &Setup,
    walk_dirs: &[AbsPath],
    window_tokens: u32,
    after_start: &mut dyn FnMut(&mut Collector),
) -> InstructionsOut {
    let (mode, mode_set_in) = agents_mode(setup);
    let off = instructions_off(setup, cx);
    let mut c = Collector::new(cx, setup, window_tokens, off);
    let mut pending: Vec<Pending> = Vec::new();

    // Spec 1.2.
    let managed = &setup.managed;
    c.claim_readonly(&managed.join("CLAUDE.md"));
    c.add_start(
        &managed.join("CLAUDE.md"),
        DocKind::Instructions,
        Scope::Managed,
        "claude.discovery.managed_claude_md",
    );
    c.add_managed_settings();
    c.rules_dir(
        &managed.join(".claude/rules"),
        Scope::Managed,
        "claude.discovery.managed_rules",
        &setup.cwd,
        &mut pending,
        "claude.access.managed_path_rule",
    );
    let only_managed = mode == AgentsMode::ManagedOnly;
    c.managed_only = only_managed;
    c.add_start(
        &setup.config.join("CLAUDE.md"),
        DocKind::Instructions,
        Scope::User,
        "claude.discovery.user_claude_md",
    );
    c.rules_dir(
        &setup.config.join("rules"),
        Scope::User,
        "claude.discovery.user_rules",
        &setup.cwd,
        &mut pending,
        "claude.access.user_path_rule",
    );

    let ancestors = ancestors_of(cx, setup);
    let claude_md_present = ancestors.iter().find_map(|dir| first_claude_md(cx, dir));
    for dir in &ancestors {
        let scope = Scope::Project { dir: dir.clone() };
        let skipped = nested_worktree_skip(setup, dir);
        c.worktree_skipped = skipped;
        c.add_start(
            &dir.join("CLAUDE.md"),
            DocKind::Instructions,
            scope.clone(),
            "claude.discovery.project_claude_md",
        );
        c.add_start(
            &dir.join(".claude/CLAUDE.md"),
            DocKind::Instructions,
            scope.clone(),
            "claude.discovery.project_dotclaude_md",
        );
        c.rules_dir(
            &dir.join(".claude/rules"),
            scope.clone(),
            "claude.discovery.project_rules",
            dir,
            &mut pending,
            "claude.access.path_rule",
        );
        c.worktree_skipped = false;
        c.agents_md(dir, &scope, mode, claude_md_present.as_ref());
        // `CLAUDE.local.md` of a skipped directory still loads (spec 1.1, nested worktree).
        c.add_start(
            &dir.join("CLAUDE.local.md"),
            DocKind::Instructions,
            Scope::Local,
            "claude.discovery.local_claude_md",
        );
    }
    c.managed_only = false;
    // Spec 1.2: auto memory comes last of everything that loads at start.
    after_start(&mut c);

    // Spec 1.3: what the walk of the project tree adds, and the path rules of the ancestors.
    let mut nested = Vec::new();
    for dir in walk_dirs {
        if dir == &setup.cwd || !dir.starts_with(&setup.cwd) || hidden_below(&setup.cwd, dir) {
            continue;
        }
        own_candidates(cx, dir, &mut nested);
    }
    global_candidates(cx, setup, &mut nested);
    for cand in nested {
        pending.push(Pending {
            file: cand.file,
            kind: cand.kind,
            scope: cand.scope,
            rule: cand.rule,
            access: cand.access,
        });
    }
    let (loads, diagnostics, files) = c.finish_with(pending);
    InstructionsOut {
        loads,
        diagnostics,
        files,
        mode,
        mode_set_in,
        off,
    }
}

/// Ancestors of cwd from the root-most down to cwd, the filesystem root excluded and the
/// `CLAUDE_CODE_AUTOMOUNT_ROOTS` entries skipped (spec 1.1).
pub(super) fn ancestors_of(cx: &ScanCx, setup: &Setup) -> Vec<AbsPath> {
    let automount: Vec<AbsPath> = setup
        .var(cx, "CLAUDE_CODE_AUTOMOUNT_ROOTS")
        .map(|raw| {
            raw.split(':')
                .filter_map(|p| AbsPath::parse(p).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut dir = Some(setup.cwd.clone());
    while let Some(current) = dir {
        if current.as_str() == "/" {
            break;
        }
        if !automount.contains(&current) {
            out.push(current.clone());
        }
        dir = current.parent();
    }
    out.reverse();
    out
}

fn first_claude_md(cx: &ScanCx, dir: &AbsPath) -> Option<AbsPath> {
    ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| cx.fs.stat(path).is_ok_and(|s| s.kind == EntryKind::File))
}

/// Spec 1.1, nested worktree: `zbn(e, o) = inside(e, mainRepoRoot) && !inside(e, worktreeRoot)`.
fn nested_worktree_skip(setup: &Setup, dir: &AbsPath) -> bool {
    match (&setup.project.main_root, &setup.project.git_root) {
        (Some(main), Some(tree)) if main != tree => dir.starts_with(main) && !dir.starts_with(tree),
        _ => false,
    }
}

fn hidden_below(base: &AbsPath, dir: &AbsPath) -> bool {
    dir.as_str()[base.as_str().len()..]
        .split('/')
        .any(|part| part.starts_with('.'))
}

// ---------------------------------------------------------------------------------------------
// The collector
// ---------------------------------------------------------------------------------------------

pub(super) struct Collector<'a> {
    cx: &'a ScanCx<'a>,
    setup: &'a Setup,
    loads: Vec<Load>,
    diagnostics: Vec<Diagnostic>,
    files: Vec<FileClaim>,
    seen: HashSet<AbsPath>,
    at: HashMap<AbsPath, LoadId>,
    excludes: Vec<(String, ExcludeGlobs)>,
    approved: bool,
    cap: u64,
    off: Option<&'static str>,
    /// Spec 1.6 `managed-only`: user, project and local files are dropped.
    managed_only: bool,
    /// Spec 1.1: the file sits in a directory that a nested worktree replaces.
    worktree_skipped: bool,
}

/// What `evaluate` decided about a file.
enum Verdict {
    Absent,
    Skip(Skip),
    Present { text: String, stripped: bool },
}

impl<'a> Collector<'a> {
    fn new(
        cx: &'a ScanCx<'a>,
        setup: &'a Setup,
        window_tokens: u32,
        off: Option<&'static str>,
    ) -> Self {
        let excludes = setup
            .union_strings("claudeMdExcludes")
            .into_iter()
            .map(|pattern| {
                let globs = ExcludeGlobs::new(std::slice::from_ref(&pattern));
                (pattern, globs)
            })
            .collect();
        let approved = setup
            .project_record
            .as_ref()
            .and_then(|(_, rec)| rec.get("hasClaudeMdExternalIncludesApproved"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Collector {
            cx,
            setup,
            loads: Vec::new(),
            diagnostics: Vec::new(),
            files: Vec::new(),
            seen: HashSet::new(),
            at: HashMap::new(),
            excludes,
            approved,
            cap: on_demand_cap(window_tokens),
            off,
            managed_only: false,
            worktree_skipped: false,
        }
    }

    fn finish_with(
        mut self,
        pending: Vec<Pending>,
    ) -> (Vec<Load>, Vec<Diagnostic>, Vec<FileClaim>) {
        // Spec 1.3: on-demand files come after everything that loads at start.
        let mut done: HashSet<(AbsPath, String)> = HashSet::new();
        for item in pending {
            let key = (item.file.clone(), format!("{:?}", item.access));
            if !done.insert(key) {
                continue;
            }
            self.add_file(
                &item.file,
                item.kind,
                item.scope,
                item.rule,
                Some(item.access),
            );
        }
        (self.loads, self.diagnostics, self.files)
    }

    fn claim_readonly(&mut self, path: &AbsPath) {
        self.files.push(FileClaim {
            path: path.clone(),
            policy: EditPolicy::ReadOnly {
                why: ReadOnlyWhy::Managed,
            },
        });
    }

    fn rule_ref(rule: &'static str) -> RuleRef {
        RuleRef::new(rule)
    }

    fn next_id(&self) -> LoadId {
        LoadId(self.loads.len() as u32)
    }

    /// Decides whether a file at `path` reaches the model, in the order of spec 1.2.
    fn evaluate(&mut self, path: &AbsPath, scope: &Scope, check_seen: bool) -> Verdict {
        let stat = match self.cx.fs.stat(path) {
            Ok(stat) => stat,
            Err(err) if err.is_not_found() => return Verdict::Absent,
            Err(err) => {
                return Verdict::Skip(Skip::Invalid {
                    why: Invalid::Unreadable {
                        os: err.os_detail(),
                    },
                });
            }
        };
        if let Some(name) = self.off {
            return Verdict::Skip(Skip::FeatureOff {
                name: name.to_owned(),
            });
        }
        if self.worktree_skipped && !matches!(scope, Scope::Local) {
            return Verdict::Skip(Skip::NotRead {
                why: RuleRef::new("claude.instructions.nested_worktree"),
            });
        }
        if self.managed_only && !matches!(scope, Scope::Managed) {
            return Verdict::Skip(Skip::FeatureOff {
                name: "instructionFiles:managed-only".into(),
            });
        }
        let real = if stat.is_symlink {
            self.cx.fs.real_path(path).ok()
        } else {
            None
        };
        if check_seen {
            let key = real.as_ref().unwrap_or(path);
            if let Some(first) = self.at.get(key).or_else(|| self.at.get(path)) {
                return Verdict::Skip(Skip::AlreadyLoaded {
                    first: first.clone(),
                });
            }
        }
        // Spec 1.7: excludes apply to user, project and local files, a symlink by either path.
        if !matches!(scope, Scope::Managed) {
            let mut paths = vec![path];
            if let Some(real) = &real {
                paths.push(real);
            }
            if let Some((pattern, _)) = self.excludes.iter().find(|(_, g)| g.excludes(&paths)) {
                return Verdict::Skip(Skip::Excluded {
                    pattern: pattern.clone(),
                });
            }
        }
        if let Some(which) = refused_location(path, real.as_ref()) {
            return Verdict::Skip(Skip::Invalid {
                why: Invalid::Location { which },
            });
        }
        if stat.len > MAX_FILE_BYTES {
            return Verdict::Skip(Skip::TooLarge {
                limit: Limit {
                    kind: LimitKind::MaxFileBytes,
                    value: MAX_FILE_BYTES,
                    unit: Unit::Bytes,
                },
            });
        }
        let bytes = match self.cx.fs.read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                let why = match err.kind {
                    ProbeErrorKind::NotRegular => Invalid::NotRegular,
                    _ => Invalid::Unreadable {
                        os: err.os_detail(),
                    },
                };
                return Verdict::Skip(Skip::Invalid { why });
            }
        };
        let (text, stripped) = delivered_text(&String::from_utf8_lossy(&bytes));
        if text.is_empty() {
            return Verdict::Skip(Skip::Empty);
        }
        Verdict::Present { text, stripped }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_load(
        &mut self,
        file: &AbsPath,
        kind: DocKind,
        scope: Scope,
        via: Via,
        when: Activation,
        status: LoadStatus,
        delivered: Option<Delivered>,
    ) -> LoadId {
        let id = self.next_id();
        self.loads.push(Load {
            id: id.clone(),
            file: file.clone(),
            kind,
            scope,
            via,
            when,
            status,
            delivered,
        });
        id
    }

    /// A start-time file (or, with `access`, an on-demand one). Returns the load when the
    /// file loaded, after adding its imports.
    fn add_file(
        &mut self,
        path: &AbsPath,
        kind: DocKind,
        scope: Scope,
        rule: RuleRef,
        access: Option<AccessRule>,
    ) -> Option<LoadId> {
        let when = match access {
            Some(rule) => Activation::OnAccess { rule },
            None => Activation::SessionStart,
        };
        let via = Via::Discovery { rule };
        match self.evaluate(path, &scope, true) {
            Verdict::Absent => None,
            Verdict::Skip(skip) => {
                self.push_load(
                    path,
                    kind,
                    scope,
                    via,
                    when,
                    LoadStatus::Skipped { skip },
                    None,
                );
                None
            }
            Verdict::Present { text, stripped, .. } => {
                let on_demand =
                    matches!(when, Activation::OnAccess { .. }) && !matches!(scope, Scope::Managed);
                let (status, shown) = self.cut_on_demand(&text, on_demand);
                let delivered = Delivered {
                    bytes: shown.len() as u64,
                    tokens: tokens::count(&shown),
                    transformed: stripped,
                };
                let real = self.cx.fs.real_path(path).unwrap_or_else(|_| path.clone());
                self.seen.insert(real);
                let id = self.push_load(
                    path,
                    kind,
                    scope.clone(),
                    via,
                    when.clone(),
                    status,
                    Some(delivered),
                );
                self.at.insert(path.clone(), id.clone());
                self.at.insert(
                    self.cx.fs.real_path(path).unwrap_or_else(|_| path.clone()),
                    id.clone(),
                );
                self.imports_of(path, id.clone(), &scope);
                Some(id)
            }
        }
    }

    fn add_start(&mut self, path: &AbsPath, kind: DocKind, scope: Scope, rule: &'static str) {
        self.add_file(path, kind, scope, RuleRef::new(rule), None);
    }

    /// Spec 1.3: a file that loads on demand is cut at the on-demand limit, in characters.
    fn cut_on_demand(&self, text: &str, on_demand: bool) -> (LoadStatus, String) {
        let total = text.chars().count() as u64;
        if !on_demand || total <= self.cap {
            return (LoadStatus::Loaded, text.to_owned());
        }
        let cut = crate::limits::cut_chars(text, self.cap);
        (
            LoadStatus::Truncated {
                limit: Limit {
                    kind: LimitKind::OnDemandChars,
                    value: self.cap,
                    unit: Unit::Chars,
                },
                kept: cut.kept,
                total: cut.total,
            },
            cut.text,
        )
    }

    /// Spec 1.2: server-managed `claudeMd` first, then the policy value.
    fn add_managed_settings(&mut self) {
        let files: Vec<_> = self
            .setup
            .settings
            .iter()
            .filter(|f| matches!(f.kind, SettingsKind::Remote | SettingsKind::Managed))
            .filter_map(|f| {
                let text = f.get("claudeMd")?.as_str()?.to_owned();
                Some((f.path.clone(), text))
            })
            .collect();
        for (path, text) in files {
            let text = text.trim().to_owned();
            let status = if self.off.is_some() {
                LoadStatus::Skipped {
                    skip: Skip::FeatureOff {
                        name: self.off.unwrap_or_default().to_owned(),
                    },
                }
            } else if text.is_empty() {
                LoadStatus::Skipped { skip: Skip::Empty }
            } else {
                LoadStatus::Loaded
            };
            let delivered = matches!(status, LoadStatus::Loaded).then(|| Delivered {
                bytes: text.len() as u64,
                tokens: tokens::count(&text),
                transformed: false,
            });
            self.claim_readonly(&path);
            self.push_load(
                &path,
                DocKind::Instructions,
                Scope::Managed,
                Via::Discovery {
                    rule: Self::rule_ref("claude.discovery.managed_setting"),
                },
                Activation::SessionStart,
                status,
                delivered,
            );
        }
    }

    // ----- imports (spec 1.5) -----

    fn import_rules(&self, scope: &Scope) -> ImportRules {
        let root = if matches!(scope, Scope::User) || self.approved {
            None
        } else {
            Some(self.setup.cwd.clone())
        };
        ImportRules::claude_observed(self.setup.home.clone(), root)
    }

    fn imports_of(&mut self, file: &AbsPath, host: LoadId, scope: &Scope) {
        let rules = self.import_rules(scope);
        let tree = expand_with_seen(file, &rules, self.cx.fs, &mut self.seen);
        if let Some(problem) = tree.capped.and_then(ImportCap::problem) {
            let at = Subject::File {
                path: file.clone(),
                line: None,
            };
            self.problem(Severity::Warning, at, problem);
        }
        self.emit_imports(&tree.nodes, host, scope);
    }

    fn emit_imports(&mut self, nodes: &[ImportNode], host: LoadId, scope: &Scope) {
        let kind = DocKind::Instructions;
        let when = Activation::WithHost;
        for node in nodes {
            let Some(path) = node.path.clone() else {
                continue;
            };
            let via = Via::Import {
                host: host.clone(),
                line: node.line,
                depth: node.depth.min(255) as u8,
            };
            let file = self.loads[host.0 as usize].file.clone();
            let at = Subject::File {
                path: file,
                line: Some(node.line),
            };
            let skip = match node.outcome {
                ImportOutcome::Imported => match self.evaluate(&path, scope, false) {
                    Verdict::Absent => Some(Skip::Missing),
                    Verdict::Skip(skip) => Some(skip),
                    Verdict::Present { text, stripped, .. } => {
                        let on_demand = matches!(
                            self.loads[host.0 as usize].when,
                            Activation::OnAccess { .. }
                        ) && !matches!(scope, Scope::Managed);
                        let (status, shown) = self.cut_on_demand(&text, on_demand);
                        let delivered = Delivered {
                            bytes: shown.len() as u64,
                            tokens: tokens::count(&shown),
                            transformed: stripped,
                        };
                        let id = self.push_load(
                            &path,
                            kind,
                            scope.clone(),
                            via,
                            when.clone(),
                            status,
                            Some(delivered),
                        );
                        self.at.insert(path.clone(), id.clone());
                        self.emit_imports(&node.children, id, scope);
                        continue;
                    }
                },
                ImportOutcome::Missing => {
                    self.problem(
                        Severity::Warning,
                        at,
                        Problem::ImportMissing {
                            raw: node.token.clone(),
                        },
                    );
                    Some(Skip::Missing)
                }
                ImportOutcome::NeedsApproval | ImportOutcome::OutsideRoot => {
                    self.problem(
                        Severity::Info,
                        at,
                        Problem::ImportNeedsApproval {
                            raw: node.token.clone(),
                        },
                    );
                    Some(Skip::NeedsApproval)
                }
                ImportOutcome::TooDeep => {
                    self.problem(Severity::Warning, at, Problem::ImportTooDeep { max: 4 });
                    Some(Skip::TooLarge {
                        limit: Limit {
                            kind: LimitKind::ImportDepth,
                            value: 4,
                            unit: Unit::Hops,
                        },
                    })
                }
                ImportOutcome::AlreadyLoaded | ImportOutcome::Cycle => {
                    self.at.get(&path).map(|first| Skip::AlreadyLoaded {
                        first: first.clone(),
                    })
                }
                ImportOutcome::NotText => Some(Skip::Invalid {
                    why: Invalid::NotText,
                }),
                ImportOutcome::Unreadable => Some(Skip::Invalid {
                    why: Invalid::Unreadable { os: None },
                }),
            };
            if let Some(skip) = skip {
                self.push_load(
                    &path,
                    kind,
                    scope.clone(),
                    via,
                    when.clone(),
                    LoadStatus::Skipped { skip },
                    None,
                );
            }
        }
    }

    fn problem(&mut self, severity: Severity, at: Subject, problem: Problem) {
        self.diagnostics.push(Diagnostic {
            severity,
            agent: Some(AgentId::builtin("claude")),
            at,
            problem,
            fix: None,
        });
    }

    // ----- rules (spec 1.4) -----

    #[allow(clippy::too_many_arguments)]
    fn rules_dir(
        &mut self,
        dir: &AbsPath,
        scope: Scope,
        rule: &'static str,
        glob_base: &AbsPath,
        pending: &mut Vec<Pending>,
        access_rule: &'static str,
    ) {
        for file in rule_files(self.cx, dir) {
            // Spec 1.4, symlinks: a link that leaves cwd waits for the external-include
            // approval, and a path-scoped rule behind it never loads.
            let external = matches!(scope, Scope::Project { .. })
                && self
                    .cx
                    .fs
                    .real_path(&file)
                    .is_ok_and(|real| !real.starts_with(&self.setup.cwd))
                && self.cx.fs.stat(&file).is_ok_and(|s| s.is_symlink);
            let parsed = self.parse_rule(&file);
            match parsed {
                RuleHead::Broken => {
                    self.push_load(
                        &file,
                        DocKind::Rule,
                        scope.clone(),
                        Via::Discovery {
                            rule: Self::rule_ref(rule),
                        },
                        Activation::SessionStart,
                        LoadStatus::Skipped {
                            skip: Skip::Invalid {
                                why: Invalid::Frontmatter,
                            },
                        },
                        None,
                    );
                    self.problem(
                        Severity::Warning,
                        Subject::File {
                            path: file.clone(),
                            line: None,
                        },
                        Problem::FrontmatterInvalid {
                            field: "paths".into(),
                        },
                    );
                }
                RuleHead::Conditional(patterns) => {
                    if external && !self.approved {
                        self.push_load(
                            &file,
                            DocKind::Rule,
                            scope.clone(),
                            Via::Discovery {
                                rule: Self::rule_ref(rule),
                            },
                            Activation::OnAccess {
                                rule: AccessRule::Glob {
                                    base: glob_base.clone(),
                                    patterns,
                                },
                            },
                            LoadStatus::Skipped {
                                skip: Skip::NeedsApproval,
                            },
                            None,
                        );
                    } else {
                        pending.push(Pending {
                            file: file.clone(),
                            kind: DocKind::Rule,
                            scope: scope.clone(),
                            rule: RuleRef::new(access_rule),
                            access: AccessRule::Glob {
                                base: glob_base.clone(),
                                patterns,
                            },
                        });
                    }
                }
                RuleHead::Unconditional => {
                    if external && !self.approved {
                        self.push_load(
                            &file,
                            DocKind::Rule,
                            scope.clone(),
                            Via::Discovery {
                                rule: Self::rule_ref(rule),
                            },
                            Activation::SessionStart,
                            LoadStatus::Skipped {
                                skip: Skip::NeedsApproval,
                            },
                            None,
                        );
                    } else {
                        self.add_file(
                            &file,
                            DocKind::Rule,
                            scope.clone(),
                            RuleRef::new(rule),
                            None,
                        );
                    }
                }
            }
        }
    }

    fn parse_rule(&self, file: &AbsPath) -> RuleHead {
        let Ok(bytes) = self.cx.fs.read(file) else {
            return RuleHead::Unconditional;
        };
        rule_head(&String::from_utf8_lossy(&bytes))
    }

    // ----- AGENTS.md (spec 1.6) -----

    fn agents_md(
        &mut self,
        dir: &AbsPath,
        scope: &Scope,
        mode: AgentsMode,
        claude_md_present: Option<&AbsPath>,
    ) {
        let plugin_on = self
            .setup
            .enabled_plugins()
            .get(AGENTS_PLUGIN)
            .copied()
            .unwrap_or(true);
        for rel in ["AGENTS.md", ".claude/AGENTS.md"] {
            let path = dir.join(rel);
            if !self
                .cx
                .fs
                .stat(&path)
                .is_ok_and(|s| s.kind == EntryKind::File)
            {
                continue;
            }
            let via = Via::Discovery {
                rule: Self::rule_ref("claude.discovery.agents_md"),
            };
            // Already loaded through an import of a CLAUDE.md: loaded once, nothing was ignored.
            let real = self.cx.fs.real_path(&path).unwrap_or_else(|_| path.clone());
            if let Some(first) = self.at.get(&real).or_else(|| self.at.get(&path)).cloned() {
                self.push_load(
                    &path,
                    DocKind::Instructions,
                    scope.clone(),
                    via,
                    Activation::SessionStart,
                    LoadStatus::Skipped {
                        skip: Skip::AlreadyLoaded { first },
                    },
                    None,
                );
                continue;
            }
            let skip = match mode {
                _ if !plugin_on => Some(Skip::FeatureOff {
                    name: AGENTS_PLUGIN.into(),
                }),
                AgentsMode::ClaudeMd | AgentsMode::ManagedOnly => Some(Skip::FeatureOff {
                    name: format!("instructionFiles:{}", mode.name()),
                }),
                AgentsMode::ClaudeMdOrAgentsMd => claude_md_present.map(|by| Skip::Shadowed {
                    by: by.clone(),
                    how: Shadow::ClaudeMdPresent,
                }),
                AgentsMode::ClaudeMdAndAgentsMd => None,
            };
            match skip {
                Some(skip) => {
                    if let Skip::Shadowed { by, .. } = &skip {
                        self.problem(
                            Severity::Info,
                            Subject::File {
                                path: path.clone(),
                                line: None,
                            },
                            Problem::AgentsMdIgnored {
                                because: by.clone(),
                            },
                        );
                    }
                    self.push_load(
                        &path,
                        DocKind::Instructions,
                        scope.clone(),
                        via,
                        Activation::SessionStart,
                        LoadStatus::Skipped { skip },
                        None,
                    );
                }
                None => {
                    self.add_file(
                        &path,
                        DocKind::Instructions,
                        scope.clone(),
                        RuleRef::new("claude.discovery.agents_md"),
                        None,
                    );
                }
            }
        }
    }
}

/// What a rule's frontmatter says (spec 1.4).
enum RuleHead {
    Unconditional,
    Conditional(Vec<String>),
    /// The YAML does not parse. Observed: such a rule never loads (UNVERIFIED in general).
    Broken,
}

fn rule_head(text: &str) -> RuleHead {
    let fm = frontmatter::parse(text);
    if fm.error.is_some() {
        return RuleHead::Broken;
    }
    let Some(map) = fm.map else {
        return RuleHead::Unconditional;
    };
    let patterns: Vec<String> = match map.get("paths") {
        Some(Value::String(one)) => split_paths_string(one),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .flat_map(split_paths_string)
            .collect(),
        _ => Vec::new(),
    };
    if RuleGlobs::new(&patterns).is_unconditional() {
        RuleHead::Unconditional
    } else {
        RuleHead::Conditional(patterns)
    }
}

/// Every `.md` file under a rules directory, recursive, in path order. Symlinks are followed
/// once each (spec 1.4: cycles are handled).
pub(super) fn rule_files(cx: &ScanCx, dir: &AbsPath) -> Vec<AbsPath> {
    let mut out: Vec<AbsPath> = walk::list_tree(cx.fs, dir, |_| true)
        .into_iter()
        .filter(|e| e.kind == EntryKind::File && e.name().ends_with(".md"))
        .map(|e| e.path)
        .collect();
    out.sort();
    out
}

/// Spec 1.8: a scratchpad-shaped path, `connector-downloads`, a device or process file.
pub(super) fn refused_location(path: &AbsPath, real: Option<&AbsPath>) -> Option<RefusedLocation> {
    for candidate in std::iter::once(path).chain(real) {
        let parts: Vec<&str> = candidate.as_str().split('/').collect();
        if parts.contains(&"connector-downloads") {
            return Some(RefusedLocation::ConnectorDownloads);
        }
        // `claude-<uid>/<project>/<session-id>/scratchpad/...`
        for (i, part) in parts.iter().enumerate() {
            let is_root = part
                .strip_prefix("claude-")
                .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()));
            if is_root && parts.get(i + 3) == Some(&"scratchpad") {
                return Some(RefusedLocation::Scratchpad);
            }
        }
        if candidate.starts_with(&AbsPath::parse("/dev").expect("constant"))
            || candidate.starts_with(&AbsPath::parse("/proc").expect("constant"))
        {
            return Some(RefusedLocation::DeviceOrProcess);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// On-demand candidates (spec 1.3)
// ---------------------------------------------------------------------------------------------

/// The files of `dir` itself that load when the agent touches a file under it. `dir` is below
/// cwd (step 2 of spec 1.3).
pub(super) fn own_candidates(cx: &ScanCx, dir: &AbsPath, out: &mut Vec<Candidate>) {
    let scope = Scope::Project { dir: dir.clone() };
    let under = || AccessRule::UnderDir { dir: dir.clone() };
    for (rel, kind, scope, rule) in [
        (
            "CLAUDE.md",
            DocKind::Instructions,
            scope.clone(),
            "claude.access.nested_claude_md",
        ),
        (
            ".claude/CLAUDE.md",
            DocKind::Instructions,
            scope.clone(),
            "claude.access.nested_claude_md",
        ),
        (
            "CLAUDE.local.md",
            DocKind::Instructions,
            Scope::Local,
            "claude.access.nested_local_md",
        ),
    ] {
        let file = dir.join(rel);
        if cx.fs.stat(&file).is_ok_and(|s| s.kind == EntryKind::File) {
            out.push(Candidate {
                file,
                kind,
                scope,
                rule: RuleRef::new(rule),
                access: under(),
            });
        }
    }
    for file in rule_files(cx, &dir.join(".claude/rules")) {
        let access = match rule_text_head(cx, &file) {
            RuleHead::Conditional(patterns) => AccessRule::Glob {
                base: dir.clone(),
                patterns,
            },
            RuleHead::Unconditional => under(),
            // Never loads, at start or on access (spec 1.4); the start walk reports it.
            RuleHead::Broken => continue,
        };
        let rule = if matches!(access, AccessRule::Glob { .. }) {
            "claude.access.path_rule"
        } else {
            "claude.access.nested_rule"
        };
        out.push(Candidate {
            file,
            kind: DocKind::Rule,
            scope: scope.clone(),
            rule: RuleRef::new(rule),
            access,
        });
    }
}

fn rule_text_head(cx: &ScanCx, file: &AbsPath) -> RuleHead {
    match cx.fs.read(file) {
        Ok(bytes) => rule_head(&String::from_utf8_lossy(&bytes)),
        Err(_) => RuleHead::Unconditional,
    }
}

/// Steps 1 and 3 of spec 1.3: path rules of managed, user, cwd and every ancestor. They do
/// not depend on the touched directory.
pub(super) fn global_candidates(cx: &ScanCx, setup: &Setup, out: &mut Vec<Candidate>) {
    let mut push_dir = |dir: AbsPath, scope: Scope, base: AbsPath, rule: &'static str| {
        for file in rule_files(cx, &dir) {
            if let RuleHead::Conditional(patterns) = rule_text_head(cx, &file) {
                out.push(Candidate {
                    file,
                    kind: DocKind::Rule,
                    scope: scope.clone(),
                    rule: RuleRef::new(rule),
                    access: AccessRule::Glob {
                        base: base.clone(),
                        patterns,
                    },
                });
            }
        }
    };
    push_dir(
        setup.managed.join(".claude/rules"),
        Scope::Managed,
        setup.cwd.clone(),
        "claude.access.managed_path_rule",
    );
    push_dir(
        setup.config.join("rules"),
        Scope::User,
        setup.cwd.clone(),
        "claude.access.user_path_rule",
    );
    for dir in ancestors_of(cx, setup).into_iter().rev() {
        push_dir(
            dir.join(".claude/rules"),
            Scope::Project { dir: dir.clone() },
            dir.clone(),
            "claude.access.path_rule",
        );
    }
}
