# context-manager design

This document is the contract the build follows. It records the shape of the system, the core types, and the reasons behind them. When the code must deviate, the deviation is recorded in [Deviations](#deviations) with who accepted it.

## 1. Goal and scope

`context-manager` is one binary. For a project directory it shows what Claude Code, Codex CLI and Gemini CLI put in front of the model, and it lets the operator change that.

In scope for the first version:

- **Agents.** Claude Code, Codex CLI, Gemini CLI. A fourth agent must be cheap to add.
- **What it shows.** Markdown each agent loads at session start. Markdown the model may see later: files the CLI loads on a condition, plus the reference graph (markdown that an included document names by link or path, followed recursively). Hooks. Each agent's local memory. Skills, custom commands, subagents.
- **What it edits.** Add, edit and delete for all of the above. Hooks also get an on/off switch and a test run.
- **Analysis.** A token budget, a doctor, a path simulator, and search.
- **Past sessions.** For a session an agent recorded, what the model received in each call, piece by piece, with the token counts the API reported (section 14). Claude Code first.
- **Safety.** OS trash for deletes, a diff before every save, a conflict instead of an overwrite when the file changed on disk, and the tool's own change history with revert.
- **Shells.** A Tauri window by default. `--serve` serves the same UI over HTTP, also on a remote Linux host without GUI libraries. `--path <dir>` selects the project; the default is the current directory. The UI can switch to projects the agents have records of.
- **Languages.** Korean and English.

Out of scope: MCP servers, general settings and permissions, duplicate detection across agents, terminal-output subcommands.

The rules each adapter implements are specified in `docs/agents/claude-code.md`, `docs/agents/codex.md` and `docs/agents/gemini-cli.md`. Each rule there carries its evidence and a status. This document refers to them as "the specs". `docs/agents/claude-code-sessions.md` specifies the record Claude Code keeps of a session, in the same form. Section 14 calls it "the session spec".

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
pub struct RuleRef(String);       // "codex.agents_md.project". Resolves to a RuleSpec; `RuleRef::new` takes a `&'static str`, so no id is built at run time.
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
    BudgetExhausted { limit: Limit },          // the scan read budget was spent before this file
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
    Invalid { why: Invalid },                // the file cannot serve; only Unreadable carries (operating-system) text
    NotRead { why: RuleRef },                // e.g. the legacy Codex prompts directory
}
pub struct Limit { pub kind: LimitKind, pub value: u64, pub unit: Unit }   // Unit: Bytes | Chars | Lines | Tokens | Hops
pub enum LimitKind { ProjectDocBudget, IndexLines, IndexBytes, OnDemandChars, MaxFileBytes,
                     SummaryTokens, ListingEntryChars, ListingBudgetChars, ImportDepth, ScanReadBytes }

/// What the agent sends when a session starts. Structure only. The literal text comes from the `render` operation.
/// `parts` count the same pieces `render` returns, in the same order, and `tokens` is their sum.
pub struct ContextBlock {
    pub delivery: Delivery,                  // SystemPrompt | SystemMessage | UserMessage | DeveloperMessage | ToolSpec
    pub label: BlockLabel,                   // Instructions | Memory | SkillCatalog | AgentCatalog | CommandCatalog
    pub parts: Vec<Part>,
    pub tokens: u32,
    pub fidelity: Fidelity,                  // Exact (compared with the real CLI in tests) | Approximate
    pub preceded_by: Vec<Unsized>,           // content of unknown size sent between the previous block and this one, in the order the agent sends it
}
pub enum Unsized { BuiltIn, FirstPrompt }    // BuiltIn: built-in prompt and tool definitions. FirstPrompt: the user's first prompt and the context around it
pub enum Part { Load { load: LoadId, tokens: u32 }, Glue { tokens: u32 } }
/// The literal text of one block: its pieces joined. A load's piece is its text without the frame.
pub struct RenderedBlock { pub delivery: Delivery, pub label: BlockLabel, pub pieces: Vec<Piece>, pub tokens: u32, pub fidelity: Fidelity }
pub enum Piece { Frame { text: String }, Load { load: LoadId, text: String } }   // Frame: a header, a separator, a list mark
/// What the model receives from one load of a file. `text` is None for a skipped load.
pub struct LoadText { pub load: LoadId, pub text: Option<String>, pub whole_file: bool }
pub enum LoadTexts { Current { texts: Vec<LoadText> }, Stale }
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
pub enum Place { File { path: AbsPath }, NamedFile { dir: AbsPath, ext: String }, NamedDir { dir: AbsPath, file: String }, InSubdir { dir: AbsPath, file: String } }
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
    ImportBudgetExhausted { max: u32 }, ImportBytesExhausted { max: u64 }, ScanExpansionExhausted { max: u64 },
    ListingCapped { max: u32 }, ListingTooDeep { max: u32 }, ReadBudgetExhausted { max: u64 }, TextPartlyAnalyzed { max: u64 },
    BodyAlsoImported { import: LoadId, host: AbsPath, line: u32 },   // a listed item whose body an import also loads at session start
}

/// Agent knowledge that the UI explains. It lives next to the adapter, in both languages.
pub struct RuleSpec { pub id: RuleRef, pub en: String, pub ko: String, pub evidence: String }
pub enum Invalid { Unreadable { os: Option<String> }, NotRegular, NotText, Frontmatter, Toml, Location { which: RefusedLocation }, OutsideRoot }
pub enum RefusedLocation { ConnectorDownloads, Scratchpad, DeviceOrProcess }
pub enum ReadOnlyReason { Host, File { why: ReadOnlyWhy }, Rule { rule: RuleRef } }
pub enum HookRunFailure { EmptyInvocation, ProgramNotFound { program: String }, StartFailed { program: String, os: String } }
```

Consequences of this model:

- **The token budget is a sum.** Each adapter assembles `session`, and the budget adds up its blocks. A skill costs its catalog line at start and its body on invoke, because it has two loads.
- **The literal context is available.** The `render` operation returns the text of each block as pieces: frame, or one load's text. Tests compare the joined text with what the real CLI prints.
- **Each place a file loads has its own text.** `load_texts` returns, per load of one file, what the model receives there. A session-start load's text is its piece of the block. A load that fires later asks its adapter (`Agent::load_text`), which returns the same text its scan counted.
- **The path simulator reuses the rules.** `OnAccess` rules are data. `simulate` evaluates them for a target path and also probes the directories between `cwd` and the target with the same rule function the scan uses.
- **A broken import is a row, not only a warning.** Its load has `Skip::Missing` and points at an `Absent` file.
- **The backend sends facts.** `Skip`, `Problem`, `Warning` and `HookBlock` are typed and are translated in the frontend. Rule explanations are agent knowledge, so they ship with the adapter as `RuleSpec`s. Reasons that belong to no agent are in `crates/cm-core/src/core_rules.toml` and travel in `Session.core_rules`. A test fails when code names a rule id that no rules file defines.

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
    ThroughSymlink { target: AbsPath },   // the path, or a directory above it below the slot's root, is a symlink: the write lands at `target`
    HardLinked { links: u32 },       // the file has other names: the write goes in place and changes all of them
    TakesEffect { agent: AgentId, when: RuleRef },   // next session, /reload-plugins, /memory reload
    NewImportToken { raw: String },  // Gemini: a new "@word" becomes an import attempt
    ParkedOnThisMachine,             // a parked Claude hook exists only in this tool's data directory
    NotRevertible,                   // a directory the plan could not capture completely: the OS trash is the only copy
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
    pub summary: ChangeSummary, pub ops: Vec<JournalOp>, pub state: ChangeState,   // Sealed | Pending | NeedsAttention | Aborted
}
pub struct JournalOp { pub path: AbsPath, pub before: Option<String>, pub after: Option<String> }   // blob ids
```

Higher-level edits compile to whole-file operations inside `plan`. Deleting a Claude memory becomes `Trash(topic file)` plus `Write(MEMORY.md without its index line)`. Switching off a Claude hook becomes `Write(settings.json)` plus `Write(parked record)`. The diff, the conflict check, the trash and the history therefore cover every edit without format-specific code. Structured edits keep the user's formatting: `jsonc-parser`'s syntax tree for JSON and JSONC, `toml_edit` for TOML.

`plan(edit)` returns a `Preview`. `apply(edit, digest)` takes the same edit and the digest of the preview the user saw. The client never sends file operations, so no mutation can skip the planner, and the tool's own parked-hook files cannot be written directly.

A plan fails in three ways that `plan` and `apply` share. A base that no longer matches the disk is a `Conflict`. A path that is not an `Editable` file of a fresh snapshot, a slot, or the tool's parked-hook directory is `OutsideBoundary`. A path that is a symlink to nothing is an `InvalidEdit`: the tool does not create a file through a link. A change that an agent's guard refuses is `Forbidden`: the Codex guard refuses any change to `hooks.state.*.trusted_hash` or `trusted_content_hash`, so this tool cannot approve a hook on the user's behalf.

### The apply pipeline

One mutex per process serializes `apply`, and a lock on a file in the data directory serializes it across processes. `apply` holds the lock from step 2 to step 5. Recovery takes the same lock without waiting. When another process holds it, that process is applying a change, so recovery leaves the pending records and the temp files alone in this round.

1. **Policy.** Refuse when the host runs with `--read-only`.
2. **Plan again.** Run a fresh scan of `cwd` and plan the edit against it. If the plan is empty, return `AlreadyApplied`. If its digest differs from the one the client sent, the disk changed after the preview: return `Stale` with the new preview and write nothing.
3. **Journal.** Store the before and after content of each file as blobs named by their hash. Then write `changes/<id>.pending.json`. Each write is a temp file, a sync and a rename.
4. **Execute** in order. `Write` hashes the target again, writes a temp file in the same directory, keeps the mode bits, syncs, and renames over the real path (a symlink's target). A file that has more than one hard link is written in place instead, because a rename would leave the other names on the old content. `Trash` moves the file to the OS trash. `TrashDir` stores every file of the directory as blobs first, up to 16 MiB. A directory the plan could not capture completely (over that limit, holding a file that is not UTF-8, a name the lister cannot represent or a symlink, or deeper or larger than the plan's walk limits) carries the `NotRevertible` warning. If the OS trash is not available, as on a headless Linux host without one, a file or directory that history holds completely is removed, the history blobs are the copy, and the result lists the path in `not_trashed`. A `NotRevertible` directory is never removed that way: the operation fails and nothing is deleted.
5. **Seal.** Write `changes/<id>.json` and remove the pending file.

| crash point | left on disk | what recovery does |
|---|---|---|
| before step 3 | nothing | nothing |
| during step 3 | blobs nothing refers to, or a complete pending record | unreferenced blobs older than a day are collected |
| during step 4 | some operations applied, temp files | the record needs attention, and the user reverts it |
| between 4 and 5 | every operation applied, record still pending | seals it |

**Recovery** runs when the host starts and when the history is read. It never writes an agent file. For each pending record it classifies every operation by the hash on disk: at before, at after, or other. When every operation is at after, it seals the record. When every operation is at before, it marks the record `Aborted`, because nothing of the change is on disk. In any other case the record becomes `NeedsAttention`: the UI shows which file is in which state, and the user reverts the change. Recovery decides from disk hashes, so running it twice changes nothing.

A revert of an unfinished record restores the files that are at after, leaves the files at before alone, and reports a file at neither as a conflict. When it completes it resolves the record, also when it plans to nothing.

**A failed apply is not a crash.** When an operation returns an error in step 4, `apply` undoes the operations it had already applied, from their before blobs and guarded by their after hash, marks the record `Aborted`, and returns the error. If the undo fails too, the record stays `NeedsAttention`. A change the user saw fail is never applied later.

**Running twice.** A second `apply` of the same edit plans against the disk as it now is. A save whose text is already on disk, or a switch that is already in the wanted position, plans to nothing and returns `AlreadyApplied`. An edit that would act again, such as adding the same hook a second time, produces a different digest and returns `Stale`. Nothing is applied twice.

**Revert** is an `Edit`. Its plan restores each file's before blob with `expect` set to the after revision. A file that is already at before is skipped. A file at neither becomes a conflict in the preview. A trashed file comes back from its blob, so emptying the OS trash does not break revert.

The window between the hash in step 4 and the rename is microseconds. POSIX has no compare-and-swap on files, and the agents do not lock either. The before blob holds the text the operator previewed. It does not hold an edit that lands inside that window: the rename overwrites such an edit, and no blob keeps it. The model of the write pipeline (section 11) records this as the finding `edit-in-window`. An atomic exchange of the two names (`renamex_np` with `RENAME_SWAP` on macOS, `renameat2` with `RENAME_EXCHANGE` on Linux) would hand back the text that was replaced and close the window. The first version does not do that.

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
    fn render(&self, cx: &ScanCx, view: &AgentView) -> Vec<RenderedBlock>;   // the literal session-start text, piece by piece
    fn load_text(&self, cx: &ScanCx, view: &AgentView, load: &Load) -> Option<String>;   // a delivered load with no piece of its own; default: the file
    fn known_projects(&self, cx: &ScanCx) -> Vec<AbsPath>;
    fn plan(&self, cx: &PlanCx, edit: &Edit) -> Option<Result<Preview, ApiError>>;   // None: the generic plan applies
    fn write_warnings(&self, cx: &PlanCx, path: &AbsPath, before: Option<&str>, after: Option<&str>) -> Vec<Warning>;   // warnings about a file, for every write or trash of it
    fn guard(&self, path: &AbsPath, before: Option<&str>, after: Option<&str>) -> Result<(), ApiError>;   // run on every planned write
    fn exec_spec(&self, hook: &Hook, input: &serde_json::Value, cx: &ScanCx) -> Result<ExecSpec, RuleRef>;
    fn sample_input(&self, event: &str, cx: &ScanCx) -> serde_json::Value;
    fn traces(&self, cx: &ScanCx) -> Vec<TraceInfo>;                      // recorded sessions that started in cwd or below; default: none
    fn trace(&self, cx: &ScanCx, id: &TraceId, part: Option<&str>) -> Result<TraceData, ApiError>;   // one session or one subagent of it; default: NotFound
}
pub fn registry() -> &'static [&'static dyn Agent];
```

`ScanCx` hands every adapter the same shared parts:

- **`ProbeFs`.** The only way an adapter reads the disk. `read` returns the bytes of a regular file of at most 8 MiB and a typed refusal for anything else, so no caller needs its own check. It also counts the bytes it has read in one scan. Once the scan's read budget is spent, `read` refuses every further file, the file gets a skip reason, and the scan reports one diagnostic and goes on. It caches bytes for the length of one scan, so two agents that read `AGENTS.md` see one revision. It records every path read, listed or checked, misses included. That record is the input of the fingerprint (section 8).
- **`ScanEnv`.** `cwd`, `home`, the OS, and the environment variables that move things (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME` and the others the specs list). Built once at the boundary. Adapters never read the process environment.
- **The project walk.** One bounded walk of the project tree for all agents (20,000 entries, gitignore-aware, hidden directories skipped except `.claude`, `.codex`, `.gemini`, `.agents`).
- **The directory lister.** An adapter lists a directory tree, such as a commands or skills directory, only through one core function. It lists each real directory once, whatever number of links reaches it, and stops at a depth limit and at an entry budget. A listing that hit a limit is a diagnostic. No adapter recurses by itself.
- **Markdown engines.** Frontmatter parsing. Token spans for links, backticks and `@`, with code regions marked. The import expander, which takes an `ImportRules` value: token syntax, resolution base, allowed root, maximum depth, cycle policy, which code regions to skip. It also takes a budget of import nodes per root file, because a per-chain cycle policy lets the same file expand once per path that reaches it, and a budget of expanded bytes per root file, because one large file can be imported many times. Both are enforced while expanding.
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
    session(())                                              -> Session;        // version, shell, read_only, launch_cwd, home, os, pending changes, core_rules
    agents(())                                               -> Vec<AgentInfo>; // id, display name, RuleSpecs, HookSchema
    projects(())                                             -> Vec<KnownProject>;   // path, exists, which agents know it
    scan(ScanReq { cwd })                                    -> Snapshot;
    read_file(ReadReq { cwd, path })                         -> FileContent;    // state plus text; path must be in the snapshot
    render(RenderReq { cwd, agent })                         -> Vec<RenderedBlock>;  // literal session-start text
    load_texts(LoadTextsReq { cwd, agent, file, fingerprint }) -> LoadTexts;      // per load of one file; Stale when a fresh scan differs
    simulate(SimReq { cwd, path })                           -> Vec<Simulation>;     // per agent: fired loads, extra loads, added tokens, notes
    search(SearchReq { cwd, query, regex, case_sensitive })  -> SearchResult;   // at most 500 hits
    watch(WatchReq { cwd, since: Fingerprint })              -> Fingerprint;    // long poll, at most 25 s
    plan(PlanReq { cwd, edit: Edit })                        -> Preview;
    apply(ApplyReq { cwd, edit: Edit, digest: String })      -> ApplyResult;
    history(HistoryReq { cwd: Option<AbsPath>, before: Option<ChangeId>, limit })  -> Vec<ChangeRecord>;
    change(ChangeReq { id })                                 -> ChangeDetail;   // the record plus before and after text per file
    hook_sample(SampleReq { cwd, agent, event })             -> serde_json::Value;
    run_hook(RunReq { cwd, hook: HookId, input })            -> HookRun;        // ExecSpec, exit code, stdout, stderr, ms, timed_out
    traces(TracesReq { cwd })                                -> Vec<TraceInfo>; // every agent's recorded sessions that started in cwd or below, newest first
    trace(TraceReq { cwd, key: TraceKey })                   -> Trace;          // the calls of one session, or of one subagent of it
    trace_step(TraceStepReq { cwd, key, read, step })        -> StepPieces;     // the pieces of one request; Stale when the record changed under the trace
    trace_piece(TracePieceReq { cwd, key, read, step, piece: PieceRef }) -> PieceText;   // the text of one piece, at most 1 MiB
}
```

The macro expands to `enum Request` (tagged with `op` and `args`), `trait Api` with one async method per row, `dispatch`, and a list of (name, argument type, result type).

`ApiError` is `NotFound | Conflict { path, expected, found, current } | OutsideBoundary { path } | ReadOnly { why: ReadOnlyReason } | InvalidEdit { field, why: RuleRef } | Forbidden { why: RuleRef } | HookRun { why: HookRunFailure } | BadRequest | HistoryCorrupt | Io { detail } | Unauthorized`. `Io` carries only text from the operating system. A condition the tool detects itself (a path that is not a regular file, a file over the read limit) is an `InvalidEdit` with a core rule.

**TypeScript stays in step by generation.** `ts-rs` writes one file per type into `ui/src/gen/`. A test writes `ui/src/gen/api-map.ts` from the operation list. The client is `call<K extends keyof ApiMap>(op: K, args: ApiMap[K]['args']): Promise<ApiMap[K]['result']>`. The generated files are committed, and `scripts/check.sh` fails when regeneration changes them. The i18n catalogs are keyed on the generated unions, so a new `Problem` variant fails the type check until both languages render it.

## 7. Modules

```
Cargo.toml                 workspace
crates/cm-core/            reads only. model.rs, api.rs, env.rs, probe_fs.rs, walk.rs, tokens.rs,
                           md/{frontmatter,spans,imports,refs}.rs, cfg/{jsonc,toml}.rs,
                           scan.rs, doctor.rs, simulate.rs, search.rs, edit.rs (Edit to ChangeSet, revert planning),
                           trace.rs (TraceData and its projections; an adapter's reader lives in its own directory),
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
| resource exhaustion by a client | request body limit 8 MiB, search returns at most 500 hits |
| a token holder reads past sessions | the trace operations are reads behind the same token as every other operation. A session record holds what the operator typed and every tool output, so the README says that `--serve` exposes them. The fields of a record that identify an account, an organisation or a host are never parsed into the model |
| resource exhaustion by a session record | the line reader stops at 512 MiB per record and 64 MiB per line, and one piece's text is returned up to 1 MiB |
| resource exhaustion by repository content | `ProbeFs::read` reads regular files only and at most 8 MiB, so a link to a device or a named pipe is refused before it is opened. The walk stops at 20,000 entries. A scan as a whole has a read budget, and directory listings have an entry budget and visit each real directory once, so links that loop or that all point at one large file cannot multiply the work. Brace expansion, frontmatter nesting and import expansion each have a budget that is enforced while the work runs, never after it. Token counts stay exact for a text of ordinary size, and a text over the counting bound gets an estimate and a diagnostic that says so. An exceeded budget is a diagnostic or a skip reason on that file, and the scan goes on |

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
- **Navigation state** lives in the URL hash (project, view, selection), so reload and the back button work in a browser. A file selection may name one place the file loads: `at` holds the load's key, made from its kind, its activation and how it got there (rule, catalog entry, or host file and line). A `LoadId` never goes in the URL, because a rescan renumbers loads. Without `at`, or when no load matches it, the selection means every place of the file.
- **Editor state** is one reducer per open file: `{ path, base, loaded, text }`. Dirty is computed, never stored.
- **No agent-specific code.** Trees, forms and menus render from `AgentInfo`, `Load`, `Hook`, `Extension`, `Slot` and `Diagnostic`.
- **Views.** Context (tree grouped by agent: session start, on access, on invoke, may read; a second mode lists the session-start blocks in the order the model receives them, with the literal text from `render` and the selected load's piece marked). The detail pane shows one card per load of the file, the selected one first, each with the text `load_texts` returns for it. Memory. Hooks (list by event, switch, form generated from `HookSchema` with a raw JSON tab, test run). Doctor (rows link to their subject; a row with a `fix` offers it through the normal preview). History (diff, revert, pending changes on top). Sessions (the project's recorded sessions; one bar per call of the model with the cache read, the cache write and the uncached input the API reported, and a mark at each compaction; a selected call shows its pieces in the order the model received them, each with its kind, its estimate, whether the previous call already sent it, and the cache marks; a piece opens its text; a list ranks the largest pieces). Search and the path simulator sit in the top bar and act on the tree.
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
| write pipeline, every order of steps | a TLA+ model of apply, its undo, recovery, revert and the data-directory lock, checked with TLC for two instances, two files, one crash, one failed operation and two outside edits. Seven invariants, each with a broken variant of the model that must fail on it. `formal/write-pipeline/run.sh` runs it in about four minutes, and `.claude/skills/write-pipeline/SKILL.md` says which code it covers and how to read a failure. `scripts/check.sh` prints a notice when covered code changed after the commit the model was confirmed against. Two findings are open: `in-place-torn` (a failed in-place write of a hard-linked file is recorded as aborted) and `edit-in-window` (section 4) |
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
8. **Past sessions.** The session spec and its oracle, then the reader and the trace operations, then the view.

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

## 14. Past sessions

The scan predicts: it reads the files on disk and says what a new session would load. A trace reports: it reads the record an agent kept of a past session and says what the model received in each call. The two never mix. A trace is not part of a snapshot, has no fingerprint, and is never an input of a plan.

From version 2.1.302 Claude Code records every request it sends, and a reader can rebuild each request body and check it against a digest that the record carries (the session spec, sections 3 and 4). Codex CLI and Gemini CLI record the conversation and the token counts, not the request (the session spec, section 16). So the base of the model is a list of calls with their usage, and the request a call sent is a layer on top that says whether it is exact.

### Types

```rust
pub struct TraceId(String);   // the agent's own id of a session. For Claude Code the transcript's file stem.

/// One row of the session list. Read from the head and the tail of the record, never the whole file.
pub struct TraceInfo {
    pub agent: AgentId,
    pub id: TraceId,
    pub file: AbsPath,
    pub bytes: u64,
    pub title: Option<String>,     // the agent's own title for the session, else its first prompt. The operator's text.
    pub started: Option<String>,   // RFC 3339, as ChangeRecord.at
    pub last: Option<String>,
    pub cwd: Option<AbsPath>,      // where the session started
}

/// A session, or one subagent of it. `part` is the agent's id for the subagent.
pub struct TraceKey { pub agent: AgentId, pub id: TraceId, pub part: Option<String> }

pub struct Trace {
    pub info: TraceInfo,
    pub read: u64,                   // bytes of the record this trace was built from. A detail request names it.
    pub steps: Vec<Step>,            // one per call of the model, in the order they were made
    pub boundaries: Vec<Boundary>,
    pub parts: Vec<PartInfo>,        // subagents recorded under the session
    pub top: Vec<TopPiece>,          // the 30 largest pieces, each at the step that sent it first
    pub problems: Vec<TraceProblem>,
}

pub struct Step {
    pub at: String,
    pub model: Option<String>,
    pub usage: Option<Usage>,        // None: the record holds no reply (interrupted, failed, still running)
    pub sent: Option<Sent>,          // None: the agent recorded no request for this reply
    pub reply: Vec<ReplyPiece>,
}

/// What the API reported. `input` is the part that was neither read from the cache nor written to it.
pub struct Usage { pub input: u64, pub cache_read: u64, pub cache_write: u64, pub output: u64, pub thinking: Option<u64> }

pub struct Sent {
    pub exact: bool,                 // the rebuilt request matches the digest the agent recorded for it
    pub pieces: u32,
    pub shared: u32,                 // how many leading pieces equal the previous step's request, piece for piece
    pub by_kind: Vec<KindTotal>,     // estimated tokens per kind
    pub marks: Vec<u32>,             // indexes of the pieces that end at a cache mark
}
pub struct KindTotal { pub kind: PieceKind, pub shared: u32, pub fresh: u32 }
pub struct ReplyPiece { pub kind: PieceKind, pub label: Option<String>, pub bytes: u64, pub tokens: u32 }

/// A compaction. `before` is the index of the first step after it.
pub struct Boundary { pub before: u32, pub pre_tokens: Option<u64>, pub post_tokens: Option<u64> }
pub struct PartInfo { pub part: String, pub label: Option<String>, pub description: Option<String>, pub bytes: u64 }
pub struct TopPiece { pub step: u32, pub piece: u32, pub kind: PieceKind, pub label: Option<String>, pub tokens: u32 }

pub enum TraceProblem {
    NoRequests { steps: u32 },                   // replies whose request the agent did not record
    Inexact { steps: u32 },                      // requests that do not match their digest, or hold a message that did not resolve
    UnreadableLines { lines: u32 },
    RecordTooLarge { bytes: u64, limit: u64 },
}

/// One piece of a request, in the order the model received them: system blocks, tool definitions, messages.
pub struct Piece {
    pub place: PiecePlace,
    pub kind: PieceKind,
    pub label: Option<String>,       // a short name from the record: the tool, the skill, the hook event
    pub file: Option<AbsPath>,       // the file the piece came from, when the record names it
    pub bytes: u64,
    pub tokens: u32,                 // o200k estimate of this piece alone
    pub head: String,                // its first 160 characters
}
pub enum PiecePlace { System { block: u32 }, Tool { index: u32 }, Message { message: u32, role: Role, block: u32 } }
pub enum Role { User, Model, System, Developer }
pub enum PieceKind {
    System, ToolDefinition, Instructions, Catalog, Environment, Reminder, HookOutput,
    UserText, Command, FileMention, ExtensionBody, Summary,
    ModelText, Thinking, ToolCall, ToolResult, Frame, Other,
}

pub enum StepPieces { Current { pieces: Vec<Piece> }, Stale }
pub enum PieceRef { Sent { piece: u32 }, Reply { piece: u32 } }
pub enum PieceText { Current { text: String, bytes: u64, cut: bool }, Stale }
```

### The rules the types carry

- **Real and estimated numbers stay apart.** `Usage` holds only what the agent recorded from the API. `tokens` on a piece is always the o200k estimate. The difference between the prompt totals of two consecutive steps is real, so the UI can say how much a step added without an estimate.
- **A piece is a span of text with one kind.** The pieces of a message joined give the message's text, as the pieces of a `RenderedBlock` give its text. Separators and wrappers are `Frame`.
- **A kind comes from the record, not from guessing at prose.** An adapter labels a piece from the record type, the content block type, the flags of the conversation record it came from, or the attachment whose rendered text it holds. A piece that nothing explains is `Other`.
- **`shared` is what a prompt cache can reuse.** A cache reads a prefix. A request that shares fewer pieces with the previous request than that request had has changed its own past, and `shared` is the index of the first piece that differs.
- **A step without `sent` is still a step.** Its reply and its usage are exact. That is the whole view for an agent that records no requests, and for the turns an old Claude Code wrote.

### Reading

`Agent::traces` and `Agent::trace` have empty defaults. An agent without a reader returns nothing, so the UI needs no flag and no agent name.

`trace` returns a `TraceData`: every distinct message once, as its pieces, and every step as references to them. `trace.rs` projects that into `Trace`, `StepPieces` and `PieceText`. It counts tokens once per distinct message and computes `shared`, `by_kind` and `top`. No adapter builds those.

A record is far larger than the 8 MiB that a scan may read from one file. `ProbeFs` gains a bounded line reader (at most 512 MiB per record and 64 MiB per line) and bounded reads of the head and the tail of a file. A record over the limit yields the steps read so far and `RecordTooLarge`. A last line that does not parse belongs to a record that is still being written, and is ignored.

A session id in a request is matched against the adapter's own listing. A string from the client is never joined into a path.

The host keeps the two traces it opened last in memory, keyed by file, length and modification time. That is a memo of a pure function of the file, checked on every use. It is the one exception to "the host keeps no state" in section 2, and it is not state of record.

### What the first version leaves out

Following a session while it runs, comparing a session's first request with what the scan predicts, the full text of a tool output that the agent saved outside the record, and readers for Codex CLI and Gemini CLI.

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

**Changed after the security review.** The first version of this section had recovery finish a pending change on its own ("roll forward"). A reviewer showed two ways that goes wrong: a save that failed with a permission error was applied silently on a later history read, and a file that returned to its old content (a `git checkout`) was overwritten at the next start. Recovery now only classifies, a failed apply undoes itself, and the user decides about anything in between.

**Added after the security review (second batch).** The reviewer reproduced six more defects, and each one became a rule above: a trash of a nested Gemini context file took its whole directory, because a slot shape meant for directory items was reused for "a file in any subdirectory" (the rule on directory items now reads from the snapshot, not from the path); a link to `/dev/zero` at a settings path made a scan run without end (the one reader now checks kind and size); Gemini imports multiplied with every sibling (the expander has a node budget); a save through a dangling link created a file outside the project (refused); a save split hard links (written in place); and a second instance finished or broke the first instance's change (the data directory lock).

**Accepted after a capture.** `Delivery::SystemMessage` (a system-role message inside the conversation) and `ContextBlock.preceded_by` with `Unsized { BuiltIn, FirstPrompt }` were added because the lead captured one real Claude Code 2.1.329 request: it sends the subagent listing before the skill listing, in a system-role message, with the first prompt in between. `preceded_by` is a list so that one block can follow several gaps. The core pass starts the list of the first session block of every agent with `BuiltIn`. The Claude adapter adds `FirstPrompt` to its first listing block, so a project with skills and no instruction file starts with `[BuiltIn, FirstPrompt]`.

**A budget per kind of work (third batch).** A worker that fixed the reader measured two more shapes of the same class: a commands directory with four links to itself did not finish listing, and 500 links to one 8 MiB file made a scan read 4 GB. Closing shapes one at a time does not end, so the rule is now general: a scan has a budget for each kind of work a repository can cause (listing, reading, expanding, counting), each enforced in the one place that does that work, and each exceeded budget is a typed diagnostic.

**Past sessions, added on the owner's request.** The owner asked whether the tool could also analyse a real session, not only predict one. A specification unit measured the record first: on the research machine all 9,923 request records of Claude Code rebuilt to the digest they carry, and an oracle compared rebuilt requests with the bytes a scripted endpoint received in nine scenarios. Two shapes were weighed for the model. In the first, every step carries its full list of pieces: simple to render, but a session of 500 calls with hundreds of messages each repeats almost every piece in every step. In the second, each distinct message is stored once and a step refers to it, which is how the record itself is built. The second was chosen for the core (`TraceData`), and the wire gives an overview with one row per step plus one step's pieces and one piece's text on demand, so the UI never sees the record's own chaining. A third option, mixing recorded sessions into the snapshot, was rejected: a prediction and a report would then share types that mean different things. The host memo of the last two traces is accepted because a record of tens of megabytes cannot be parsed again for every click.

## Deviations

- Contract unit: `Preview` gains `conflicts: Vec<Conflict>`, because section 4 says a revert reports a file at neither state "as a conflict in the preview" and the listing has no field for it.
- Contract unit: `ChangeSummary` is `{ edit: ChangeEdit, agent: Option<AgentId>, targets: Vec<AbsPath> }`, facts only, as rule 3 of `AGENTS.md` requires. `ChangeEdit` mirrors the `Edit` variants.
- Contract unit: `ApiError::ReadOnly`, `InvalidEdit` and `Forbidden` carry `why: RuleRef` instead of prose. `ExtStatus::Shadowed { by }` holds an `ExtId`.
- Write pipeline unit: `parked.rs` (the record type, its path rule and the scan's read) lives in `cm-core`, not `cm-host`, because the Claude planner in `cm-core` must build the record. It still writes nothing: the planner emits the record as an ordinary `Write` operation, and `apply` writes it.
- Write pipeline unit: `HostConfig` gains `data_dir`, and `PlanCx` gains `snapshot` and `parked_dir`. Neither is in `model.rs` or the `api!` table.
- Write pipeline unit: `Edit::Trash` on the entry file of a `NamedDir` slot item that the agent lists as a skill, command or subagent (`skills/<name>/SKILL.md`) plans `TrashDir` over the directory with `Base::Tree`. Any other file, such as Gemini's `src/GEMINI.md`, is trashed alone. An agent's plan may write below a `NamedDir` slot only inside such a listed item or one that the same change creates. The client sends no tree hash; the plan computes it, so a file added to the directory between `plan` and `apply` yields `Stale`. A `Save` carries its own base, so an outside write between `plan` and `apply` yields `Conflict`, not `Stale`.
- Write pipeline unit: a directory that goes to the trash is journaled as one operation per UTF-8 file plus a last operation with neither blob, which stands for the directory. A directory over 16 MiB, or holding a file that is not UTF-8, warns `NotRevertible` and journals only what it can restore as text.
- Write pipeline unit: `Revert` accepts paths outside the snapshot (a trashed file is no longer in it). It still refuses a path the snapshot marks read only, and the guards still run. A revert resolves a record that never finished by marking it `Aborted`, because none of that change is on disk once the revert completes.
- Write pipeline unit: recovery marks a record `NeedsAttention` in its `.pending.json` and writes a record it seals or aborts as `<id>.json`. The state of each file is in the `Report` that `recover` returns, not in `ChangeRecord`. Recovery also removes the `.cm-<id>-*.tmp` files a crashed run of that record left beside its targets; they are the tool's own temp files, not agent files.
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
- Codex planners: a `HookId` that no agent lists is `ApiError::NotFound` (since the one-way unit, from the core planner, not from an adapter). Switching a hook back on removes its `enabled` key (and the table when empty) instead of writing `enabled = true`, so off then on restores a file byte for byte except that a file with no final newline gains one. `cfg::toml` gained `remove_pruned`, `table_set_key`, `table_merge` and `element_remove` for this.
- Gemini planners unit: `Warning` gains `HookKeyShared { others: u32 }`. `hooksConfig.disabled` matches a hook's `name` or `command`, so one switch can turn off several hooks, and no existing warning carries the count. `Agent` gains `write_warnings(cx, path, before, after)`, which `Planner::finish` calls for every planned write so that a plain save gets an agent's warnings, and `Edit::Trash` on a file an agent owns goes to that agent's `plan` first (the generic trash applies when it returns `None`), so a Gemini memory note and its index line leave in one plan. Gemini hook enable removes the key from every editable settings layer that lists it, and refuses with `ReadOnly` when a managed layer lists it.
- Integration unit: the reference-graph cap (500 nodes per scan, over all agents) reports `Problem::WalkCapped { entries: 500 }`, the same variant as the project walk, which reports 20,000. The value tells them apart. No new `Problem` variant, so the UI catalogs stay as they are.
- Integration unit: the reference pass treats a whole line as claimed when an adapter found an import on it, because `Via::Import` records the line and not a byte range. A reference on the same line as an import is not followed.
- Integration unit: `Budget` is computed by the core pass from `session` and `loads`, over what the adapters return. `on_access_max` counts the delivered tokens of every `OnAccess` load and of the imports below it.
- Integration unit: `ProbeFs`'s `Stat` gains `executable`, so the doctor can report `HookNotExecutable` through the one reader.
- Integration unit: `simulate` takes its notes from the loads. A `ModelMayRead` load with a discovery rule inside the touched directory tree contributes that rule (Codex: `codex.agents_md.below_cwd`). No `Agent` method was added. An extra load is not in `Snapshot.files`, so `read_file` refuses it until a scan lists it.
- Integration unit: `search` reads the raw entry of a hook only when the hook's source file is not already searched as text, so a hook in a settings file is not reported twice.
- Integration unit: `projects` is sorted by path. No adapter's `known_projects` reports when a project was last used, so there is no recency order yet.
- Integration unit: `cm-host` gains a direct `libc` dependency (already in the lockfile) to signal the process group of a hook, and `the_os_trash_takes_the_file` runs only with `CM_TEST_REAL_TRASH=1`.
- Claude planners unit: `ScanCx` gains `parked_dir: Option<&AbsPath>` (`None` where no data directory exists, so no hook is reported as parked), and `scan::scan_with_parked` is `scan_with` plus that directory. The host calls it. `ParkedHook::index` counts the event's handlers with every parked one in, so parked hooks rebuild the original order in any restore order. `ParkedHook` gains an optional `restore` (the text the park cut out, its offset, and the hash of the file after the cut), so a switch off and on returns the settings file to its exact bytes while nothing else touched it, and falls back to inserting the handler by value otherwise.
- Claude planners unit: `Planner::plan` offers `Edit::Trash` to the agents' `plan` before the generic plan, through `agent_plan` in `edit.rs`, so that trashing a memory topic also removes its index line. A `None` answer keeps the generic plan.
- Claude planners unit: `AddMemory` has no field for the memory kind, so the topic file gets `metadata.type: project` and no `modified` stamp, since a plan has no clock. A title with no `[a-z0-9]` (Korean, for one) gets the file name `memory-<8 hex of its hash>`.
- Claude planners unit: the session blocks gain `SkillCatalog` and `AgentCatalog`, both `Approximate`: no run pins their framing, only each entry and the character budget. The oracle comparison keeps to the `Exact` blocks. `MOVING_VARS` gains `SLASH_COMMAND_TOOL_CHAR_BUDGET`, which overrides the skill listing budget. Past the budget, descriptions go from the least-used skill first by `skillUsage` in `~/.claude.json`, aged from the newest use on record because a scan has no clock.
- Write safety unit: the pipeline lock is `apply.lock` in the data directory. A read-only host's recovery classifies without it, because it writes nothing and opening the lock file would create it.
- Write safety unit: the root in `Warning::ThroughSymlink` ("below the slot's root") is the deepest of the home directory, the project root and `cwd` that holds the path. A path below none of them is checked at its own entry only. `ProbeErrorKind` gains `DanglingLink`; `is_not_found` still holds for it, so a reader sees such a link as absent and only the planner refuses it.
- Write safety unit: `ProbeFs`'s `Stat` gains `links`, the hard link count, so the plan warns `HardLinked` through the one reader. `apply` decides to write in place from the link count on disk at the moment of the write.
- Write safety unit: `ProbeFs` gains `listing`, the entries of a directory with `not_utf8`, the count of names it has no entry for. `list` returns the entries alone, so the adapters read directories as before. The walk of a directory trashed whole does not follow a symlink inside it.
- Write safety unit: `Place` gains `InSubdir { dir, file }`, a file with a fixed name in a subdirectory that the user names. Gemini's subdirectory context slot uses it instead of `NamedDir`, so no rule about directory items, such as `TrashDir` or a write inside an item, applies to `src/GEMINI.md`. `NamedDir` is only for items that are directories. The listed-item check in `Planner::item_entry` stays for those.
- Read bounds unit: the import node budget is 1,000 nodes per root file and belongs to `CyclePolicy::PerChain`, the policy that lets a file expand once per path. Claude's one seen-set expands each file once, so its nodes grow with the files and it has no budget. `tests/hostile_repo.rs` shows the fan-out shape stays linear for Claude.
- Typed reasons unit: `ApiError::ReadOnly` carries a `ReadOnlyReason` (host, the file's `ReadOnlyWhy`, or an agent rule) instead of a `RuleRef`. `ApiError` gains `HookRun`, `BadRequest` and `HistoryCorrupt` for sentences that Rust built before. `Skip::Invalid` carries an `Invalid` enum instead of a code string. Skips of a nested worktree, a rejected extension and a rejected Codex skill or subagent are `Skip::NotRead` with a rule. `Session` gains `core_rules`.
- Scan budgets unit: the directory lister is `walk::list_tree` (and `walk::list_dir` for one level). Flat listings go through it as well, so every adapter listing has the entry budget of 2,000 and the depth limit of 8, and a test fails on any `.listing(` call outside `walk.rs` and `probe_fs.rs`: `ProbeFs::listing` is the one-level read under the lister, crate-private, and `ProbeFs::list` is gone. The plan's tree of a directory trashed whole lists through `walk::list_tree_with` at its own limits (depth 16, 10,000 entries); a listing it could not finish is `partial` and the tree is not revertible. The Codex plugin content hash lists through `walk::list_tree_with` with no depth limit and its own entry limit (5,000 in a scan, 200,000 in `inspect`), because Codex hashes the whole tree and a capped tree has no hash. It still lists each real directory once.
- Scan budgets unit: the counting bound is a bound on text analysis, `md::MAX_ANALYZED_BYTES` (1 MiB). Past it a token count is an estimate from the first MiB, and the reference pass and the missing-link check look for links only in the first MiB, because a span scan costs as much per byte as a count. One `TextPartlyAnalyzed` diagnostic per file says both, and a session block whose loads deliver more than the bound is `Approximate`. A text of at least 64 KiB is counted once per content, keyed by the hash of its first MiB.
- Scan budgets unit: besides the 4 MiB per root file, import expansion has a budget of 8 MiB for the whole scan (`imports::MAX_SCAN_EXPANDED_BYTES`), charged in `ProbeFs` by the expander. Gemini expands every context file on its own, so a hundred directories whose GEMINI.md imports one 1 MiB file three times took 58 s and 370 MB without it. A root that the shared budget stopped carries no diagnostic of its own; the scan reports one `ScanExpansionExhausted`.
- Scan budgets unit: the reference pass knows a file by its real path, so links that all point at one file are one reference under the first name that reaches it. Before, 500 links to one 8 MiB file were 500 references of 8 MiB each.
- Load text unit: a block's `tokens` is the sum of its pieces' counts, each piece counted alone, for every agent. Claude and Gemini counted the joined text before, so their block totals move by a few tokens at the joins. `parts` now follow the pieces in order, so a header between two files is glue in its place instead of a share of the next file's part.
- Load text unit: the frame of a load is not its text. Claude's `Contents of <path> (<label>):` header and the `- ` of a listing line, Codex's line break after a catalog line and Gemini's `--- Context from: <p> ---` lines are frame. A Gemini context file's `Delivered` now counts its text without that frame.
- Load text unit: a Gemini import is inlined into its host's text, so it has no piece of its own. `load_texts` returns the imported file with its own imports inlined, which is a run of the host's piece.
- Load text unit: `LoadText.whole_file` compares the text with the file after trimming both, because Claude and Codex trim a file's ends and the operator reads that as the whole file.
- Load text unit: `Problem::BodyAlsoImported` carries the host file and line beside the import's `LoadId`, because a message is a function of the variant's fields and cannot look a load up.
- One-way unit: `UpdateHook`, `RemoveHook` and `SetHookEnabled` resolve their hook once, in `Planner`, with `Snapshot::hook`. `PlanCx` gains `hook: Option<&Hook>`, set only for those three edits, and only the agent that lists the hook plans the edit. A hook no agent lists is `NotFound`. A switch on a hook whose `toggle` is `Unavailable` is `InvalidEdit { field: "enabled", why }` for all three agents, where Codex used `field: "hook"`.
- One-way unit: `Agent::write_warnings` carries every warning that belongs to a file, for all three agents, so a plain save gets it too: `AgentAlsoWrites` for the user Codex `config.toml`, Claude `settings.local.json`, Gemini's settings layers and the private memory index. A hook edit in the project `.codex/config.toml` no longer carries `AgentAlsoWrites`, because spec 7.2 says Codex rewrites that file only through migration. `TakesEffect` stays with the hook planners, because its text is about hooks. A plan that writes nothing carries no write warning, and `Planner::add_write_warnings` still drops a repeat, because a change that writes two of one agent's files would repeat an agent-level warning.
- One-way unit: the Gemini scan lists the on-access context files by calling `jit_files` for every directory the walk entered, the same function `on_access` uses. `Candidate` gains `skip: Option<Skip>`, which an adapter sets when it finds a file and does not load it. Gemini sets `Skip::Untrusted` for every on-access file of an untrusted folder, and the simulator reports such a candidate as a skipped load without reading it, so the scan and the simulator agree and the file stays in the snapshot.
- Trace reader unit: the `Piece` of section 14 is `TracePiece` in Rust and TypeScript, because `Piece` already names a run of a session-start block. `TraceData` stores each distinct content block, system block and tool definition once as its spans (`trace::Block`), and a message as the list of its blocks, so a request that sends a prefix of a reply shares that reply's blocks. `trace.rs` derives `NoRequests` and `Inexact` from the steps; the reader reports only `UnreadableLines` and `RecordTooLarge`.
- Trace reader unit: the host memo is keyed by the request's `cwd` and `TraceKey` and holds the file the adapter's listing named. Every use compares that file's length and modification time with the ones the trace was read at, and reads it again when they differ.
- Trace reader unit: Claude Code takes a session folder's key from the working directory in NFC form (session spec, section 1.2). The reader takes it from the path as the disk names it, because the crate has no Unicode normalization. It lists the folders whose names start with the first 200 characters of that key and confirms each record by its first `cwd`, so a directory whose name is stored decomposed lists no sessions.
- Trace reader unit: the session list takes the first `cwd` from the first 4 MiB of a record and the titles and the last time from its first 4 MiB and last 64 KiB. A record with no `cwd` in that head is not listed. A title record that sits only in the middle of a long record is not seen.
- Trace reader unit: the trace of a subagent carries its session's `TraceInfo` with `file` and `bytes` of the subagent's record and `title` set to the subagent's description, else its type. It has no `parts`.
