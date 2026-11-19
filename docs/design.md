# context-manager design

This document is the contract the build follows. It records the shape of the system, the core types, and the reasons behind them. When the code must deviate, the deviation is recorded in [Deviations](#deviations) with who accepted it.

## 1. Goal and scope

`context-manager` is one binary. For a project directory it shows what Claude Code, Codex CLI and Gemini CLI put in front of the model, and it lets the operator change that.

In scope for the first version:

- **Agents.** Claude Code, Codex CLI, Gemini CLI. A fourth agent must be cheap to add.
- **What it shows.** Markdown each agent loads at session start. Markdown the model may see later: files the CLI loads on a condition, plus the reference graph (markdown that an included document names by link or path, followed recursively). Hooks. Each agent's local memory. Skills, custom commands, subagents.
- **What it edits.** Add, edit and delete for all of the above. Hooks also get an on/off switch and a test run.
- **Analysis.** A token budget, a doctor, a path simulator, and search.
- **Safety.** OS trash for deletes, a diff before every save, a conflict instead of an overwrite when the file changed on disk, and the tool's own change history with revert.
- **Shells.** A Tauri window by default. `--serve` serves the same UI over HTTP, also on a remote Linux host without GUI libraries. `--path <dir>` selects the project; the default is the current directory. The UI can switch to projects the agents have records of.
- **Languages.** Korean and English.

Out of scope: MCP servers, general settings and permissions, duplicate detection across agents, terminal-output subcommands.

The rules each adapter implements are specified in `docs/agents/claude-code.md`, `docs/agents/codex.md` and `docs/agents/gemini-cli.md`. Each rule there carries its evidence and a status. This document refers to them as "the specs".

## 2. Shape

### One operation table, two pipes, no server state

`cm-core` declares every operation once in a table. `dispatch(Request)` is the only entry point. The desktop shell registers one Tauri command, `rpc`. The serve shell registers one route, `POST /api/rpc`. Both forward a request to `dispatch` and know nothing about individual operations. The frontend has one `call(op, args)` function and one small transport file.

The host keeps no state of record. A scan is a function of the filesystem and the process environment. The host stores no plan: `apply` plans the edit again and checks that the result is what the user previewed. Change detection is an operation (a long poll), so no event channel exists. The only things the tool owns on disk are its change history and the hooks it parked (section 4).

### Shapes rejected

| shape | why rejected |
|---|---|
| A Tauri command and an HTTP route per operation | Every operation has two implementations, and they drift. |
| Always HTTP, with the Tauri window loading `127.0.0.1` | The default mode would open a socket that can write hooks and run them, guarded by a token the user never asked for. |
| Agent rules in TypeScript over plain file operations | The remote host's files are only readable from Rust, so the rules would exist twice. |
| A host with a snapshot cache, a file watcher, an event stream and a plan table | Two detection mechanisms (watcher and polling fallback), two event transports (Tauri events and a streamed HTTP response), and a plan table with expiry. Each is state that can disagree with the disk. The cost of the stateless host is up to one second of delay before the UI notices an outside change. |

### Measured before deciding

A throwaway prototype settled the technical questions on this machine.

- A page in the Tauri 2.12.7 webview reached Rust through one command, including a call that awaits 2 s (2007 ms round trip). A long poll therefore works over `invoke`.
- The desktop process had no listening TCP socket.
- The same crate built with `--no-default-features` links only `libSystem` and `libiconv`.
- A classic script that declares a top-level `const isTauri` fails silently inside Tauri, because Tauri defines `window.isTauri` as a fixed global. The UI ships ES modules only and detects Tauri with `'__TAURI_INTERNALS__' in window`.
- `jsonc-parser` 0.34 (`cst` feature) removed, changed and appended hook entries in a commented settings file. Comments survived.
- `toml_edit` 0.25 keeps comments. Index assignment into missing tables writes an inline table, so the code must create standard tables.
- `tiktoken-rs` 0.12 `o200k_base` counts 1 MB in 46 ms after a 97 ms load.

## 3. Read model

Every type derives `Serialize`, `Deserialize` and `ts_rs::TS`. Field names are camelCase on the wire. Newtypes are transparent strings. A `u64` field is exported as `number` and stays below 2^53.

An enum whose variants carry data is tagged with `kind`. The listings below write a variant that wraps one other type as a tuple variant, for brevity. In the code it is a struct variant with one named field, because two tagged enums cannot nest as tuples. `Skipped(Skip)` is `Skipped { skip: Skip }`, and its JSON is `{"kind":"skipped","skip":{"kind":"missing"}}`. The Rust source is the authority for those field names once the contract unit lands.

A snapshot holds each file once, however many agents use it. Each agent then lists its `Load`s. A `Load` answers three separate questions: how the file was found (`Via`), when it reaches the model (`Activation`), and what happened to it (`LoadStatus`). Keeping the three apart is what lets the model say "loads on access, but excluded by a setting" or "imported and cut at a limit" without a new variant per combination.

```rust
pub struct AbsPath(String);       // absolute, lexically normalized, UTF-8. Built only by AbsPath::parse at a boundary.
pub struct Revision(String);      // "sha256:<hex>" of the file bytes. The compare-and-swap token for writes.
pub struct Fingerprint(String);   // hash over the stat of every path a scan probed
pub struct AgentId(String);       // "claude" | "codex" | "gemini". Checked against the registry at parse. Nothing matches on it.
pub struct RuleRef(String);       // "codex.agents_md.project". Resolves to a RuleSpec.
pub struct LoadId(u32);           // index into AgentView.loads, valid within one snapshot
pub struct HookId(String);        // hash(agent, source file, locator, digest of the stored entry)
pub struct ExtId(String);         // hash(agent, kind, file)
pub struct SlotId(String);
pub struct ChangeId(String);      // ULID, so history sorts and pages by id

pub struct Snapshot {
    pub cwd: AbsPath,                          // the directory a session would start in
    pub git_root: Option<AbsPath>,
    pub fingerprint: Fingerprint,
    pub files: BTreeMap<AbsPath, FileEntry>,   // every file any agent looked at, present or absent, exactly once
    pub agents: Vec<AgentView>,                // registry order
    pub diagnostics: Vec<Diagnostic>,
    pub walk: WalkSummary,                     // { dirs, files, capped } for the project tree walk
}

pub struct FileEntry {
    pub display: String,                       // "~/.claude/CLAUDE.md", "./docs/a.md"
    pub real_path: Option<AbsPath>,            // set when the path is a symlink. Writes go to the target.
    pub format: Format,                        // Markdown | Json | Jsonc | Toml | Yaml | Text | Binary
    pub state: FileState,
    pub policy: EditPolicy,                    // the most restrictive claim across agents
    pub agents: Vec<AgentId>,                  // which agents use this file
}
pub enum FileState {
    Present { rev: Revision, bytes: u64, lines: u32, tokens: u32, mtime_ms: i64 },   // tokens: whole file
    Absent,                                    // a probed location, or the target of a broken import
    Unreadable { error: String },
    TooBig { bytes: u64 },                     // over 8 MiB: listed, never read
}
pub enum EditPolicy { Editable, ReadOnly(ReadOnlyWhy) }
pub enum ReadOnlyWhy { Managed, PluginCache { plugin: String }, Generated, AgentState, Synced, NotUtf8 }

pub struct AgentView {
    pub agent: AgentId,
    pub presence: Presence,                    // Installed { bin } | ConfigOnly | Absent
    pub homes: Vec<HomeDir>,                   // { role: RuleRef, path, from_env: Option<String>, exists }
    pub trust: Trust,                          // { level: Trusted | Unknown | Untrusted, recorded_in: Option<AbsPath> }
    pub assumptions: Vec<Assumption>,          // what a static scan had to assume
    pub loads: Vec<Load>,
    pub session: Vec<ContextBlock>,            // the session-start context, in delivery order
    pub budget: Budget,
    pub hooks: Vec<Hook>,
    pub hook_files: Vec<HookFile>,             // files a new hook may be added to
    pub memory: Vec<MemoryStore>,
    pub extensions: Vec<Extension>,            // skills, commands, subagents
    pub slots: Vec<Slot>,                      // places where a new item would load
}
pub enum Assumption {
    ContextWindow { tokens: u32 },             // sets Claude's on-demand cut and the Codex skill catalog budget
    Feature { name: String, on: bool, set_in: Option<AbsPath> },
    Mode { name: String, value: String, set_in: Option<AbsPath> },   // e.g. Claude instructionFiles, Gemini context.fileName
    Unknown { what: RuleRef },                 // e.g. server-managed policy, MCP server instructions
}

/// One reason one file reaches, or would reach, the model.
pub struct Load {
    pub id: LoadId,
    pub file: AbsPath,
    pub kind: DocKind,
    pub scope: Scope,
    pub via: Via,
    pub when: Activation,
    pub status: LoadStatus,
    pub delivered: Option<Delivered>,          // set exactly when status is Loaded or Truncated
}
pub struct Delivered { pub bytes: u64, pub tokens: u32, pub transformed: bool }   // transformed: frontmatter or comments stripped
pub enum DocKind { Instructions, Rule, MemoryIndex, MemoryTopic, MemorySummary,
                   Listing, SkillBody, CommandBody, SubagentPrompt, Reference }
pub enum Scope { Managed, User, Project { dir: AbsPath }, Local, Plugin { id: String }, Builtin }
pub enum Via {
    Discovery { rule: RuleRef },
    Import { host: LoadId, line: u32, depth: u8 },
    Reference { host: LoadId, line: u32, depth: u8 },   // a link or a path the host names
    Listing { ext: ExtId },                             // a generated catalog line, not file text
}
pub enum Activation {
    SessionStart,
    OnAccess(AccessRule),              // the CLI adds it when the agent touches a matching file
    OnInvoke,                          // a skill or command body, a subagent prompt
    WithHost,                          // an import: active exactly when its host is
    ModelMayRead,                      // reference targets, memory topic files, Codex AGENTS.md below cwd
}
pub enum AccessRule { UnderDir { dir: AbsPath }, Glob { base: AbsPath, patterns: Vec<String> } }
pub enum LoadStatus {
    Loaded,
    Truncated { limit: Limit, kept: u64, total: u64 },
    Skipped(Skip),
}
pub enum Skip {
    Shadowed { by: AbsPath, how: Shadow },   // OnePerDirectory | EmptyOverride | FirstNonEmpty | ClaudeMdPresent | NameCollision
    BudgetExhausted { limit: Limit },
    TooLarge { limit: Limit },
    Empty,
    Missing,
    Untrusted,
    NeedsApproval,                           // Claude: an import outside cwd that the user has not approved
    Excluded { pattern: String },            // Claude claudeMdExcludes
    FeatureOff { name: String },
    AlreadyLoaded { first: LoadId },
    Invalid { detail: String },
    NotRead { why: RuleRef },                // e.g. the legacy Codex prompts directory
}
pub struct Limit { pub kind: LimitKind, pub value: u64, pub unit: Unit }   // Unit: Bytes | Chars | Lines | Tokens | Hops
pub enum LimitKind { ProjectDocBudget, IndexLines, IndexBytes, OnDemandChars, MaxFileBytes,
                     SummaryTokens, ListingEntryChars, ListingBudgetChars, ImportDepth }

/// What the agent sends when a session starts. Structure only. The literal text comes from the `render` operation.
pub struct ContextBlock {
    pub delivery: Delivery,                  // SystemPrompt | UserMessage | DeveloperMessage | ToolSpec
    pub label: BlockLabel,                   // Instructions | Memory | SkillCatalog | AgentCatalog | CommandCatalog
    pub parts: Vec<Part>,
    pub tokens: u32,
    pub fidelity: Fidelity,                  // Exact (compared with the real CLI in tests) | Approximate
}
pub enum Part { Load { load: LoadId, tokens: u32 }, Glue { tokens: u32 } }
pub struct Budget {
    pub session_start: u32,                  // sum of ContextBlock.tokens
    pub by_label: Vec<(BlockLabel, u32)>,
    pub on_access_max: u32,                  // if every OnAccess load fired
    pub method: TokenMethod,                 // O200kEstimate
}

pub struct Hook {
    pub id: HookId,
    pub source: HookSource,
    pub event: String,                       // the agent's own event name
    pub matcher: Matcher,
    pub handler: Handler,
    pub state: HookState,
    pub toggle: Toggle,
}
pub struct HookSource { pub file: AbsPath, pub at: Locator, pub scope: Scope, pub registered: Registration }
pub enum Locator { Json { pointer: String }, Toml { path: Vec<TomlSeg> }, Frontmatter { path: Vec<String> }, Parked { key: String } }
pub enum Registration { SessionStart, OnSkillInvoke { ext: ExtId }, WhileSubagent { ext: ExtId } }
pub struct Matcher { pub raw: Option<String>, pub eval: MatcherEval }   // All | Exact { values } | Regex | Ignored | Invalid { error }
pub struct Handler {
    pub handler_kind: String,                // "command", "http", "mcp_tool", "prompt", "agent", or the unknown type as written
    pub raw: serde_json::Value,              // the entry exactly as stored. Unknown keys survive an edit.
    pub summary: String,                     // the command line, the URL, or the head of the prompt
    pub timeout_ms: Option<u64>,             // normalized from seconds or milliseconds
}
pub enum HookState { Active, Disabled(DisabledBy), WillNotRun(HookBlock) }
pub enum DisabledBy { Native { set_in: AbsPath }, Parked, AllHooksOff { set_in: AbsPath } }
pub enum HookBlock { NeedsTrust, Modified, FolderUntrusted, FeatureOff, PluginDisabled, ManagedOnly, UnsupportedType }
pub enum Toggle { Native, Park, Unavailable { why: RuleRef } }
pub struct HookFile { pub file: AbsPath, pub scope: Scope, pub format: HookFormat, pub exists: bool, pub policy: EditPolicy }
pub enum HookFormat { ClaudeSettingsJson, CodexHooksJson, CodexConfigToml, GeminiSettingsJsonc }

pub struct MemoryStore {
    pub id: String,                          // "claude.auto", "claude.agent:<name>", "codex.memories", "gemini.private"
    pub label: RuleRef,
    pub root: AbsPath,
    pub enabled: Enabled,                    // On | Off { by: String }
    pub index: Option<AbsPath>,
    pub entries: Vec<MemoryEntry>,
    pub write: MemoryWrite,                  // Direct | NotesDir { dir } | ReadOnly { why: RuleRef }
}
pub struct MemoryEntry { pub file: AbsPath, pub role: MemoryRole, pub indexed: Option<bool>, pub title: String, pub description: Option<String> }
pub enum MemoryRole { Index, Topic, Summary, Note, Generated }

pub struct Extension {
    pub id: ExtId,
    pub ext_kind: ExtKind,                   // Skill | Command | Subagent
    pub name: String,                        // the name it is invoked by
    pub description: Option<String>,
    pub file: AbsPath,
    pub scope: Scope,
    pub listing: Option<LoadId>,             // the always-loaded catalog line, if any
    pub body: LoadId,                        // the text that loads on invoke
    pub status: ExtStatus,                   // Active | Conditional { patterns } | Hidden { why } | Shadowed { by } | Rejected { why } | Disabled { by }
}
pub struct Slot { pub id: SlotId, pub place: Place, pub new_kind: NewItemKind, pub scope: Scope, pub rule: RuleRef }
pub enum Place { File { path: AbsPath }, NamedFile { dir: AbsPath, ext: String }, NamedDir { dir: AbsPath, file: String } }
pub enum NewItemKind { Instructions, Rule, Skill, Command, Subagent, Memory }

pub struct Diagnostic { pub severity: Severity, pub agent: Option<AgentId>, pub at: Subject, pub problem: Problem, pub fix: Option<Edit> }
pub enum Subject { File { path: AbsPath, line: Option<u32> }, Load { agent: AgentId, load: LoadId }, Hook { hook: HookId }, Extension { ext: ExtId }, Agent }
pub enum Problem {
    ImportMissing { raw: String }, ImportOutsideRoot { raw: String }, ImportTooDeep { max: u8 }, ImportCycle { path: AbsPath },
    ImportNeedsApproval { raw: String }, AccidentalImport { raw: String }, ReferenceMissing { raw: String },
    Truncated { limit: Limit, kept: u64, total: u64 }, DroppedByBudget { limit: Limit }, TooLarge { limit: Limit },
    ShadowedByEmptyOverride, AgentsMdIgnored { because: AbsPath },
    MemoryNotInIndex { index: AbsPath }, IndexLinkMissing { target: String }, IndexOverLimit { limit: Limit },
    HookProgramMissing { program: String }, HookNotExecutable { path: AbsPath }, HookUnknownEvent { event: String },
    HookMatcherInvalid { error: String }, HookMatcherIgnored, HookWillNotRun { why: HookBlock },
    ExtensionRejected { why: RuleRef }, CatalogOverBudget { used: u64, limit: Limit },
    ParseError { format: Format, detail: String, fatal_for_agent: bool },
    FrontmatterInvalid { field: String }, LegacyLocation { why: RuleRef }, ProjectUntrusted { skipped: Vec<RuleRef> },
    GlobMatchesNothing { glob: String }, WalkCapped { entries: u32 },
}

/// Agent knowledge that the UI explains. It lives next to the adapter, in both languages.
pub struct RuleSpec { pub id: RuleRef, pub en: String, pub ko: String, pub evidence: String }
```

Consequences of this model:

- **The token budget is a sum.** Each adapter assembles `session`, and the budget adds up its blocks. A skill costs its catalog line at start and its body on invoke, because it has two loads.
- **The literal context is available.** The `render` operation returns the text of each block. Tests compare it with what the real CLI prints.
- **The path simulator reuses the rules.** `OnAccess` rules are data. `simulate` evaluates them for a target path and also probes the directories between `cwd` and the target with the same rule function the scan uses.
- **A broken import is a row, not only a warning.** Its load has `Skip::Missing` and points at an `Absent` file.
- **The backend sends facts.** `Skip`, `Problem`, `Warning` and `HookBlock` are typed and are translated in the frontend. Rule explanations are agent knowledge, so they ship with the adapter as `RuleSpec`s.

## 4. Write model

Every mutation is `plan`, then `apply`. `plan` reads the disk and writes nothing. `apply` is the only code that writes outside the tool's own data directory.

```rust
pub enum Edit {
    Save { path: AbsPath, base: Base, text: String },            // an editor save; base Absent creates
    Trash { path: AbsPath, base: Revision },                     // a skill trashes its directory
    NewItem { agent: AgentId, slot: SlotId, name: Option<String> },
    AddMemory { agent: AgentId, store: String, title: String, description: String, body: String },
    AddHook { agent: AgentId, file: AbsPath, draft: HookDraft },
    UpdateHook { hook: HookId, draft: HookDraft },
    RemoveHook { hook: HookId },
    SetHookEnabled { hook: HookId, enabled: bool },
    Revert { change: ChangeId },
}
pub struct HookDraft { pub event: String, pub matcher: Option<String>, pub handler: serde_json::Value }
pub enum Base { Absent, Rev(Revision), Tree(String) }            // Tree: hash over a directory's paths and revisions

pub struct ChangeSet { pub summary: ChangeSummary, pub ops: Vec<FileOp> }
pub struct FileOp { pub path: AbsPath, pub expect: Base, pub action: Action }
pub enum Action { Write { text: String }, Trash, TrashDir }

pub struct Preview {
    pub digest: String,                                          // hash of the change set; `apply` must reproduce it
    pub change_set: ChangeSet,
    pub before: BTreeMap<AbsPath, Option<String>>,               // for the diff view
    pub warnings: Vec<Warning>,
}
pub enum Warning {
    HookTrustReset,                  // Codex: an edited hook stops until the user trusts it again in /hooks
    AgentAckReset,                   // Gemini: an edited project subagent must be approved again
    AgentAlsoWrites { agent: AgentId },   // Codex config.toml, Gemini settings.json, Claude settings.local.json
    SessionMayOverwrite { agent: AgentId }, // Gemini rewrites settings.json from its startup snapshot
    SharedFile { agents: Vec<AgentId> },  // the file is read by more than one agent
    ThroughSymlink { target: AbsPath },
    TakesEffect { agent: AgentId, when: RuleRef },   // next session, /reload-plugins, /memory reload
    NewImportToken { raw: String },  // Gemini: a new "@word" becomes an import attempt
    ParkedOnThisMachine,             // a parked Claude hook exists only in this tool's data directory
    NotRevertible,                   // a directory over 16 MiB: the OS trash is the only copy
}
pub enum ApplyResult {
    Applied { change: ChangeId, files: Vec<(AbsPath, Option<Revision>)>, not_trashed: Vec<AbsPath> },
    AlreadyApplied,                         // the edit plans to nothing: the disk already has the end state
    Stale { now: Preview },                 // the disk changed since the preview; here is the plan as it stands now
}
// An edit whose own base no longer matches the disk fails in `plan`, and so in `apply`, with
// ApiError::Conflict { path, expected: Base, found: Base, current: Option<String> }.

pub struct ChangeRecord {                   // history/changes/<ulid>.json, or <ulid>.pending.json
    pub id: ChangeId, pub at: String, pub cwd: Option<AbsPath>,
    pub summary: ChangeSummary, pub ops: Vec<JournalOp>, pub state: ChangeState,   // Sealed | Pending | NeedsAttention
}
pub struct JournalOp { pub path: AbsPath, pub before: Option<String>, pub after: Option<String> }   // blob ids
```

Higher-level edits compile to whole-file operations inside `plan`. Deleting a Claude memory becomes `Trash(topic file)` plus `Write(MEMORY.md without its index line)`. Switching off a Claude hook becomes `Write(settings.json)` plus `Write(parked record)`. The diff, the conflict check, the trash and the history therefore cover every edit without format-specific code. Structured edits keep the user's formatting: `jsonc-parser`'s syntax tree for JSON and JSONC, `toml_edit` for TOML.

`plan(edit)` returns a `Preview`. `apply(edit, digest)` takes the same edit and the digest of the preview the user saw. The client never sends file operations, so no mutation can skip the planner, and the tool's own parked-hook files cannot be written directly.

A plan fails in three ways that `plan` and `apply` share. A base that no longer matches the disk is a `Conflict`. A path that is not an `Editable` file of a fresh snapshot, a slot, or the tool's parked-hook directory is `OutsideBoundary`. A change that an agent's guard refuses is `Forbidden`: the Codex guard refuses any change to `hooks.state.*.trusted_hash` or `trusted_content_hash`, so this tool cannot approve a hook on the user's behalf.

### The apply pipeline

One mutex per process serializes `apply`.

1. **Policy.** Refuse when the host runs with `--read-only`.
2. **Plan again.** Run a fresh scan of `cwd` and plan the edit against it. If the plan is empty, return `AlreadyApplied`. If its digest differs from the one the client sent, the disk changed after the preview: return `Stale` with the new preview and write nothing.
3. **Journal.** Store the before and after content of each file as blobs named by their hash. Then write `changes/<id>.pending.json`. Each write is a temp file, a sync and a rename.
4. **Execute** in order. `Write` hashes the target again, writes a temp file in the same directory, keeps the mode bits, syncs, and renames over the real path (a symlink's target). `Trash` moves the file to the OS trash. `TrashDir` stores every file of the directory as blobs first, up to 16 MiB. If the OS trash is not available, as on a headless Linux host without one, the file is removed, the history blob is the copy, and the result lists the path in `not_trashed`.
5. **Seal.** Write `changes/<id>.json` and remove the pending file.

| crash point | left on disk | what recovery does |
|---|---|---|
| before step 3 | nothing | nothing |
| during step 3 | blobs nothing refers to, or a complete pending record | unreferenced blobs older than a day are collected |
| during step 4 | some operations applied, temp files | see below |
| between 4 and 5 | every operation applied, record still pending | seals it |

**Recovery** runs when the host starts and when the history is read. For each pending record it classifies every operation by the hash on disk: at before, at after, or other. If no operation is "other", it applies the ones still at before and seals the record, because the user asked for that change. If any operation is "other", someone edited the file in between. The record stays pending, the UI shows it, and the user reverts or resolves it. Recovery decides from disk hashes, so running it twice changes nothing, and two processes that recover the same record write the same bytes.

**Running twice.** A second `apply` of the same edit plans against the disk as it now is. A save whose text is already on disk, or a switch that is already in the wanted position, plans to nothing and returns `AlreadyApplied`. An edit that would act again, such as adding the same hook a second time, produces a different digest and returns `Stale`. Nothing is applied twice.

**Revert** is an `Edit`. Its plan restores each file's before blob with `expect` set to the after revision. A file that is already at before is skipped. A file at neither becomes a conflict in the preview. A trashed file comes back from its blob, so emptying the OS trash does not break revert.

The window between the hash in step 4 and the rename is microseconds. POSIX has no compare-and-swap on files, and the agents do not lock either. The before blob is already stored by then, so nothing is lost for good.

### What the tool owns on disk

Everything is under the platform data directory (`~/Library/Application Support/context-manager` on macOS, `$XDG_DATA_HOME/context-manager` on Linux), mode 0700, files 0600. `CONTEXT_MANAGER_DATA_DIR` moves it.

- `history/changes/<ulid>.json` and `.pending.json`, one file per change. Two instances never write the same history file.
- `history/blobs/<sha256>`.
- `parked/<sha256 of source file>.json`: Claude hooks the user switched off.

## 5. Agent adapters

```rust
pub trait Agent: Sync {
    fn id(&self) -> AgentId;
    fn display_name(&self) -> &'static str;
    fn rules(&self) -> &'static [RuleSpec];            // loaded from rules.toml next to the adapter
    fn hook_schema(&self) -> &'static HookSchema;      // events, matcher kind per event, handler kinds and their fields
    fn scan(&self, cx: &ScanCx) -> AgentScan;          // every discovery rule, applied to one environment
    fn on_access(&self, cx: &ScanCx, dir: &AbsPath) -> Vec<Candidate>;   // what loads when the agent touches a file in dir
    fn render(&self, cx: &ScanCx, view: &AgentView) -> Vec<RenderedBlock>;   // the literal session-start text
    fn known_projects(&self, cx: &ScanCx) -> Vec<AbsPath>;
    fn plan(&self, cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>>;   // None: the generic plan applies
    fn guard(&self, path: &AbsPath, before: Option<&str>, after: Option<&str>) -> Result<(), ApiError>;   // run on every planned write
    fn exec_spec(&self, hook: &Hook, input: &serde_json::Value, cx: &ScanCx) -> Result<ExecSpec, RuleRef>;
    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value;
}
pub fn registry() -> &'static [&'static dyn Agent];
```

`ScanCx` hands every adapter the same shared parts:

- **`ProbeFs`.** The only way an adapter reads the disk. It caches bytes for the length of one scan, so two agents that read `AGENTS.md` see one revision. It records every path read, listed or checked, misses included. That record is the input of the fingerprint (section 8).
- **`ScanEnv`.** `cwd`, `home`, the OS, and the environment variables that move things (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME` and the others the specs list). Built once at the boundary. Adapters never read the process environment.
- **The project walk.** One bounded walk of the project tree for all agents (20,000 entries, gitignore-aware, hidden directories skipped except `.claude`, `.codex`, `.gemini`, `.agents`).
- **Markdown engines.** Frontmatter parsing. Token spans for links, backticks and `@`, with code regions marked. The import expander, which takes an `ImportRules` value: token syntax, resolution base, allowed root, maximum depth, cycle policy, which code regions to skip.
- **The tokenizer.** `o200k_base`, with counts cached by revision.

`on_access` is the one implementation of an agent's conditional rules. `scan` builds its `OnAccess` loads by calling it for each directory the walk visits. `simulate` evaluates those loads for a target path, and calls `on_access` again only for directories between `cwd` and the target that the walk skipped, such as ignored ones.

The core runs these passes after the adapters, so no adapter repeats them: file interning and policy merging, the reference graph (links and paths that no adapter claimed as an import, to depth 3, at most 500 nodes), budget totals, generic diagnostics (missing references, parse errors, a hook program missing from `PATH`), and merging parked hooks into the hook list.

**Adding a fourth agent** touches one new directory under `agents/` (the rules, `rules.toml` in both languages, the hook schema, planners), one line in `registry()`, and fixtures. It does not touch the frontend, the API, the pipeline or the fingerprint.

### How the awkward rules map onto the model

| rule | Claude Code | Codex | Gemini CLI | in the model |
|---|---|---|---|---|
| where the walk goes | every ancestor of cwd up to just below `/`, root-most first, past the git root and home | project root (nearest `.git`) down to cwd | cwd up to the nearest `.git`; only cwd when there is none | `Via::Discovery { rule }`, `Scope::Project { dir }`, order in `session` |
| cut at a limit | `MEMORY.md`: whole lines up to 200 lines and 25,000 bytes. On-demand files: max(40,000 chars, 5% of the window). Over 4 MiB: skipped | project `AGENTS.md` chain shares 32 KiB: the crossing file is cut at the byte, later files are dropped. `memory_summary.md`: 2,500 tokens | none | `LoadStatus::Truncated { limit, kept, total }`, `Skip::BudgetExhausted`, `Skip::TooLarge` |
| one file hides another | `AGENTS.md` loads only when no `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md` exists in cwd or above (default mode). A path loads once | one file per directory: override, then `AGENTS.md`, then fallbacks, chosen by existence, so an empty override hides. Global: first non-empty | every configured name in a directory loads | `Skip::Shadowed { by, how }`, `Skip::AlreadyLoaded`, `Problem::ShadowedByEmptyOverride`, `Problem::AgentsMdIgnored` |
| loads on a condition | files below cwd, on Read, Write or Edit under that directory. Rules with `paths`, gitignore-style match against the rule's base | never. `AGENTS.md` below cwd is at most something the model reads itself | files between the touched path and the boundary, on `read_file`, `list_directory`, `write_file`, `replace`, `read_many_files` | `Activation::OnAccess(UnderDir | Glob)`. Codex: `ModelMayRead` |
| imports | `@path` outside code, 4 hops, relative to the file. Outside cwd needs a recorded approval. Each import is its own block after its parent | none. `@x.md` is literal text | `@` after whitespace, first character `.`, `/` or a letter, depth 5. Each top-level context file gets one allowed root (its nearest git root, else its own directory), and nested imports reuse it | `Via::Import` with `WithHost`; failures are `Skip::Missing`, `NeedsApproval`, `Invalid`. Codex `@x.md` becomes `Via::Reference` |
| skill: description versus body | catalog line always, body on invoke | catalog line always, with a budget of 2% of the window or 8,000 chars; body when read | name and description in the system prompt, body on `activate_skill` | two loads: `Listing` at `SessionStart`, `SkillBody` at `OnInvoke`; `Problem::CatalogOverBudget` |
| trust | settings-file hooks wait for folder trust in interactive sessions | untrusted: project `AGENTS.md` skipped. Unknown: project config, hooks, agents skipped, `AGENTS.md` loaded. Skills always load | untrusted: project files, workspace settings and every settings-file hook are off | `AgentView.trust`, `Skip::Untrusted`, `HookBlock::FolderUntrusted` |
| hooks | 33 events, five handler types, settings files, plugins, frontmatter. No per-hook switch | 14 events, `hooks.json` and `[hooks]`. A hook runs only when its hash matches `hooks.state`. `enabled` is a native switch | 11 events, command only, timeouts in ms. `hooksConfig.disabled` is a native switch. It matches a hook's `name`, or its `command` when it has no name, so one entry can switch off several hooks | one `Hook` type; `HookFormat` picks the codec; `Handler.raw` keeps unknown keys; `Toggle::Native` or `Park` |
| memory | `<config>/projects/<key>/memory/`: `MEMORY.md` index plus topic files. The key is the main repository root, sanitized, hashed past 200 chars | `memories/`: generated, off by default. Notes go in `extensions/ad_hoc/notes/` | `tmp/<slug>/memory/MEMORY.md` plus sibling notes. The slug comes from `projects.json` | `MemoryStore.write`: `Direct`, `NotesDir`, `ReadOnly` |
| what the tool must not write | managed files, plugin cache, `~/.claude.json` | `hooks.state.*.trusted_hash`, generated memory files, plugin cache, SQLite | `trusted_hooks.json`, `acknowledgments/agents.json`, `trustedFolders.json`, `.inbox` | `EditPolicy::ReadOnly`, `Agent::guard` |

**Switching a hook off.** Codex and Gemini have a native switch, and the tool writes it (`[hooks.state."<key>"].enabled` in the user `config.toml`; `hooksConfig.disabled`). Claude Code has none. There the tool removes the handler from the settings file and stores it in `parked/`, in one change set. A parked hook appears in the list in its old place as `Disabled(Parked)`. Plugin and managed hooks get `Toggle::Unavailable` with the reason, because the plugin cache is overwritten on update.

**Hook test run.** `exec_spec` builds the invocation the agent itself would use, with its working directory and environment. The host runs it in its own process group, writes the sample input to stdin, kills the group at the timeout, and caps stdout and stderr at 1 MiB each.

## 6. API

```rust
api! {
    session(())                                              -> Session;        // version, shell, read_only, launch_cwd, home, os, pending changes
    agents(())                                               -> Vec<AgentInfo>; // id, display name, RuleSpecs, HookSchema
    projects(())                                             -> Vec<KnownProject>;   // path, exists, which agents know it
    scan(ScanReq { cwd })                                    -> Snapshot;
    read_file(ReadReq { cwd, path })                         -> FileContent;    // state plus text; path must be in the snapshot
    render(RenderReq { cwd, agent })                         -> Vec<RenderedBlock>;  // literal session-start text
    simulate(SimReq { cwd, path })                           -> Vec<Simulation>;     // per agent: fired loads, extra loads, added tokens, notes
    search(SearchReq { cwd, query, regex, case_sensitive })  -> SearchResult;   // at most 500 hits
    watch(WatchReq { cwd, since: Fingerprint })              -> Fingerprint;    // long poll, at most 25 s
    plan(PlanReq { cwd, edit: Edit })                        -> Preview;
    apply(ApplyReq { cwd, edit: Edit, digest: String })      -> ApplyResult;
    history(HistoryReq { cwd: Option<AbsPath>, before: Option<ChangeId>, limit })  -> Vec<ChangeRecord>;
    change(ChangeReq { id })                                 -> ChangeDetail;   // the record plus before and after text per file
    hook_sample(SampleReq { cwd, agent, event })             -> serde_json::Value;
    run_hook(RunReq { cwd, hook: HookId, input })            -> HookRun;        // ExecSpec, exit code, stdout, stderr, ms, timed_out
}
```

The macro expands to `enum Request` (tagged with `op` and `args`), `trait Api` with one async method per row, `dispatch`, and a list of (name, argument type, result type).

`ApiError` is `NotFound | Conflict { path, expected, found, current } | OutsideBoundary { path } | ReadOnly { why } | InvalidEdit { field, why } | Forbidden { why } | Io { detail } | Unauthorized`.

**TypeScript stays in step by generation.** `ts-rs` writes one file per type into `ui/src/gen/`. A test writes `ui/src/gen/api-map.ts` from the operation list. The client is `call<K extends keyof ApiMap>(op: K, args: ApiMap[K]['args']): Promise<ApiMap[K]['result']>`. The generated files are committed, and `scripts/check.sh` fails when regeneration changes them. The i18n catalogs are keyed on the generated unions, so a new `Problem` variant fails the type check until both languages render it.

## 7. Modules

```
Cargo.toml                 workspace
crates/cm-core/            reads only. model.rs, api.rs, env.rs, probe_fs.rs, walk.rs, tokens.rs,
                           md/{frontmatter,spans,imports,refs}.rs, cfg/{jsonc,toml}.rs,
                           scan.rs, doctor.rs, simulate.rs, search.rs, edit.rs (Edit to ChangeSet, revert planning),
                           agents/{mod.rs, claude/, codex/, gemini/}   each with discovery, hooks, memory, extensions, plan, rules.toml
crates/cm-host/            effects. apply.rs, history.rs, recovery.rs, parked.rs, runner.rs, watch.rs, host.rs (implements Api)
crates/app/                the binary `context-manager`. cli.rs, serve.rs (axum, auth, embedded assets), desktop.rs (feature `desktop`)
ui/                        Vite, React, TypeScript, Tailwind, CodeMirror 6
docs/agents/               the three specs
fixtures/                  agent homes and projects for tests
tools/oracle/              scripts that ask the real CLIs what they load
scripts/check.sh           fmt, clippy, tests, binding drift, type check, UI build
```

Dependencies point one way: `app` uses `cm-host`, which uses `cm-core`. Only `app` with the `desktop` feature uses Tauri. `cm-core` contains no code that writes a file. `cm-host::apply` is the only code that writes agent files.

`context-manager [--path <dir>] [--serve [--host <addr>] [--port <n>]] [--read-only]`. The default port is 7457.

## 8. Live refresh

`watch { cwd, since }` is a long poll. The host keeps, per `cwd`, the list of paths the last scan probed. This list is a cache: any scan rebuilds it. Once per second the handler stats every path in the list and hashes the result into a fingerprint. It returns when the fingerprint differs from `since`, or after 25 s.

Because the probe list includes directories that were listed and paths that were missing, creating a new `CLAUDE.md` in a probed place changes the fingerprint. No adapter declares what to watch.

The fingerprint is built from length and mtime, not content. A rewrite that keeps the length and lands in the same mtime tick goes unnoticed until the next change. Writes stay safe, because `apply` compares content hashes.

The frontend runs one `watch` loop per open project and refetches `scan` when the fingerprint changes. It pauses the loop while the page is hidden. An editor with unsaved text shows a "changed on disk" bar instead of reloading, and its save still carries the old base, so `apply` reports a conflict. After its own `apply`, the client knows the new revisions and does not show the bar for them.

## 9. Serve security and headless build

Hooks are commands, so write access to this tool is command execution on the host.

| threat | measure |
|---|---|
| the default mode exposes a port | the desktop shell opens no socket |
| another machine reaches the port | `--serve` binds `127.0.0.1` unless `--host` says otherwise. A non-loopback bind prints a warning and the `ssh -L` command to use instead |
| another local user, or anyone who reaches the port | every `/api/*` request needs `Authorization: Bearer <token>`, compared in constant time. The token is 32 random bytes, new per run. `CONTEXT_MANAGER_TOKEN` pins it |
| the token leaks through logs or history | it travels in the URL fragment (`http://host:port/#token=…`), which is never sent to a server. The page moves it to `sessionStorage` and removes it from the address bar. `Referrer-Policy: no-referrer` |
| a web page in the operator's browser forges a request | no cookies, so no ambient credential. No CORS headers. A request whose `Origin` differs from its `Host` gets 403 |
| DNS rebinding | a rebinding page has no token. On a loopback bind the `Host` header must also be a loopback name with the bound port |
| a stolen token writes arbitrary files | the client sends edits, never file operations, and a plan refuses any path outside a fresh snapshot (section 4). `run_hook` runs only a hook in the current snapshot |
| hostile repository content | file text renders as text. The markdown preview has raw HTML off. CSP `default-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'` |
| the operator only wants to look | `--read-only` rejects `apply` and `run_hook` in one place |
| resource exhaustion | request body limit 8 MiB, files over 8 MiB are not read, the walk stops at 20,000 entries, search returns at most 500 hits |

There is no TLS in the first version. The documented path to a remote host is an SSH tunnel to a loopback bind.

**Headless build.** `default = ["desktop"]`, `desktop = ["dep:tauri", "dep:tauri-build"]`. `cargo build -p context-manager --no-default-features` contains no GUI library. That binary run without `--serve` prints one line and exits with status 2. The UI assets are embedded with `rust-embed` from `ui/dist` for the serve shell and through Tauri's own embedding for the desktop shell.

## 10. Frontend

The layout is the explorer variant from the layout prototype: a rail of sections on the left, a tree in the middle, the detail and editor pane on the right.

```
┌ project ▾ ── search ── simulate path [            ] ──────────────── 한국어 | EN ── theme ┐
├────────────┬──────────────────────────────────┬───────────────────────────────────────────┤
│ Context    │ Claude Code        always 7.8k   │ ./CLAUDE.md   always   1.2k tokens        │
│ Memory     │ ▓▓▓▓▓▓▓▓░░░░░░░░                 │ why it loads: …                           │
│ Hooks      │  instructions and rules          │ ─────────────────────────────────────────│
│ Doctor (5) │   ./CLAUDE.md        always 1.2k │ editor | preview | diff                   │
│ History    │    @AGENTS.md        always  640 │                                           │
│            │   ./src/api/CLAUDE.md on access  │                                           │
│            │  reference graph                 │                                           │
│            │  memory · skills · commands      │                                           │
│            │ Codex CLI          always 4.1k   │                                           │
└────────────┴──────────────────────────────────┴───────────────────────────────────────────┘
```

- **Server state** lives in TanStack Query: `['scan', cwd]`, `['file', path, rev]`, `['history', cwd]`. One `useWatch(cwd)` hook runs the long poll and invalidates `scan`.
- **Navigation state** lives in the URL hash (project, view, selection), so reload and the back button work in a browser.
- **Editor state** is one reducer per open file: `{ path, base, loaded, text }`. Dirty is computed, never stored.
- **No agent-specific code.** Trees, forms and menus render from `AgentInfo`, `Load`, `Hook`, `Extension`, `Slot` and `Diagnostic`.
- **Views.** Context (tree grouped by agent: session start, on access, on invoke, may read; a second mode lists the session-start blocks in the order the model receives them, with the literal text from `render`). Memory. Hooks (list by event, switch, form generated from `HookSchema` with a raw JSON tab, test run). Doctor (rows link to their subject; a row with a `fix` offers it through the normal preview). History (diff, revert, pending changes on top). Search and the path simulator sit in the top bar and act on the tree.
- **Two languages.** `ui/src/i18n/en.ts` defines the key set. `ko.ts` is typed `typeof en`, so a missing key fails the type check. Messages for `Skip`, `Problem`, `Warning` and `HookBlock` are functions of their typed fields. Rule explanations come from `RuleSpec.en` or `.ko`. The locale comes from local storage, then the browser language.
- **Editor.** CodeMirror 6 with Markdown, JSON and TOML modes, a gutter mark at a truncation point, and `@codemirror/merge` for the save diff, the conflict view and the history diff.

## 11. Verification

Every check runs on this machine against the real artifact. Agent homes are only read. Fixture homes live in temp directories.

| unit | check |
|---|---|
| skeleton | `--serve` answers `session` with a token and 401 without. The desktop self-test passes. The headless build links no GUI library |
| contract | binding export, `git diff --exit-code ui/src/gen`, type check |
| engines | table tests from the specs: Gemini `@someone said` is a failed import, `@~/x.md` stays literal, `@` in backticks is ignored; Claude imports stop at 4 hops; the Codex 32 KiB fold including a multi-byte split |
| Codex adapter | **oracle**: for fixture homes and repositories, `render` equals the `AGENTS.md` block of `codex debug prompt-input` byte for byte, and the skill catalog lines match. Hooks match `codex app-server` `hooks/list` |
| Claude adapter | **oracle**: the capture scripts in `tools/oracle/claude` run the real binary against a local sink at no cost. The instruction files in the captured request body, and their order, equal our session-start loads. The comparison uses the request body, because `InstructionsLoaded` does not fire for `AGENTS.md`. Fixtures sit outside scratchpad-shaped paths, which Claude Code refuses to load from |
| Gemini adapter | fixtures only, one per confirmed rule in its spec, because the CLI is not installed. This gap is stated in the UI as `Fidelity::Approximate` |
| write pipeline | temp-directory tests: a conflict after an outside edit, a second apply returns `AlreadyApplied`, an outside edit between preview and apply returns `Stale`, a failure injected after each step followed by recovery, a symlink survives, comments survive, revert twice ends at before. Trash: the original path is gone, the trash call succeeded, and revert restores the file from its blob (macOS does not let a terminal list `~/.Trash`) |
| hook planners | round trips keep comments and unknown keys. A Codex toggle leaves `trustStatus` unchanged in `hooks/list`. Park then unpark restores a Claude settings file byte for byte. A change to `trusted_hash` is refused |
| serve hardening | no token 401, wrong token 401, foreign `Origin` 403, oversized body 413, assets 200 without a token |
| UI | a browser drives `--serve` on fixtures: open, edit, diff, save, conflict, revert, hook toggle, hook test run, simulator, language switch. A screenshot per view |
| desktop | `CM_SELFTEST=<file>` makes the page call every operation over `invoke` and write a report. `lsof` shows no listening socket |
| Linux | `cargo check --target x86_64-unknown-linux-gnu --no-default-features` after `rustup target add`. A real run on a Linux host cannot be done on this machine, which has no container runtime. It stays unproven until the operator runs it on a server |

## 12. Build sequence

Each unit ends in a check and a local commit.

1. **Skeleton.** Workspace, three crates, UI skeleton, `scripts/check.sh`, `session` through both shells.
2. **Contract.** `model.rs`, `api.rs`, generated bindings, `ProbeFs`, `ScanEnv`, the fixture helper, a host that answers `scan` from a fixture file.
3. **Engines.** Walk, tokenizer, frontmatter, spans, import expander, reference pass, JSONC and TOML edit helpers.
4. In parallel, each in its own worktree:
   - **Codex adapter**, first because its oracle settles `ContextBlock` and `Limit`.
   - **Claude adapter.**
   - **Gemini adapter.**
   - **Write pipeline.** `edit.rs`, apply, history, recovery, parked store.
   - **Shells.** Serve auth and assets, CLI, desktop shell with self-test.
   - **UI shell.** Transport, stores, router, i18n, read-only views against a fixture snapshot.
5. **Integration.** Scan passes, doctor, simulate, search, projects, watch, hook planners, hook runner.
6. **UI write flows.** Diff, apply, conflict, hooks, memory, history.
7. **Finish.** README in both languages, install script, a run on a Linux host.

## 13. Risks

| unknown | cheapest experiment |
|---|---|
| Codex hook hash reproduction drifts in a later Codex | the oracle compares `currentHash` on every test run; on mismatch the UI shows trust as unknown |
| Claude wrapper text around instruction files | compare `render` with the request body the capture scripts save |
| Gemini rules were never run | install 0.69.0 into a temp prefix later and compare `/memory list` with the scan |
| o200k is not Claude's or Gemini's tokenizer | cannot be measured offline; every number is labelled an estimate with its method |
| the stat pass on a very large repository | time it on the largest local repository; the walk cap bounds it |
| OS trash on a headless Linux host | the `trash` crate follows the freedesktop spec; untested here. The history blob is the real safety net |
| plain HTTP on a non-loopback bind | ask how the operator reaches dev servers; add `--tls-cert` and `--tls-key` if a tunnel is not an option |
| a desktop app started from Finder has no shell environment | `CODEX_HOME` and similar are then unset; show `homes[].from_env` so the difference is visible |
| frontmatter hooks | edited as text in the first version, because no Rust YAML editor keeps comments |

## Decision record

Three candidates wrote this design independently against one grounding file: one by Opus, one by Sonnet, one by the orchestrating session. A judge from another model family scored them against a six-point rubric that the candidates did not see.

| criterion | Opus candidate | orchestrator's candidate | Sonnet candidate |
|---|---|---|---|
| one owner per piece of state | 5 | 4 | 4 |
| one way per task | 5 | 4 | 5 |
| domain fidelity | 4 | 3 | 4 |
| callers know least | 4 | 3 | 4 |
| verifiability on this machine | 5 | 3 | 4 |
| safety of remote serve and of writes | 5 | 3 | 5 |
| total | 28 | 20 | 26 |

**Base: the Opus candidate.** The judge and the orchestrator picked it independently. Its read model separates how a file was found, when it loads and what happened to it. It assembles the session-start context, which makes the budget a sum and gives the Codex adapter a byte-for-byte oracle. Its history uses one file per change and recovers by classifying disk hashes. Its watch set comes from the scan's own probe record.

**Grafted from the orchestrator's candidate.** The stateless host: no snapshot cache, no plan table, no event stream, no file-watcher dependency. Change detection is one long-poll operation over the probe record. Memory edits that keep the index in step. Directory deletes with a tree hash. The fallback when the OS trash is missing.

**Grafted from the Sonnet candidate.** A `fix` on a diagnostic, which is an ordinary `Edit`. The guard that refuses a change to a Codex trust hash. `--read-only`. The headers and limits in the serve threat table.

**Where the judge and the orchestrator disagreed.** The orchestrator's candidate let the client carry the planned file operations back to `apply`. The judge marked that down: a mutation could skip the planner, and a token holder could write the parked-hook files directly. The Opus and Sonnet candidates kept plans in a server-side table with an expiry instead. This design takes neither. `apply` receives the edit and the digest of the preview, plans again, and proceeds only when the digests match. No mutation skips the planner, and the host still stores no plan.

**Corrected in the base.** The judge found these in the Opus candidate, and this document fixes them. The simulator was a second adapter method beside the scan's rules; here `on_access` is the one implementation and the scan calls it. The Gemini import root and the `hooksConfig.disabled` match key were stated loosely. A content cache keyed by mtime could show stale text; here bytes are cached for one scan only and token counts are cached by revision. Three checks could not run on this machine as written (`~/.Trash` listing, a Linux container, a paid Claude API call).

**Rejected.** A filesystem trait with an in-memory implementation: tests use real temp directories, so symlinks and case-insensitive names behave as they do for the agents. Calling `codex app-server` during every scan: it needs Codex installed and makes a scan start a process. A native directory picker and "reveal in Finder": they need a second Tauri API beside `rpc`. Detecting running agent processes for a warning: it adds a process-inspection dependency. A limit on failed logins, a token file flag, a `Host` allowlist flag and a directory allowlist for serve: the token is 256 random bits and is never sent by the browser on its own, and a token holder can already run commands through a hook.

## Deviations

- Contract unit: `Preview` gains `conflicts: Vec<Conflict>`, because section 4 says a revert reports a file at neither state "as a conflict in the preview" and the listing has no field for it.
- Contract unit: `ChangeSummary` is `{ edit: ChangeEdit, agent: Option<AgentId>, targets: Vec<AbsPath> }`, facts only, as rule 3 of `AGENTS.md` requires. `ChangeEdit` mirrors the `Edit` variants.
- Contract unit: `ApiError::ReadOnly`, `InvalidEdit` and `Forbidden` carry `why: RuleRef` instead of prose. `ExtStatus::Shadowed { by }` holds an `ExtId`.
- Write pipeline unit: `parked.rs` (the record type, its path rule and the scan's read) lives in `cm-core`, not `cm-host`, because the Claude planner in `cm-core` must build the record. It still writes nothing: the planner emits the record as an ordinary `Write` operation, and `apply` writes it.
- Write pipeline unit: `HostConfig` gains `data_dir`, and `PlanCx` gains `snapshot` and `parked_dir`. Neither is in `model.rs` or the `api!` table.
- Write pipeline unit: `Edit::Trash` on the entry file of a `NamedDir` slot (`skills/<name>/SKILL.md`) plans `TrashDir` over the directory with `Base::Tree`. The client sends no tree hash; the plan computes it, so a file added to the directory between `plan` and `apply` yields `Stale`. A `Save` carries its own base, so an outside write between `plan` and `apply` yields `Conflict`, not `Stale`.
- Write pipeline unit: a directory that goes to the trash is journaled as one operation per UTF-8 file plus a last operation with neither blob, which stands for the directory. A directory over 16 MiB, or holding a file that is not UTF-8, warns `NotRevertible` and journals only what it can restore as text.
- Write pipeline unit: `Revert` accepts paths outside the snapshot (a trashed file is no longer in it). It still refuses a path the snapshot marks read only, and the guards still run. Reverting a record that never finished seals that record, so recovery does not roll it forward again.
- Write pipeline unit: recovery marks a pending record with an edited file as `NeedsAttention` in its `.pending.json`. The files it names are in the `Report` that `recover` returns, not in `ChangeRecord`.
- Gemini adapter: with folder trust on and no `trustedFolders.json` rule, `Trust.level` is `Untrusted` and `recorded_in` is `None`. The CLI asks the user and behaves as untrusted until they answer (`isTrusted ?? false`), so `Unknown` would imply the Codex behaviour of loading some project files.
- Gemini adapter: `cm-core` gains a direct `regex` dependency, already in the lockfile through `ignore`, to test hook matchers. It tests the Rust regex syntax, so a JavaScript-only pattern with look-around is treated as valid.
- Gemini adapter: `MOVING_VARS` gains the nine variables the Gemini rules read. The planners of the Gemini unit land with the write pipeline.
- Codex adapter: `Hook` has no field for the trust status or `currentHash`, so `agents::codex::inspect_hooks(cx)` returns them, with the hook key, as `HookInspection`. The write pipeline and the oracle tests read trust there. `agents::codex::current_hash(&Hook)` computes the declaration hash for the guard.
- Codex adapter: a handler Codex skips (`prompt`, `agent`, an empty command, an invalid matcher, an MCP handler on `SessionEnd`) is still a `Hook`, with `HookBlock::UnsupportedType`. `hooks/list` does not list it. A hook of a project layer that is not trusted is a `Hook` with `HookBlock::FolderUntrusted`, also unlisted by Codex.
- Codex adapter: the integration tests in `crates/cm-core/tests/` use `cm_core::testing`, so `cm-core` has itself as a dev-dependency with the `testing` feature. `regex` is a new dependency, because Codex matches hooks with the same crate.
- Claude adapter unit: the session-start text is two `RenderedBlock`s, `Instructions` and `Memory`, because `BlockLabel` separates them. One `<system-reminder>` spans both: the first block opens it and the last closes it, and the real text is the two joined by a blank line.
- Claude adapter unit: the managed `claudeMd` setting is a `Load` whose `file` is the settings file that holds the value and whose `via` rule is `claude.discovery.managed_setting`. Claude Code shows it under the pseudo-path `<managed-settings>`, which is not an `AbsPath`.
- Claude adapter unit: `ScanEnv` cannot tell an empty `CLAUDE_CONFIG_DIR` from an unset one, so the split behaviour of spec 5.1 (config home becomes the working directory, `.claude.json` stays in `~`) is not mirrored. The managed directory has a test-only override, `CONTEXT_MANAGER_MANAGED_DIR`, which is not in `MOVING_VARS`, so a real process never reads it.
- Claude adapter unit: `md::imports` gained `ImportRules::claude_observed` and `Protect::MarkdownAndInlineComments`. The shipped `ImportRules::claude` still treats an inline `<!-- @x.md -->` as text, and the 2.1.328 binary does not import from it (oracle scenario `fromrepo`).
- Claude adapter unit: parked hooks and the write planners are not part of this unit. A settings-file hook has `Toggle::Park` and a plugin or managed hook has `Toggle::Unavailable`.
- Codex planners: `Warning::HookTrustReset` has no payload, so a removal that shifts the keys of later hooks carries the same single warning as any other hook edit. A hook edit also carries `TakesEffect` (rule `codex.takes_effect.hooks`) and, for `config.toml`, `AgentAlsoWrites`. A memory note carries `TakesEffect` with `codex.takes_effect.memory_note` or, with the memories feature off, `codex.takes_effect.memory_off`.
- Codex planners: a `HookId` that no agent lists is `ApiError::NotFound` from the first planner that sees it, and a hook another agent lists is left to that agent. Switching a hook back on removes its `enabled` key (and the table when empty) instead of writing `enabled = true`, so off then on restores a file byte for byte except that a file with no final newline gains one. `cfg::toml` gained `remove_pruned`, `table_set_key`, `table_merge` and `element_remove` for this.
- Gemini planners unit: `Warning` gains `HookKeyShared { others: u32 }`. `hooksConfig.disabled` matches a hook's `name` or `command`, so one switch can turn off several hooks, and no existing warning carries the count. `Agent` gains `write_warnings(cx, path, before, after)`, which `Planner::finish` calls for every planned write so that a plain save gets an agent's warnings, and `Edit::Trash` on a file an agent owns goes to that agent's `plan` first (the generic trash applies when it returns `None`), so a Gemini memory note and its index line leave in one plan. Gemini hook enable removes the key from every editable settings layer that lists it, and refuses with `ReadOnly` when a managed layer lists it.
