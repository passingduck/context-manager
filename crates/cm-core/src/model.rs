//! The read model and the write model of `docs/design.md` sections 3 and 4.
//!
//! Wire convention: field names are camelCase. A plain enum is a camelCase string. An enum with
//! data is tagged with `kind`, and a variant that wraps one other type is a struct variant with one
//! named field (`Skipped { skip }`). Newtypes are transparent strings or numbers.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::agents;

/// A record: a struct with camelCase fields.
macro_rules! records {
    ($($item:item)*) => {$(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export)]
        $item
    )*};
}

/// An enum whose variants carry no data: a camelCase string on the wire.
macro_rules! choices {
    ($($item:item)*) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export)]
        $item
    )*};
}

/// An enum with data: tagged with `kind`, struct variants with camelCase fields.
macro_rules! variants {
    ($($item:item)*) => {$(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
        #[ts(export)]
        $item
    )*};
}

/// A transparent newtype over a string or a number.
macro_rules! ids {
    ($($item:item)*) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export)]
        $item
    )*};
}

// ---------------------------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------------------------

/// Why a string is not an absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    NotAbsolute(String),
    Nul,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::NotAbsolute(raw) => write!(f, "not an absolute path: {raw:?}"),
            PathError::Nul => write!(f, "a path cannot contain a NUL byte"),
        }
    }
}

impl std::error::Error for PathError {}

/// Absolute, lexically normalized, UTF-8. Built only by [`AbsPath::parse`] at a boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, TS)]
#[ts(export, type = "string")]
pub struct AbsPath(String);

impl AbsPath {
    /// Accepts `/a/b/../c//d/.` and stores `/a/c/d`. A `..` at the root stays at the root.
    pub fn parse(raw: &str) -> Result<AbsPath, PathError> {
        if raw.contains('\0') {
            return Err(PathError::Nul);
        }
        if !raw.starts_with('/') {
            return Err(PathError::NotAbsolute(raw.to_owned()));
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in raw.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        Ok(AbsPath(format!("/{}", parts.join("/"))))
    }

    pub fn root() -> AbsPath {
        AbsPath("/".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &std::path::Path {
        std::path::Path::new(&self.0)
    }

    /// Joins a relative path and normalizes the result. An absolute `rel` replaces `self`.
    pub fn join(&self, rel: &str) -> AbsPath {
        if rel.starts_with('/') {
            return AbsPath::parse(rel).unwrap_or_else(|_| self.clone());
        }
        AbsPath::parse(&format!("{}/{rel}", self.0)).unwrap_or_else(|_| self.clone())
    }

    pub fn parent(&self) -> Option<AbsPath> {
        if self.0 == "/" {
            return None;
        }
        let cut = self.0.rfind('/').unwrap_or(0);
        Some(AbsPath(if cut == 0 {
            "/".to_owned()
        } else {
            self.0[..cut].to_owned()
        }))
    }

    pub fn file_name(&self) -> Option<&str> {
        if self.0 == "/" {
            None
        } else {
            self.0.rsplit('/').next()
        }
    }

    pub fn starts_with(&self, base: &AbsPath) -> bool {
        self.as_path().starts_with(base.as_path())
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AbsPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AbsPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        AbsPath::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// `"claude" | "codex" | "gemini"`. Checked against the registry at parse. Nothing matches on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, TS)]
#[ts(export, type = "string")]
pub struct AgentId(String);

impl AgentId {
    pub fn parse(raw: &str) -> Result<AgentId, String> {
        agents::registry()
            .iter()
            .map(|agent| agent.id())
            .find(|id| id.0 == raw)
            .ok_or_else(|| format!("unknown agent {raw:?}"))
    }

    /// For adapters: each adapter names itself.
    pub(crate) fn builtin(name: &'static str) -> AgentId {
        AgentId(name.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Snapshot {
    /// The view of one agent.
    pub fn view(&self, agent: &AgentId) -> Option<&AgentView> {
        self.agents.iter().find(|view| view.agent == *agent)
    }

    /// The one lookup of a hook by id: the view of the agent that lists it, and the hook.
    pub fn hook(&self, id: &HookId) -> Option<(&AgentView, &Hook)> {
        self.agents.iter().find_map(|view| {
            let hook = view.hooks.iter().find(|hook| hook.id == *id)?;
            Some((view, hook))
        })
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AgentId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        AgentId::parse(&raw).map_err(serde::de::Error::custom)
    }
}

ids! {
    /// `"sha256:<hex>"` of the file bytes. The compare-and-swap token for writes.
    pub struct Revision(pub String);
    /// Hash over the stat of every path a scan probed.
    pub struct Fingerprint(pub String);
    /// `"codex.agents_md.project"`. Resolves to a [`RuleSpec`].
    pub struct RuleRef(pub String);
    /// Index into `AgentView.loads`, valid within one snapshot.
    pub struct LoadId(pub u32);
    /// hash(agent, source file, locator, digest of the stored entry).
    pub struct HookId(pub String);
    /// hash(agent, kind, file).
    pub struct ExtId(pub String);
    pub struct SlotId(pub String);
    /// A ULID, so history sorts and pages by id.
    pub struct ChangeId(pub String);
}

/// Lowercase hex of the SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Revision {
    pub fn of(bytes: &[u8]) -> Revision {
        Revision(format!("sha256:{}", sha256_hex(bytes)))
    }
}

impl RuleRef {
    /// The id is a literal, so a test can find every rule the code names.
    pub fn new(id: &'static str) -> RuleRef {
        RuleRef(id.to_owned())
    }
}

// ---------------------------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------------------------

records! {
    pub struct Snapshot {
        /// The directory a session would start in.
        pub cwd: AbsPath,
        pub git_root: Option<AbsPath>,
        pub fingerprint: Fingerprint,
        /// Every file any agent looked at, present or absent, exactly once.
        pub files: BTreeMap<AbsPath, FileEntry>,
        /// Registry order.
        pub agents: Vec<AgentView>,
        pub diagnostics: Vec<Diagnostic>,
        /// The project tree walk.
        pub walk: WalkSummary,
    }

    pub struct WalkSummary {
        pub dirs: u32,
        pub files: u32,
        pub capped: bool,
    }

    pub struct FileEntry {
        /// `~/.claude/CLAUDE.md`, `./docs/a.md`.
        pub display: String,
        /// Set when the path is a symlink. Writes go to the target.
        pub real_path: Option<AbsPath>,
        pub format: Format,
        pub state: FileState,
        /// The most restrictive claim across agents.
        pub policy: EditPolicy,
        /// Which agents use this file.
        pub agents: Vec<AgentId>,
    }

    pub struct AgentView {
        pub agent: AgentId,
        pub presence: Presence,
        pub homes: Vec<HomeDir>,
        pub trust: Trust,
        /// What a static scan had to assume.
        pub assumptions: Vec<Assumption>,
        pub loads: Vec<Load>,
        /// The session-start context, in delivery order.
        pub session: Vec<ContextBlock>,
        pub budget: Budget,
        pub hooks: Vec<Hook>,
        /// Files a new hook may be added to.
        pub hook_files: Vec<HookFile>,
        pub memory: Vec<MemoryStore>,
        /// Skills, commands, subagents.
        pub extensions: Vec<Extension>,
        /// Places where a new item would load.
        pub slots: Vec<Slot>,
    }

    pub struct HomeDir {
        pub role: RuleRef,
        pub path: AbsPath,
        /// The environment variable that moved this directory, when one did.
        pub from_env: Option<String>,
        pub exists: bool,
    }

    pub struct Trust {
        pub level: TrustLevel,
        pub recorded_in: Option<AbsPath>,
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
        /// Set exactly when `status` is `Loaded` or `Truncated`.
        pub delivered: Option<Delivered>,
    }

    pub struct Delivered {
        pub bytes: u64,
        pub tokens: u32,
        /// Frontmatter or comments stripped.
        pub transformed: bool,
    }

    pub struct Limit {
        pub kind: LimitKind,
        pub value: u64,
        pub unit: Unit,
    }
}

choices! {
    pub enum Format { Markdown, Json, Jsonc, Toml, Yaml, Text, Binary }
    pub enum TrustLevel { Trusted, Unknown, Untrusted }
    pub enum Os { Macos, Linux, Windows, Other }
    pub enum Severity { Error, Warning, Info }
    pub enum Unit { Bytes, Chars, Lines, Tokens, Hops }
    pub enum Shadow { OnePerDirectory, EmptyOverride, FirstNonEmpty, ClaudeMdPresent, NameCollision }
    pub enum DocKind {
        Instructions, Rule, MemoryIndex, MemoryTopic, MemorySummary,
        Listing, SkillBody, CommandBody, SubagentPrompt, Reference,
    }
    pub enum LimitKind {
        ProjectDocBudget, IndexLines, IndexBytes, OnDemandChars, MaxFileBytes,
        SummaryTokens, ListingEntryChars, ListingBudgetChars, ImportDepth,
        /// The bytes one scan reads (`probe_fs::MAX_SCAN_READ_BYTES`).
        ScanReadBytes,
    }
}

variants! {
    pub enum FileState {
        /// `tokens` counts the whole file.
        Present { rev: Revision, bytes: u64, lines: u32, tokens: u32, mtime_ms: i64 },
        /// A probed location, or the target of a broken import.
        Absent,
        Unreadable { error: String },
        /// Over 8 MiB: listed, never read.
        TooBig { bytes: u64 },
        /// The scan had spent `limit` before it reached the file, so it did not read it.
        BudgetExhausted { limit: Limit },
    }

    pub enum ReadOnlyWhy {
        Managed,
        PluginCache { plugin: String },
        Generated,
        AgentState,
        Synced,
        NotUtf8,
    }

    pub enum EditPolicy {
        Editable,
        ReadOnly { why: ReadOnlyWhy },
    }

    pub enum Presence {
        Installed { bin: AbsPath },
        ConfigOnly,
        Absent,
    }

    pub enum Assumption {
        /// Sets Claude's on-demand cut and the Codex skill catalog budget.
        ContextWindow { tokens: u32 },
        Feature { name: String, on: bool, set_in: Option<AbsPath> },
        /// For example Claude `instructionFiles`, Gemini `context.fileName`.
        Mode { name: String, value: String, set_in: Option<AbsPath> },
        /// For example server-managed policy, MCP server instructions.
        Unknown { what: RuleRef },
    }

    pub enum Scope {
        Managed,
        User,
        Project { dir: AbsPath },
        Local,
        Plugin { id: String },
        Builtin,
    }

    pub enum Via {
        Discovery { rule: RuleRef },
        Import { host: LoadId, line: u32, depth: u8 },
        /// A link or a path the host names.
        Reference { host: LoadId, line: u32, depth: u8 },
        /// A generated catalog line, not file text.
        Listing { ext: ExtId },
    }

    pub enum Activation {
        SessionStart,
        /// The CLI adds it when the agent touches a matching file.
        OnAccess { rule: AccessRule },
        /// A skill or command body, a subagent prompt.
        OnInvoke,
        /// An import: active exactly when its host is.
        WithHost,
        /// Reference targets, memory topic files, Codex `AGENTS.md` below cwd.
        ModelMayRead,
    }

    pub enum AccessRule {
        UnderDir { dir: AbsPath },
        Glob { base: AbsPath, patterns: Vec<String> },
    }

    pub enum LoadStatus {
        Loaded,
        Truncated { limit: Limit, kept: u64, total: u64 },
        Skipped { skip: Skip },
    }

    /// Why a file is not usable. Only `Unreadable` carries text, and that text is the operating
    /// system's.
    pub enum Invalid {
        Unreadable { os: Option<String> },
        /// A directory, a device, a named pipe or a socket.
        NotRegular,
        NotText,
        /// The front matter does not parse.
        Frontmatter,
        Toml,
        /// A path the agent never reads, by where it is.
        Location { which: RefusedLocation },
        /// A Gemini import that leaves the project root.
        OutsideRoot,
    }

    pub enum RefusedLocation { ConnectorDownloads, Scratchpad, DeviceOrProcess }

    pub enum Skip {
        Shadowed { by: AbsPath, how: Shadow },
        BudgetExhausted { limit: Limit },
        TooLarge { limit: Limit },
        Empty,
        Missing,
        Untrusted,
        /// Claude: an import outside cwd that the user has not approved.
        NeedsApproval,
        /// Claude `claudeMdExcludes`.
        Excluded { pattern: String },
        FeatureOff { name: String },
        AlreadyLoaded { first: LoadId },
        /// The file cannot serve as instructions or as an extension.
        Invalid { why: Invalid },
        /// For example the legacy Codex prompts directory.
        NotRead { why: RuleRef },
    }
}

// ---------------------------------------------------------------------------------------------
// Session-start context and budget
// ---------------------------------------------------------------------------------------------

choices! {
    pub enum Delivery { SystemPrompt, SystemMessage, UserMessage, DeveloperMessage, ToolSpec }
    /// Content of unknown size that an agent sends between two session blocks.
    pub enum Unsized { BuiltIn, FirstPrompt }
    pub enum BlockLabel { Instructions, Memory, SkillCatalog, AgentCatalog, CommandCatalog }
    /// `Exact` is compared with the real CLI in tests.
    pub enum Fidelity { Exact, Approximate }
    pub enum TokenMethod { O200kEstimate }
}

records! {
    /// What the agent sends when a session starts. Structure only. The literal text comes from
    /// the `render` operation.
    pub struct ContextBlock {
        pub delivery: Delivery,
        pub label: BlockLabel,
        pub parts: Vec<Part>,
        pub tokens: u32,
        pub fidelity: Fidelity,
        /// Content of unknown size that the agent sends between the previous block and this
        /// one, in the order the agent sends it. The core pass starts the list of the first
        /// block of every agent with `BuiltIn`.
        pub preceded_by: Vec<Unsized>,
    }

    pub struct Budget {
        /// Sum of `ContextBlock.tokens`.
        pub session_start: u32,
        pub by_label: Vec<(BlockLabel, u32)>,
        /// If every `OnAccess` load fired.
        pub on_access_max: u32,
        pub method: TokenMethod,
    }
}

variants! {
    pub enum Part {
        Load { load: LoadId, tokens: u32 },
        Glue { tokens: u32 },
    }

    /// One run of a session-start block's literal text. A block's text is its pieces joined,
    /// and its `parts` count the same pieces in the same order.
    pub enum Piece {
        /// Text the agent adds around the loads: a header, a separator, the intro of a list.
        Frame { text: String },
        /// The text one load puts in front of the model, without the frame around it.
        Load { load: LoadId, text: String },
    }
}

impl Piece {
    pub fn text(&self) -> &str {
        match self {
            Piece::Frame { text } | Piece::Load { text, .. } => text,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Hooks, memory, extensions, slots
// ---------------------------------------------------------------------------------------------

records! {
    pub struct Hook {
        pub id: HookId,
        pub source: HookSource,
        /// The agent's own event name.
        pub event: String,
        pub matcher: Matcher,
        pub handler: Handler,
        pub state: HookState,
        pub toggle: Toggle,
    }

    pub struct HookSource {
        pub file: AbsPath,
        pub at: Locator,
        pub scope: Scope,
        pub registered: Registration,
    }

    pub struct Matcher {
        pub raw: Option<String>,
        pub eval: MatcherEval,
    }

    pub struct Handler {
        /// `"command"`, `"http"`, `"mcp_tool"`, `"prompt"`, `"agent"`, or the unknown type as written.
        pub handler_kind: String,
        /// The entry exactly as stored. Unknown keys survive an edit.
        pub raw: serde_json::Value,
        /// The command line, the URL, or the head of the prompt.
        pub summary: String,
        /// Normalized from seconds or milliseconds.
        pub timeout_ms: Option<u64>,
    }

    pub struct HookFile {
        pub file: AbsPath,
        pub scope: Scope,
        pub format: HookFormat,
        pub exists: bool,
        pub policy: EditPolicy,
    }

    pub struct MemoryStore {
        /// `"claude.auto"`, `"claude.agent:<name>"`, `"codex.memories"`, `"gemini.private"`.
        pub id: String,
        pub label: RuleRef,
        pub root: AbsPath,
        pub enabled: Enabled,
        pub index: Option<AbsPath>,
        pub entries: Vec<MemoryEntry>,
        pub write: MemoryWrite,
    }

    pub struct MemoryEntry {
        pub file: AbsPath,
        pub role: MemoryRole,
        pub indexed: Option<bool>,
        pub title: String,
        pub description: Option<String>,
    }

    pub struct Extension {
        pub id: ExtId,
        pub ext_kind: ExtKind,
        /// The name it is invoked by.
        pub name: String,
        pub description: Option<String>,
        pub file: AbsPath,
        pub scope: Scope,
        /// The always-loaded catalog line, if any.
        pub listing: Option<LoadId>,
        /// The text that loads on invoke.
        pub body: LoadId,
        pub status: ExtStatus,
    }

    pub struct Slot {
        pub id: SlotId,
        pub place: Place,
        pub new_kind: NewItemKind,
        pub scope: Scope,
        pub rule: RuleRef,
    }
}

choices! {
    pub enum HookFormat { ClaudeSettingsJson, CodexHooksJson, CodexConfigToml, GeminiSettingsJsonc }
    pub enum HookBlock {
        NeedsTrust, Modified, FolderUntrusted, FeatureOff, PluginDisabled, ManagedOnly, UnsupportedType,
    }
    pub enum MemoryRole { Index, Topic, Summary, Note, Generated }
    pub enum ExtKind { Skill, Command, Subagent }
    pub enum NewItemKind { Instructions, Rule, Skill, Command, Subagent, Memory }
}

variants! {
    pub enum Locator {
        Json { pointer: String },
        Toml { path: Vec<TomlSeg> },
        Frontmatter { path: Vec<String> },
        Parked { key: String },
    }

    pub enum TomlSeg {
        Key { name: String },
        Index { index: u32 },
    }

    pub enum Registration {
        SessionStart,
        OnSkillInvoke { ext: ExtId },
        WhileSubagent { ext: ExtId },
    }

    pub enum MatcherEval {
        All,
        Exact { values: Vec<String> },
        Regex,
        Ignored,
        Invalid { error: String },
    }

    pub enum HookState {
        Active,
        Disabled { by: DisabledBy },
        WillNotRun { why: HookBlock },
    }

    pub enum DisabledBy {
        Native { set_in: AbsPath },
        Parked,
        AllHooksOff { set_in: AbsPath },
    }

    pub enum Toggle {
        Native,
        Park,
        Unavailable { why: RuleRef },
    }

    pub enum Enabled {
        On,
        Off { by: String },
    }

    pub enum MemoryWrite {
        Direct,
        NotesDir { dir: AbsPath },
        ReadOnly { why: RuleRef },
    }

    pub enum ExtStatus {
        Active,
        Conditional { patterns: Vec<String> },
        Hidden { why: RuleRef },
        Shadowed { by: ExtId },
        Rejected { why: RuleRef },
        Disabled { by: String },
    }

    pub enum Place {
        File { path: AbsPath },
        NamedFile { dir: AbsPath, ext: String },
        /// An item that is a directory: `<dir>/<name>/` with its entry file, such as
        /// `skills/<name>/SKILL.md`.
        NamedDir { dir: AbsPath, file: String },
        /// A file with a fixed name in a subdirectory of `dir` that the user names, such as
        /// Gemini's `src/GEMINI.md`. The subdirectory belongs to the project and is no item.
        InSubdir { dir: AbsPath, file: String },
    }
}

// ---------------------------------------------------------------------------------------------
// Diagnostics and rule specs
// ---------------------------------------------------------------------------------------------

records! {
    pub struct Diagnostic {
        pub severity: Severity,
        pub agent: Option<AgentId>,
        pub at: Subject,
        pub problem: Problem,
        pub fix: Option<Edit>,
    }

    /// Agent knowledge that the UI explains. It lives next to the adapter, in both languages.
    pub struct RuleSpec {
        pub id: RuleRef,
        pub en: String,
        pub ko: String,
        pub evidence: String,
    }
}

variants! {
    pub enum Subject {
        File { path: AbsPath, line: Option<u32> },
        Load { agent: AgentId, load: LoadId },
        Hook { hook: HookId },
        Extension { ext: ExtId },
        Agent,
    }

    pub enum Problem {
        ImportMissing { raw: String },
        ImportOutsideRoot { raw: String },
        ImportTooDeep { max: u8 },
        /// The scan stopped expanding the imports of this root file after `max` import nodes,
        /// so what it shows for the file is less than the agent loads.
        ImportBudgetExhausted { max: u32 },
        /// The scan stopped expanding the imports of this root file before they passed `max`
        /// bytes, so what it shows for the file is less than the agent loads.
        ImportBytesExhausted { max: u64 },
        ImportCycle { path: AbsPath },
        ImportNeedsApproval { raw: String },
        AccidentalImport { raw: String },
        ReferenceMissing { raw: String },
        Truncated { limit: Limit, kept: u64, total: u64 },
        DroppedByBudget { limit: Limit },
        TooLarge { limit: Limit },
        ShadowedByEmptyOverride,
        AgentsMdIgnored { because: AbsPath },
        MemoryNotInIndex { index: AbsPath },
        IndexLinkMissing { target: String },
        IndexOverLimit { limit: Limit },
        HookProgramMissing { program: String },
        HookNotExecutable { path: AbsPath },
        HookUnknownEvent { event: String },
        HookMatcherInvalid { error: String },
        HookMatcherIgnored,
        HookWillNotRun { why: HookBlock },
        ExtensionRejected { why: RuleRef },
        CatalogOverBudget { used: u64, limit: Limit },
        ParseError { format: Format, detail: String, fatal_for_agent: bool },
        FrontmatterInvalid { field: String },
        LegacyLocation { why: RuleRef },
        ProjectUntrusted { skipped: Vec<RuleRef> },
        GlobMatchesNothing { glob: String },
        WalkCapped { entries: u32 },
        /// A listing of this directory stopped after `max` entries, so the scan can miss files
        /// below it that the agent loads.
        ListingCapped { max: u32 },
        /// A listing of this directory went no deeper than `max` levels, so the scan can miss
        /// files below that depth that the agent loads.
        ListingTooDeep { max: u32 },
        /// The scan read `max` bytes and then stopped reading files, so the files it did not read
        /// are shown as not read.
        ReadBudgetExhausted { max: u64 },
        /// The imports of the whole scan added `max` bytes, and the scan expanded no more, so the
        /// files it expanded after that show fewer imports than the agent loads.
        ScanExpansionExhausted { max: u64 },
        /// The text of this file is over `max` bytes, on disk or where it reaches the model. The
        /// scan counted the tokens of its first `max` bytes and estimated the rest, and it looked
        /// for links only in those first bytes.
        TextPartlyAnalyzed { max: u64 },
        /// A skill, command or subagent whose body also loads at session start, because `line`
        /// of `host` imports its file. The body then reaches the model in every session, and
        /// again when the item is invoked.
        BodyAlsoImported { import: LoadId, host: AbsPath, line: u32 },
    }
}

// ---------------------------------------------------------------------------------------------
// Write model
// ---------------------------------------------------------------------------------------------

variants! {
    pub enum Edit {
        /// An editor save. `base` Absent creates.
        Save { path: AbsPath, base: Base, text: String },
        /// A skill trashes its directory.
        Trash { path: AbsPath, base: Revision },
        NewItem { agent: AgentId, slot: SlotId, name: Option<String> },
        AddMemory { agent: AgentId, store: String, title: String, description: String, body: String },
        AddHook { agent: AgentId, file: AbsPath, draft: HookDraft },
        UpdateHook { hook: HookId, draft: HookDraft },
        RemoveHook { hook: HookId },
        SetHookEnabled { hook: HookId, enabled: bool },
        Revert { change: ChangeId },
    }

    pub enum Base {
        Absent,
        Rev { rev: Revision },
        /// Hash over a directory's paths and revisions.
        Tree { hash: String },
    }

    pub enum Action {
        Write { text: String },
        Trash,
        TrashDir,
    }

    pub enum Warning {
        /// Codex: an edited hook stops until the user trusts it again in `/hooks`.
        HookTrustReset,
        /// Gemini: an edited project subagent must be approved again.
        AgentAckReset,
        /// Codex `config.toml`, Gemini `settings.json`, Claude `settings.local.json`.
        AgentAlsoWrites { agent: AgentId },
        /// Gemini rewrites `settings.json` from its startup snapshot.
        SessionMayOverwrite { agent: AgentId },
        /// The file is read by more than one agent.
        SharedFile { agents: Vec<AgentId> },
        ThroughSymlink { target: AbsPath },
        /// The file has other names: the write goes in place and changes all of them.
        HardLinked { links: u32 },
        /// Next session, `/reload-plugins`, `/memory reload`.
        TakesEffect { agent: AgentId, when: RuleRef },
        /// Gemini: a new `@word` becomes an import attempt.
        NewImportToken { raw: String },
        /// A parked Claude hook exists only in this tool's data directory.
        ParkedOnThisMachine,
        /// A directory the plan could not capture completely (design section 4, step 4): the OS
        /// trash is the only copy.
        NotRevertible,
        /// Gemini: the switch is a name or command that other hooks share, so it switches them too.
        HookKeyShared { others: u32 },
    }

    pub enum ApplyResult {
        Applied { change: ChangeId, files: Vec<(AbsPath, Option<Revision>)>, not_trashed: Vec<AbsPath> },
        /// The edit plans to nothing: the disk already has the end state.
        AlreadyApplied,
        /// The disk changed since the preview. Here is the plan as it stands now.
        Stale { now: Preview },
    }

    pub enum ChangeEdit {
        Save,
        Trash,
        NewItem,
        AddMemory,
        AddHook,
        UpdateHook,
        RemoveHook,
        SetHookEnabled,
        Revert,
    }
}

choices! {
    pub enum ChangeState { Sealed, Pending, NeedsAttention, Aborted }
}

records! {
    pub struct HookDraft {
        pub event: String,
        pub matcher: Option<String>,
        pub handler: serde_json::Value,
    }

    /// Facts about a change, for the history list.
    pub struct ChangeSummary {
        pub edit: ChangeEdit,
        pub agent: Option<AgentId>,
        pub targets: Vec<AbsPath>,
    }

    pub struct ChangeSet {
        pub summary: ChangeSummary,
        pub ops: Vec<FileOp>,
    }

    pub struct FileOp {
        pub path: AbsPath,
        pub expect: Base,
        pub action: Action,
    }

    pub struct Preview {
        /// Hash of the change set. `apply` must reproduce it.
        pub digest: String,
        pub change_set: ChangeSet,
        /// For the diff view.
        pub before: BTreeMap<AbsPath, Option<String>>,
        pub warnings: Vec<Warning>,
        /// Files a revert finds at neither the before nor the after state.
        pub conflicts: Vec<Conflict>,
    }

    /// A file whose content no longer matches what an edit expected.
    pub struct Conflict {
        pub path: AbsPath,
        pub expected: Base,
        pub found: Base,
        pub current: Option<String>,
    }

    /// `history/changes/<ulid>.json`, or `<ulid>.pending.json`.
    pub struct ChangeRecord {
        pub id: ChangeId,
        pub at: String,
        pub cwd: Option<AbsPath>,
        pub summary: ChangeSummary,
        pub ops: Vec<JournalOp>,
        pub state: ChangeState,
    }

    /// `before` and `after` are blob ids.
    pub struct JournalOp {
        pub path: AbsPath,
        pub before: Option<String>,
        pub after: Option<String>,
    }

    /// A record with the text of each file before and after.
    pub struct ChangeDetail {
        pub record: ChangeRecord,
        pub files: Vec<ChangeFile>,
    }

    pub struct ChangeFile {
        pub path: AbsPath,
        pub before: Option<String>,
        pub after: Option<String>,
    }
}

// ---------------------------------------------------------------------------------------------
// Hook schema, test run, render, simulate, search, session
// ---------------------------------------------------------------------------------------------

records! {
    /// Events, matcher kind per event, handler kinds and their fields. Drives the hook form.
    pub struct HookSchema {
        pub events: Vec<HookEventSpec>,
        pub handler_kinds: Vec<HandlerKindSpec>,
    }

    pub struct HookEventSpec {
        pub name: String,
        pub matcher: MatcherKind,
    }

    pub struct HandlerKindSpec {
        pub kind: String,
        pub fields: Vec<HandlerField>,
    }

    pub struct HandlerField {
        pub key: String,
        pub ty: FieldType,
        pub required: bool,
    }

    /// The invocation an agent would use for a hook.
    pub struct ExecSpec {
        pub argv: Vec<String>,
        pub cwd: AbsPath,
        pub env: Vec<(String, String)>,
        pub stdin: String,
    }

    pub struct HookRun {
        pub spec: ExecSpec,
        pub exit_code: Option<i32>,
        pub stdout: String,
        pub stderr: String,
        pub ms: u64,
        pub timed_out: bool,
    }

    /// The literal text of one session-start block, as the ordered pieces it is made of.
    pub struct RenderedBlock {
        pub delivery: Delivery,
        pub label: BlockLabel,
        pub pieces: Vec<Piece>,
        pub tokens: u32,
        pub fidelity: Fidelity,
    }

    /// What the model receives from one load of a file.
    pub struct LoadText {
        pub load: LoadId,
        /// `None` when the load sends nothing: it is skipped.
        pub text: Option<String>,
        /// The text is the file as it is on disk, apart from white space at either end.
        pub whole_file: bool,
    }

    /// Per agent: what a path access adds.
    pub struct Simulation {
        pub agent: AgentId,
        /// Loads already in the snapshot that the access fires.
        pub fired: Vec<LoadId>,
        /// Loads the access adds that the snapshot does not list.
        pub extra: Vec<Load>,
        pub added_tokens: u32,
        pub notes: Vec<RuleRef>,
    }

    pub struct SearchResult {
        /// At most 500.
        pub hits: Vec<SearchHit>,
        pub capped: bool,
    }

    pub struct SearchHit {
        pub file: AbsPath,
        pub line: u32,
        pub column: u32,
        pub text: String,
    }

    pub struct Session {
        pub version: String,
        pub shell: Shell,
        pub read_only: bool,
        pub launch_cwd: AbsPath,
        pub home: AbsPath,
        pub os: Os,
        pub pending: Vec<ChangeId>,
        /// The rules that belong to no agent.
        pub core_rules: Vec<RuleSpec>,
    }

    pub struct AgentInfo {
        pub id: AgentId,
        pub display_name: String,
        pub rules: Vec<RuleSpec>,
        pub hook_schema: HookSchema,
    }

    pub struct KnownProject {
        pub path: AbsPath,
        pub exists: bool,
        pub agents: Vec<AgentId>,
    }

    pub struct FileContent {
        pub path: AbsPath,
        pub state: FileState,
        /// Set when the file is present and valid UTF-8.
        pub text: Option<String>,
    }
}

impl RenderedBlock {
    /// The block's literal text: its pieces joined.
    pub fn text(&self) -> String {
        self.pieces.iter().map(Piece::text).collect()
    }
}

variants! {
    /// The answer of `load_texts`.
    pub enum LoadTexts {
        /// One entry per load of the file, in load order.
        Current { texts: Vec<LoadText> },
        /// The disk no longer matches the snapshot the request names. Scan again.
        Stale,
    }
}

choices! {
    pub enum MatcherKind { None, Exact, Regex, ExactOrRegex }
    pub enum FieldType { String, Number, Bool, StringList, StringMap, Json }
    pub enum Shell { Desktop, Serve }
}

// ---------------------------------------------------------------------------------------------
// Past sessions (design section 14)
// ---------------------------------------------------------------------------------------------

ids! {
    /// The agent's own id of a session. For Claude Code the transcript's file stem.
    pub struct TraceId(pub String);
}

records! {
    /// One row of the session list. Read from the head and the tail of the record, never the
    /// whole file.
    pub struct TraceInfo {
        pub agent: AgentId,
        pub id: TraceId,
        pub file: AbsPath,
        pub bytes: u64,
        /// The agent's own title for the session, else its first prompt. The operator's text.
        pub title: Option<String>,
        /// RFC 3339, as `ChangeRecord.at`.
        pub started: Option<String>,
        pub last: Option<String>,
        /// Where the session started.
        pub cwd: Option<AbsPath>,
    }

    /// A session, or one subagent of it. `part` is the agent's id for the subagent.
    pub struct TraceKey {
        pub agent: AgentId,
        pub id: TraceId,
        pub part: Option<String>,
    }

    pub struct Trace {
        pub info: TraceInfo,
        /// Bytes of the record this trace was built from. A detail request names it.
        pub read: u64,
        /// One per call of the model, in the order they were made.
        pub steps: Vec<Step>,
        pub boundaries: Vec<Boundary>,
        /// Subagents recorded under the session.
        pub parts: Vec<PartInfo>,
        /// The 30 largest pieces, each at the step that sent it first.
        pub top: Vec<TopPiece>,
        pub problems: Vec<TraceProblem>,
    }

    pub struct Step {
        pub at: String,
        pub model: Option<String>,
        /// `None`: the record holds no reply (interrupted, failed, still running).
        pub usage: Option<Usage>,
        /// `None`: the agent recorded no request for this reply.
        pub sent: Option<Sent>,
        pub reply: Vec<ReplyPiece>,
    }

    /// What the API reported. `input` is the part that was neither read from the cache nor
    /// written to it.
    pub struct Usage {
        pub input: u64,
        pub cache_read: u64,
        pub cache_write: u64,
        pub output: u64,
        pub thinking: Option<u64>,
    }

    pub struct Sent {
        /// The rebuilt request matches the digest the agent recorded for it.
        pub exact: bool,
        pub pieces: u32,
        /// How many leading pieces equal the previous step's request, piece for piece. 0 when
        /// the previous step has no request.
        pub shared: u32,
        /// Estimated tokens per kind.
        pub by_kind: Vec<KindTotal>,
        /// Indexes of the pieces that end at a cache mark.
        pub marks: Vec<u32>,
    }

    pub struct KindTotal {
        pub kind: PieceKind,
        pub shared: u32,
        pub fresh: u32,
    }

    pub struct ReplyPiece {
        pub kind: PieceKind,
        pub label: Option<String>,
        pub bytes: u64,
        pub tokens: u32,
    }

    /// A compaction. `before` is the index of the first step after it.
    pub struct Boundary {
        pub before: u32,
        pub pre_tokens: Option<u64>,
        pub post_tokens: Option<u64>,
    }

    pub struct PartInfo {
        pub part: String,
        pub label: Option<String>,
        pub description: Option<String>,
        pub bytes: u64,
    }

    pub struct TopPiece {
        pub step: u32,
        pub piece: u32,
        pub kind: PieceKind,
        pub label: Option<String>,
        pub tokens: u32,
    }

    /// One piece of a request, in the order the model received them: system blocks, tool
    /// definitions, messages. Named `Piece` in design section 14; `Piece` already names a
    /// run of a session-start block.
    pub struct TracePiece {
        pub place: PiecePlace,
        pub kind: PieceKind,
        /// A short name from the record: the tool, the skill, the hook event.
        pub label: Option<String>,
        /// The file the piece came from, when the record names it.
        pub file: Option<AbsPath>,
        pub bytes: u64,
        /// o200k estimate of this piece alone.
        pub tokens: u32,
        /// Its first 160 characters.
        pub head: String,
    }

    pub struct TracesReq { pub cwd: AbsPath }
    pub struct TraceReq { pub cwd: AbsPath, pub key: TraceKey }
    /// `read` is `Trace.read` of the trace the client shows.
    pub struct TraceStepReq { pub cwd: AbsPath, pub key: TraceKey, pub read: u64, pub step: u32 }
    pub struct TracePieceReq { pub cwd: AbsPath, pub key: TraceKey, pub read: u64, pub step: u32, pub piece: PieceRef }
}

choices! {
    pub enum Role { User, Model, System, Developer }

    pub enum PieceKind {
        System, ToolDefinition, Instructions, Catalog, Environment, Reminder, HookOutput,
        UserText, Command, FileMention, ExtensionBody, Summary,
        ModelText, Thinking, ToolCall, ToolResult, Frame, Other,
    }
}

variants! {
    pub enum TraceProblem {
        /// Replies whose request the agent did not record.
        NoRequests { steps: u32 },
        /// Requests that do not match their digest, or hold a message that did not resolve.
        Inexact { steps: u32 },
        UnreadableLines { lines: u32 },
        RecordTooLarge { bytes: u64, limit: u64 },
    }

    pub enum PiecePlace {
        System { block: u32 },
        Tool { index: u32 },
        Message { message: u32, role: Role, block: u32 },
    }

    /// The answer of `trace_step`.
    pub enum StepPieces {
        Current { pieces: Vec<TracePiece> },
        /// The record changed under the trace the request names. Open it again.
        Stale,
    }

    pub enum PieceRef {
        Sent { piece: u32 },
        Reply { piece: u32 },
    }

    /// The answer of `trace_piece`.
    pub enum PieceText {
        /// At most 1 MiB of the text. `bytes` is the whole piece; `cut` says the text stops early.
        Current { text: String, bytes: u64, cut: bool },
        Stale,
    }
}

// ---------------------------------------------------------------------------------------------
// API request arguments and errors
// ---------------------------------------------------------------------------------------------

records! {
    pub struct ScanReq { pub cwd: AbsPath }
    pub struct ReadReq { pub cwd: AbsPath, pub path: AbsPath }
    pub struct RenderReq { pub cwd: AbsPath, pub agent: AgentId }
    /// `fingerprint` is the one of the snapshot the client shows.
    pub struct LoadTextsReq { pub cwd: AbsPath, pub agent: AgentId, pub file: AbsPath, pub fingerprint: Fingerprint }
    pub struct SimReq { pub cwd: AbsPath, pub path: AbsPath }
    pub struct SearchReq { pub cwd: AbsPath, pub query: String, pub regex: bool, pub case_sensitive: bool }
    pub struct WatchReq { pub cwd: AbsPath, pub since: Fingerprint }
    pub struct PlanReq { pub cwd: AbsPath, pub edit: Edit }
    pub struct ApplyReq { pub cwd: AbsPath, pub edit: Edit, pub digest: String }
    pub struct HistoryReq { pub cwd: Option<AbsPath>, pub before: Option<ChangeId>, pub limit: u32 }
    pub struct ChangeReq { pub id: ChangeId }
    pub struct SampleReq { pub cwd: AbsPath, pub agent: AgentId, pub event: String }
    pub struct RunReq { pub cwd: AbsPath, pub hook: HookId, pub input: serde_json::Value }
}

variants! {
    /// Why a write is refused as read-only.
    pub enum ReadOnlyReason {
        /// The host runs with `--read-only`.
        Host,
        /// The file's own policy.
        File { why: ReadOnlyWhy },
        /// An agent rule says the target is read-only, such as a Gemini state file.
        Rule { rule: RuleRef },
    }

    pub enum HookRunFailure {
        EmptyInvocation,
        ProgramNotFound { program: String },
        StartFailed { program: String, os: String },
    }

    pub enum ApiError {
        NotFound,
        /// An edit whose base no longer matches the disk.
        Conflict { path: AbsPath, expected: Base, found: Base, current: Option<String> },
        OutsideBoundary { path: AbsPath },
        ReadOnly { why: ReadOnlyReason },
        InvalidEdit { field: String, why: RuleRef },
        Forbidden { why: RuleRef },
        /// A hook that was allowed to run could not be started.
        HookRun { why: HookRunFailure },
        /// The request is not one the API defines.
        BadRequest,
        /// A record in the history store is damaged.
        HistoryCorrupt,
        /// Text from the operating system, and nothing else.
        Io { detail: String },
        Unauthorized,
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::NotFound => write!(f, "not found"),
            ApiError::Conflict { path, .. } => write!(f, "conflict on {path}"),
            ApiError::OutsideBoundary { path } => write!(f, "outside the boundary: {path}"),
            ApiError::ReadOnly { why } => write!(f, "read only: {why:?}"),
            ApiError::HookRun { why } => write!(f, "hook run: {why:?}"),
            ApiError::BadRequest => write!(f, "bad request"),
            ApiError::HistoryCorrupt => write!(f, "history is corrupt"),
            ApiError::InvalidEdit { field, why } => write!(f, "invalid edit, {field}: {}", why.0),
            ApiError::Forbidden { why } => write!(f, "forbidden: {}", why.0),
            ApiError::Io { detail } => write!(f, "io: {detail}"),
            ApiError::Unauthorized => write!(f, "unauthorized"),
        }
    }
}

impl std::error::Error for ApiError {}
