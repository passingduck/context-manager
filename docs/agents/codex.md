# OpenAI Codex CLI: file-based context sources (spec for context-manager)

Target: `codex-cli 0.165.0`, macOS arm64, as of 2026-11-19.

## How this was verified

- **Source.** github.com/openai/codex at tag `rust-v0.165.0` (commit `360319d8dee6`), the exact installed version. Source paths below are relative to `https://github.com/openai/codex/blob/rust-v0.165.0/`. A local clone is at `/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/scratchpad/src/codex-0.165.0`.
- **Docs.** `developers.openai.com/codex/*` now 308-redirects to `learn.chatgpt.com/docs/*`. The URLs below are the redirect targets.
- **Probe 1 (prompt).** `/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/scratchpad/probe_codex.py` runs the installed binary's `codex debug prompt-input` against a throwaway `HOME` + `CODEX_HOME` + git repo, and asserts on the model-visible prompt. 44 of 44 checks pass. The results are in `probe_results.txt` next to it. Cited below as `probe:<letter>`.
- **Probe 2 (hooks).** `/private/tmp/claude-501/-Users-sungjin-context-manger/dddccb01-94a9-4428-bd2b-2f11d7667ddd/scratchpad/probe_hooks.py` starts `codex app-server` over stdio and calls JSON-RPC `hooks/list`. Cited as `probe:hooks`.
- **Local install.** These checks read the local install only. Listings and key names of `~/.codex` were taken read-only. No values from `config.toml` are reproduced here. Cited as `local`.

Status labels:

- **CONFIRMED** means source at the tag, a probe run against the installed binary, or both. The evidence column says which.
- **CONFIRMED (docs)** means it is documented but was not checked in source.
- **UNVERIFIED** means it was not checked. The same item appears again under "Open questions".

Terms used below:

- `CODEX_HOME` is the `$CODEX_HOME` environment variable, or `~/.codex` when it is unset.
- **Project root** is the nearest ancestor of cwd that contains a root marker. The default marker is `.git`.
- **Trusted** means the user config has `[projects."<path>"] trust_level = "trusted"` for the directory, the project root or the git repo root.
- **Unknown trust** means there is no entry. **Untrusted** means `trust_level = "untrusted"`. These three states gate different things, and section 6 has the matrix.

---

## 1. AGENTS.md discovery

### 1.1 Rules table

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `$CODEX_HOME/AGENTS.override.md`, else `$CODEX_HOME/AGENTS.md` | global (user) | Read at session start. The read repeats at every turn boundary, because the user-instructions provider is re-queried on each `refresh` (see 1.4). | Codex takes the **first non-empty file after trim**. An override that is empty or whitespace-only falls through to `AGENTS.md`. Content is `trim()`med. **There is no size limit**: a 40 KB file was not truncated. `project_doc_fallback_filenames` does **not** apply here. A directory with that name is skipped. | `codex-rs/codex-home/src/instructions/mod.rs` `CodexHomeUserInstructionsProvider::load_from_codex_home`; probe:C, probe:N, probe:F | CONFIRMED |
| `<dir>/AGENTS.override.md` \| `<dir>/AGENTS.md` \| `<dir>/<fallback>` for each `<dir>` from the project root down to cwd, inclusive | project | Session start. The result is cached for the session and re-discovered only when the turn's environment selection (cwd/env) or the active project's trust level changes. | **Exactly one file per directory**: the first candidate that exists *as a file*, in the order override, AGENTS.md, then fallbacks in config order. The choice is made on existence, not content, so an **empty `AGENTS.override.md` shadows `AGENTS.md`** in that directory and contributes nothing. Empty or whitespace-only files are skipped and consume no budget. Symlinked files are followed. A directory named `AGENTS.md` is ignored. | `codex-rs/core/src/agents_md.rs` `agents_md_paths`, `candidate_filenames`, `read_agents_md`; `codex-rs/core/src/agents_md_manager.rs` `AgentsMdManager::refresh`; probe:A, probe:B, probe:F, probe:O | CONFIRMED |
| files below cwd (`<cwd>/**/AGENTS.md`) | project | **Never auto-loaded.** | Some model base prompts (`codex-rs/protocol/src/prompts/base_instructions/default.md`, `core/gpt_5_1_prompt.md`, `core/gpt_5_2_prompt.md`) tell the model to look for nested AGENTS.md itself, using tools, when it edits files there. The base prompt for the current default model on this machine has no such section. | `agents_md.rs` (the walk stops at cwd); probe:A (`DEEP` absent); `default.md` lines 17-27 | CONFIRMED |
| `CLAUDE.md` and other names | project | Not loaded unless listed in `project_doc_fallback_filenames`. | — | probe:R | CONFIRMED |
| plugin-root `AGENTS.md` (e.g. `plugins/cache/<mkt>/<plugin>/<ver>/AGENTS.md`) | plugin | Not loaded. | The plugin manifest has no instructions field, and AGENTS.md discovery walks only project root → cwd. | `codex-rs/core-plugins/src/manifest.rs` (`PluginManifestPaths` = skills, mcp_servers, apps, hooks) | CONFIRMED |

### 1.2 Project root detection for AGENTS.md

- Codex walks up from cwd to the nearest ancestor `A` such that `A/<marker>` exists for any marker in `project_root_markers`. Any file type counts. **For AGENTS.md, a `.git` directory without `HEAD` still counts** (contrast with config layers in 6.3). Source: `codex-rs/file-system/src/find_up.rs` `find_nearest_ancestor_with_markers`; probe:Q. **CONFIRMED**
- The default markers are `[".git"]`. Source: `codex-rs/config/src/project_root_markers.rs` `DEFAULT_PROJECT_ROOT_MARKERS`. **CONFIRMED**
- `project_root_markers = []` disables parent traversal, so only cwd is checked. When no marker is found, only cwd is checked. Source: `agents_md.rs` module doc and `agents_md_paths`; probe:G. **CONFIRMED**
- `project_root_markers` is read from the merged config **excluding project `.codex/config.toml` layers**. A repo cannot move its own root. A non-array value logs a warning and falls back to the defaults. Source: `agents_md.rs` `agents_md_paths`; probe:H (a project-layer marker is ignored and a user-layer marker is honored). **CONFIRMED**
- Codex never walks above the project root. Source: `agents_md.rs`. **CONFIRMED**

### 1.3 Size limit (`project_doc_max_bytes`)

- The default is `32 * 1024` = 32768 bytes. Source: `codex-rs/config/src/config_toml.rs` `DEFAULT_PROJECT_DOC_MAX_BYTES`. **CONFIRMED**
- The budget covers **project files only**, summed across all selected environments. The global file is not counted. Source: `agents_md.rs` `load_project_instructions`; probe:E. **CONFIRMED**
- At the limit, the file that crosses the budget is **truncated byte-wise to the remaining budget**. Lossy UTF-8 decoding means a split multi-byte character becomes U+FFFD. Codex logs a warning, and every later file is **dropped**. Source: `read_agents_md`; probe:E. **CONFIRMED**
- `project_doc_max_bytes = 0` loads no project docs. **CONFIRMED** (source)
- The docs say Codex "stops adding files once the combined size reaches the limit". That wording does not mention the mid-file truncation, and the source is authoritative here.

### 1.4 Concatenation and the model-visible form

- Order: global (user) instructions, then host "thread" instructions (not file-based), then the project files root→cwd.
- Separator: `"\n\n--- project-doc ---\n\n"` on the first switch from user/internal text to project text, and `"\n\n"` between project files. Project file content is **not** trimmed, so trailing newlines survive.
- The text reaches the model as one **user-role** message: `# AGENTS.md instructions for <cwd>\n\n<INSTRUCTIONS>\n<text>\n</INSTRUCTIONS>`. With more than one environment, each group gets a `for \`<env_id>\` with root <cwd>` label and the outer cwd is omitted.
- Source: `agents_md.rs` `legacy_text`, `environment_labeled_text`; `codex-rs/core/src/context/user_instructions.rs`; probe:A. **CONFIRMED**

### 1.5 Trust, includes, flags, reload

- **Trust.** Project AGENTS.md is skipped **only when the active project is explicitly `untrusted`**. Unknown trust still loads it. The active project is the `[projects]` entry for cwd, else the one for the git repo root. The global file is always loaded. Source: `agents_md.rs` `load_project_instructions` (`config.active_project.is_untrusted()`); `config_toml.rs` `get_active_project`; probe:A, probe:D. **CONFIRMED**
- **Import or include syntax.** None. File bytes are passed verbatim, so `@file.md` stays literal text. Source: `read_agents_md`; probe:R. **CONFIRMED**
- **Feature flags.** 0.165.0 has no AGENTS.md-specific flag. No `child_agents_md` or hierarchical flag exists, by grep of the source and `codex features list`. **CONFIRMED**
- **Reload.** The docs say the chain is built "once per run". In source, `AgentsMdManager::refresh` runs at session start and at each turn preparation (`codex-rs/core/src/session/mod.rs`, around line 4042). It re-reads the global file each time but re-discovers project files only when the environment or trust changes. **CONFIRMED** (source). Whether a mid-session change to the global file is re-sent to the model in an existing conversation is **UNVERIFIED**.
- **Subagents.** They inherit the parent's global and thread instructions and run their own project discovery for their cwd. Source: `agents_md_manager.rs` `inherited_instructions`. **CONFIRMED** (source)

---

## 2. Hooks

### 2.1 Where hooks are defined

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `/etc/codex/hooks.json` (Windows `%ProgramData%\OpenAI\Codex\hooks.json`) | system (treated as managed) | thread start | `is_managed=true`. Always trusted and enabled. | `codex-rs/hooks/src/engine/discovery.rs` `discover_handlers`, `load_hooks_json`, `hook_metadata_for_config_layer_source`; `codex-rs/config/src/state.rs` `config_folder` | CONFIRMED (source) |
| `[hooks]` table in `/etc/codex/config.toml` | system (managed) | thread start | same as above | `discovery.rs` `load_toml_hooks_from_layer` | CONFIRMED (source) |
| `$CODEX_HOME/hooks.json` | user | thread start | It must be trusted per hook before it runs (2.5). | `discovery.rs`; probe:hooks | CONFIRMED |
| `[hooks]` table in `$CODEX_HOME/config.toml`, and in `$CODEX_HOME/<profile>.config.toml` when `--profile` is used | user | thread start | Codex warns when one layer has both `hooks.json` and `[hooks]`, and both sets run. `hooks.json` is read once per folder, so the base and profile layers share one. | `discovery.rs` (`visited_json_hook_folders`); probe:hooks (warning text observed) | CONFIRMED |
| `<dir>/.codex/hooks.json` and the `[hooks]` table in `<dir>/.codex/config.toml`, for each `<dir>` from the config project root down to cwd | project | thread start | **Loaded only from trusted project layers.** Unknown or untrusted layers are disabled and skipped. In a **linked git worktree**, hook declarations come from the matching `.codex/` of the **root checkout**, while the rest of the config still comes from the worktree. | `discovery.rs` (iterates `layers_low_to_high`, which filters disabled layers); `codex-rs/config/src/loader/mod.rs` `merge_root_checkout_project_hooks`, `root_checkout_hooks_folder_for_dir`; probe:hooks (trusted listed, unknown absent) | CONFIRMED |
| `-c hooks....` CLI overrides | session flags | thread start | source path shown as `<session-flags>/config.toml`; not managed | `discovery.rs` `config_toml_source_path` | CONFIRMED (source) |
| MDM (`com.openai.codex` managed prefs, key `config_toml_base64`), cloud bundle, legacy `/etc/codex/managed_config.toml` | managed | thread start | `[hooks]` inside these configs. Managed. | `discovery.rs` `hook_metadata_for_config_layer_source`; `codex-rs/config/src/loader/macos.rs` | CONFIRMED (source) |
| `[hooks]` in `/etc/codex/requirements.toml` (+ `managed_dir`, `windows_managed_dir`) | managed requirements | thread start | Required: a load failure is a hard error. `managed_dir` only names where admin scripts live. It is **not** a `hooks.json` search path. | `codex-rs/config/src/hook_config.rs` `ManagedHooksRequirementsToml`; `discovery.rs` `append_managed_requirement_handlers` | CONFIRMED (source) |
| plugin hooks: `<plugin_root>/hooks/hooks.json` by default, or the manifest `hooks` field (a string, a string array, an inline object or an array of objects), relative to the plugin root | plugin | thread start, for **enabled, installed** plugins only | The commands get env `PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT`, `PLUGIN_DATA` and `CLAUDE_PLUGIN_DATA`. The literal `${PLUGIN_ROOT}`-style placeholders in `command` are substituted. Trust uses the declaration hash **plus a plugin content hash**, so a plugin update that changes scripts needs re-review. | `codex-rs/core-plugins/src/loader.rs` `DEFAULT_HOOKS_CONFIG_FILE = "hooks/hooks.json"`; `core-plugins/src/manifest.rs` `resolve_manifest_hooks`; `discovery.rs` `append_plugin_hook_sources`; local (`omo` manifest lists 19 hook JSON files) | CONFIRMED |

### 2.2 Ordering and policy

- **Discovery order.** Managed requirements hooks come first. Then each enabled config layer from lowest to highest precedence, and inside a layer `hooks.json` before `[hooks]`. Plugin hooks come last. Every matching handler is selected, with no override or replace between layers. Source: `discovery.rs` `discover_handlers` (`display_order`). **CONFIRMED** (source)
- `allow_managed_hooks_only = true` is honored **only in `requirements.toml`**. It drops user, project, session and plugin hooks and keeps managed ones. Source: `codex-rs/docs/config.md`; `discovery.rs` `HookDiscoveryPolicy`. **CONFIRMED**

### 2.3 Feature flag

- `[features] hooks` is stage Stable with default `true`. `codex_hooks` is a legacy alias. Source: `codex-rs/features/src/lib.rs` (`Feature::CodexHooks`, key `"hooks"`); `features/src/legacy.rs`; local `codex features list` shows `hooks stable true`. **CONFIRMED**
- `[features] plugin_hooks` is stage **Removed** and is ignored. This machine's config still sets it. Source: `features/src/lib.rs`; local. **CONFIRMED**

### 2.4 Schema

`hooks.json` (`HooksFile`, `deny_unknown_fields` at the top level only):

```json
{
  "description": "optional",
  "hooks": {
    "<EventName>": [
      { "matcher": "<optional string>", "hooks": [ <handler>, ... ] }
    ]
  }
}
```

The `[hooks]` table in config.toml (`HooksToml`) holds the same event keys flattened under `hooks`, plus `hooks.state`.

- The 14 event keys (PascalCase in both JSON and TOML) are `PreModelRequest`, `PostModelRequest`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`, `Stop` and `Interrupt`. Source: `codex-rs/protocol/src/hooks.rs` `HookEventsToml`. **CONFIRMED**
- Unknown event keys are silently ignored, because `HookEventsToml` has no `deny_unknown_fields`. **CONFIRMED** (source: attribute absent). The tool should flag them itself.

Handler (`HookHandlerConfig`, internally tagged by `type`). The field names are the same in JSON and TOML.

| `type` | fields | notes | status |
|---|---|---|---|
| `"command"` | `command` (string, required, non-blank); `commandWindows` (alias `command_windows`); `timeout` (seconds); `async` (bool, default false); `statusMessage`; `additionalContextLimit` (approx tokens) | Runs with the session cwd. `timeout` defaults to 600 with a minimum of 1. For `SessionEnd` and `Interrupt` it defaults to 1 and is clamped to 1..3. `async` is forced off for `SessionEnd`. `additionalContextLimit` is honored only for PreToolUse, PostToolUse, SessionStart, UserPromptSubmit and SubagentStart. Its default is 2500, `0` disables spilling, and on other events it is ignored with a warning. | CONFIRMED (source `discovery.rs` `append_matcher_groups`, `normalize_command_hook`; probe:hooks shows timeout 600 and `statusMessage` parsed from TOML) |
| `"mcp_tool"` | `server`, `tool` (both non-blank), `input` (object, TOML-representable, default `{}`), `timeout`, `statusMessage` | Rejected for `SessionEnd`. | CONFIRMED (source) |
| `"prompt"`, `"agent"` | none | Parsed, then **skipped** with the warning "...hooks are not supported yet". | CONFIRMED (probe:hooks warning) |

Matcher semantics. Source: `codex-rs/hooks/src/engine/matcher.rs` `HookMatcher::new`; `hooks/src/events/common.rs` `matcher_pattern_for_event`; probe:hooks (the `Stop` matcher was dropped to `None`).

- A missing matcher, `""` or `"*"` matches everything.
- A string made only of `[A-Za-z0-9_|]` is an **exact** match against `|`-separated alternatives, so `Bash` does not match `BashOutput`.
- Anything else is a Rust `regex`, unanchored (`is_match`).
- An invalid regex makes Codex skip that group with a warning.
- **CONFIRMED**

What each event matches against:

| event | matcher input | status |
|---|---|---|
| PreToolUse, PostToolUse, PermissionRequest | tool name plus tool aliases. The docs say `apply_patch` also matches `Edit` and `Write`. | CONFIRMED (source `matcher_inputs`); alias list CONFIRMED (docs) |
| SessionStart | source: `startup`, `resume`, `clear`, `compact`, `fork` | CONFIRMED (`hooks/src/events/session_start.rs` `SessionStartSource::as_str`) |
| SubagentStart, SubagentStop | agent type | CONFIRMED (source) |
| SessionEnd | reason, always `"other"` | CONFIRMED (`session_end.rs` `SESSION_END_REASON`) |
| PreCompact, PostCompact | trigger: `manual` or `auto` | CONFIRMED (`codex-rs/core/src/hook_runtime.rs`, `CompactionTrigger::Manual => "manual"`, `Auto => "auto"`) |
| PreModelRequest, PostModelRequest, UserPromptSubmit, Stop, Interrupt | the matcher is **ignored** (treated as absent) | CONFIRMED (source; probe:hooks) |

How hook output reaches the model:

- `additionalContext` from SessionStart, UserPromptSubmit, SubagentStart and PostToolUse becomes **developer-role** context. For SessionStart and UserPromptSubmit, plain stdout also becomes developer context.
- PreToolUse `additionalContext` adds context without blocking.
- Output over the limit spills to `<temp_dir>/hook_outputs/<session_id>/<uuid>.txt`, and the model gets a head-and-tail preview.
- Source: https://learn.chatgpt.com/docs/hooks; `codex-rs/hooks/src/output_spill.rs`. **CONFIRMED (docs)**. Per-event input and output JSON schemas are in `codex-rs/hooks/schema/generated/*.schema.json`.

### 2.5 Trust and enablement (critical for an editor)

- **Key.** Each handler's key is `<source_key>:<event_snake>:<group_index>:<handler_index>`.
  - `<event_snake>` is one of `pre_tool_use`, `permission_request`, `post_tool_use`, `pre_compact`, `post_compact`, `pre_model_request`, `post_model_request`, `session_start`, `session_end`, `user_prompt_submit`, `subagent_start`, `subagent_stop`, `stop` or `interrupt`.
  - For files, `<source_key>` is the **absolute path** of the `hooks.json` or `config.toml`. For plugins it is `<plugin>@<marketplace>:<relative hook file>`. A local example is `omo@sisyphuslabs:hooks/post-tool-use-checking-comments.json:post_tool_use:0:0`.
  - Source: `codex-rs/hooks/src/lib.rs` `hook_key`, `hook_event_key_label`; `hooks/src/declarations.rs` `plugin_hook_key_source`; probe:hooks; local key names in `[hooks.state]`. **CONFIRMED**
- **State.** State lives in `[hooks.state."<key>"]` with `enabled` (bool), `trusted_hash` and `trusted_content_hash` (plugins). It is read **only from user config layers (base and profile) and session flags**, never from project or managed layers. Fields merge field by field, and the later layer wins. Source: `codex-rs/hooks/src/config_rules.rs` `hook_states_from_stack`; `config/src/hook_config.rs` `HookStateToml`. **CONFIRMED**
- **Run rule.** A non-managed hook runs only if `enabled != false` **and** `trusted_hash == current_hash`. Otherwise its status is `untrusted` when there is no hash, or `modified` when the hash differs, and it is skipped.
  - `current_hash` is `sha256:` plus hex over the key-sorted JSON of `{event_name, matcher, hooks:[normalized handler]}`. The normalized handler has the timeout filled in and `commandWindows` dropped.
  - For plugin hooks it is `<declaration_hash>/<plugin_content_hash>`.
  - Managed hooks are always `managed`, trusted and enabled.
  - Source: `discovery.rs` `hook_hash`, `hook_trust_status`, `hook_enabled`; `codex-rs/config/src/fingerprint.rs` `version_for_toml`; probe:hooks (every new hook was `trustStatus: untrusted`). **CONFIRMED**
- **What breaks trust.** Any edit to the event, matcher, command, timeout, async, statusMessage or additionalContextLimit changes the hash and stops the hook until the user re-trusts it. So does a change to group or handler index or to the source file path: reordering, moving the file or relocating `CODEX_HOME` all change the key. **CONFIRMED** (follows from the key and hash definition)
- **Bypass.** `--dangerously-bypass-hook-trust` runs enabled hooks without persisted trust for one invocation. Source: `codex --help`. **CONFIRMED**
- **Review UI.** `/hooks` in the TUI, or Settings > Hooks in the desktop app, reviews and trusts hooks, and Codex then writes `hooks.state` into the user `config.toml`. **CONFIRMED (docs)**; the write path is `codex-rs/core/src/config/edit.rs`.
- **Reload.** The hook engine is built at thread start. It is rebuilt when Codex itself applies config changes through app-server (`codex-rs/core/src/session/mod.rs` `refresh_hooks`, `thread_manager.rs` `refresh_hook_runtimes`, `app-server/src/request_processors/config_processor.rs` `reload_user_config`). No file watcher for hook files was found. **UNVERIFIED** whether an external edit is picked up before the next thread start.

### 2.6 Minimal valid examples (both parsed and listed by the 0.165.0 binary in probe:hooks)

`$CODEX_HOME/hooks.json` or `<repo>/.codex/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo pre", "timeout": 30 } ] }
    ]
  }
}
```

`config.toml` inline:

```toml
[[hooks.SessionStart]]
matcher = "startup|resume"
[[hooks.SessionStart.hooks]]
type = "command"
command = "echo start"
statusMessage = "starting"
```

### 2.7 Legacy `notify`

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `notify = ["prog", "arg", ...]` in user, system or managed `config.toml` | user/system | Fires at the end of each agent turn. | It is an argv array, not a shell string. Codex appends one JSON argument: `{"type":"agent-turn-complete","thread-id","turn-id","cwd","client"?,"input-messages":[...],"last-assistant-message"}`. It is fire-and-forget, with stdio sent to null. **Not model-visible.** **Ignored in project `.codex/config.toml`**, which is on the denylist. The hooks crate marks it for removal ("TODO: Remove this hook ... when legacy `notify` support is removed"). | `codex-rs/hooks/src/legacy_notify.rs`; `config/src/loader/mod.rs` `PROJECT_LOCAL_CONFIG_DENYLIST`; local (key present) | CONFIRMED (source) |

---

## 3. Memories

### 3.1 Enablement and config

- The feature is `[features] memories`, stage Stable, **default `false`**. `memory_tool` is a legacy alias. It is off on this machine (`codex features list`). Source: `codex-rs/features/src/lib.rs`, `features/src/legacy.rs`; local. **CONFIRMED**
- Keys in the `[memories]` table, with their defaults. Source: `codex-rs/config/src/types.rs` `MemoriesToml`, `MemoriesConfig::default`, `DEFAULT_MEMORIES_*`. **CONFIRMED**
  - `use_memories = true` injects the summary.
  - `generate_memories = true`. When false, new threads are stored with `memory_mode = "disabled"`.
  - `disable_on_external_context = false`. Its alias is `no_memories_if_mcp_or_web_search`.
  - `dedicated_tools = false` exposes the `memories.list`, `memories.read`, `memories.search` and `memories.add_ad_hoc_note` tools.
  - Clamped numeric limits: `max_raw_memories_for_consolidation = 256` (1..4096), `max_unused_days = 30` (0..365), `max_rollout_age_days = 10` (0..90), `max_rollouts_per_startup = 2` (1..128), `min_rollout_idle_hours = 6` (1..48) and `min_rate_limit_remaining_percent = 25` (0..100).
  - Model overrides: `extract_model` and `consolidation_model`.
  - Pipeline selection: `version` (`"v1"` or `"v2"`) and `dual_write`.
- **Version selection.** When memories are enabled, `version` is unset, `dual_write != false` and `$CODEX_HOME/memories/` does **not** exist, Codex uses **v2** (`memories_v2/`). Otherwise it uses v1 (`memories/`). This machine already has `~/.codex/memories/`, so enabling memories here selects v1. Source: `codex-rs/core/src/config/mod.rs` (around line 3648); `codex-rs/protocol/src/memory_version.rs` `directory_name`. **CONFIRMED** (source)

### 3.2 On-disk layout

The root is `$CODEX_HOME/memories/` for v1 and `$CODEX_HOME/memories_v2/` for v2.

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `<root>/memory_summary.md` | global (user, all projects) | Session start (initial context build), **only if** `features.memories` and `memories.use_memories` are both on | It is the **only memory file injected automatically**. Content is trimmed and truncated to **2,500 tokens**, then wrapped in a template that tells the model about the folder. It is sent as a **developer**-role message. In v2 it is split into fragments of at most 8,900 bytes. Empty or missing means no injection. | `codex-rs/ext/memories/src/prompts.rs` `build_memory_tool_developer_instructions`; `ext/memories/src/extension.rs` `contribute_thread_context`, `MemoriesExtensionConfig::from_config`; `ext/memories/src/lib.rs` `MEMORY_TOOL_DEVELOPER_INSTRUCTIONS_SUMMARY_TOKEN_LIMIT = 2_500`; templates `ext/memories/templates/memories/read_path.md`, `read_path_v2.md` | CONFIRMED (source); not observable with probe 1 (see 3.4) |
| `<root>/MEMORY.md` | global | On demand: the model searches it with shell tools or the memory tools | It is the "searchable registry", written by the consolidation agent. It is never auto-injected. | `read_path.md`; probe:K | CONFIRMED |
| `<root>/skills/<name>/SKILL.md` (+ `scripts/`, `examples/`, `templates/`) | global | On demand, when the model opens it | Memory-generated skills. These are **not** in the skills catalog, because no skill root points here. | `read_path.md`; `codex-rs/ext/skills/src/host_roots.rs` (no memories root) | CONFIRMED |
| `<root>/rollout_summaries/*.md` | global | On demand | One file per selected rollout. Codex syncs and prunes them in Phase 2. | `codex-rs/memories/README.md`; `memories/write/src/lib.rs` `ROLLOUT_SUMMARIES_SUBDIR` | CONFIRMED |
| `<root>/raw_memories.md` | global | Consolidation input only | Rebuilt from SQLite on every Phase 2 run. | `memories/write/src/storage.rs`; README | CONFIRMED |
| `<root>/extensions/ad_hoc/instructions.md`, `<root>/extensions/ad_hoc/notes/*.md` | global | Consolidation input. The v2 prompt tells the model to append notes here on "remember/forget". | `instructions.md` is seeded by Codex (create-new only). `notes/` is the **intended edit surface**: "Do not edit generated memory files directly; consolidation applies these notes." Notes older than the extension retention window are pruned. | `memories/write/src/extensions/ad_hoc.rs`, `extensions/prune.rs`; `read_path_v2.md` | CONFIRMED |
| `<root>/phase2_workspace_diff.md`, `<root>/.git/` | global | Internal | Transient diff and git baseline that Phase 2 uses. Do not touch. | README; `memories/write/src/workspace.rs` | CONFIRMED |

### 3.3 SQLite

- `memories_1.sqlite` (v1) and `memories_v2_1.sqlite` (v2) live in `sqlite_home`. That defaults to `CODEX_HOME` and is overridden by the `CODEX_SQLITE_HOME` env var or the `sqlite_home` config key.
- Tables: `stage1_outputs` (`thread_id`, `raw_memory`, `rollout_summary`, `rollout_slug`, `usage_count`, `last_usage`, `selected_for_phase2`, ...), `jobs` (leases and watermarks), `consolidation_progress` and `_sqlx_migrations`. The per-thread `memory_mode` is in `state_5.sqlite`.
- Source: `codex-rs/state/src/sqlite.rs` (`MEMORIES_DB_FILENAME`, `MEMORIES_V2_DB`); `state/src/lib.rs` `SQLITE_HOME_ENV`; local `.schema` of `~/.codex/memories_1.sqlite` (tables only, no rows read). **CONFIRMED**
- **Plain files vs SQLite.** Per-thread extractions (Phase 1 output) live **only in SQLite**. Everything the model can see is a plain file under `<root>/`. `raw_memories.md` and `rollout_summaries/` are regenerated from SQLite on each consolidation, so hand edits to them are overwritten.

### 3.4 Generation

- Generation runs in the background when a root, non-ephemeral, non-subagent session starts with the feature on and the state DB available.
- Phase 1 extracts per thread into SQLite. Phase 2 syncs files and runs a consolidation subagent with no approvals, no network and local write access. That subagent writes `MEMORY.md`, `memory_summary.md` and `skills/`.
- Source: `codex-rs/memories/README.md`. **CONFIRMED** (source docs)
- **Probe limitation.** `codex debug prompt-input` builds its own extension registry without the memories extension (`codex-rs/cli/src/main.rs` `run_debug_prompt_input_command`, versus `app-server/src/extensions.rs`, which installs `codex_memories_extension`). Memory injection therefore cannot be observed with probe 1. It is confirmed from source only.

---

## 4. Rules (exec policy)

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `<config_folder>/rules/*.rules` for each **enabled** config layer: `/etc/codex/rules/`, `$CODEX_HOME/rules/`, and `<dir>/.codex/rules/` for trusted project layers | system/user/project | session start (policy build) | Only **regular files** directly in `rules/` with the extension `.rules` are loaded: no recursion, and symlinks are not followed (`DirEntry::file_type().is_file()`). They load in **lexicographic** order within a layer, layers from low to high. The format is Starlark `prefix_rule(pattern=[...], decision="allow"\|"prompt"\|"forbidden", justification=, match=, not_match=)`. A parse error keeps only the `requirements.toml` policy and reports the error. **Project rules need trust.** On this machine, `~/.codex/rules/ouroboros.md` is **ignored** (wrong extension). | `codex-rs/core/src/exec_policy.rs` `load_exec_policy`, `collect_policy_files` (`RULES_DIR_NAME`, `RULE_EXTENSION`); https://learn.chatgpt.com/docs/agent-configuration/rules; local listing | CONFIRMED |
| `[rules]` in `requirements.toml` | managed | session start | Overlaid on top of the file policy (`merge_overlay`). | `exec_policy.rs` | CONFIRMED (source) |
| `$CODEX_HOME/rules/default.rules` | user | — | **Codex appends to it** when the user picks "always allow" for a command prefix or network rule. | `exec_policy.rs` `default_policy_path`, `blocking_append_allow_prefix_rule`, `append_network_rule_and_update`; rules docs | CONFIRMED |

Is it model-visible? **Partly.** The rule file text is never sent. The **allow prefixes** derived from the rules are rendered into the `<permissions instructions>` developer message as a `- <prefix>` list. The list is capped at 100 prefixes and 5,000 characters (`...[Some commands were truncated]`), and appears only when the approval mode's template includes the approved-prefix section. `prompt` and `forbidden` rules affect tool approval only. Source: `codex-rs/protocol/src/models.rs` `format_allow_prefixes`, `MAX_RENDERED_PREFIXES`, `MAX_ALLOW_PREFIX_TEXT_BYTES`; `codex-rs/prompts/src/permissions_instructions.rs` `approved_command_prefixes_text`; `core/src/context/world_state/permissions.rs`. **CONFIRMED** (source). Label rules in the UI as "command policy (not instructions); allow-prefixes are summarized to the model".

---

## 5. Other context that reaches the model

### 5.1 Skills

Skill roots, in the order `resolve_skill_roots` builds them, then de-duplicated by path:

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `<dir>/.codex/skills/` for every project `.codex/` layer, **including disabled (unknown or untrusted) layers** | repo | session start | Trust does **not** gate this: the skills were listed for unknown and untrusted projects. | `codex-rs/ext/skills/src/host_roots.rs` `roots_from_layer_stack` (uses `all_layers_high_to_low`); probe:J | CONFIRMED |
| `$CODEX_HOME/skills/` | user | session start | Deprecated location, kept for backward compatibility. Hidden subdirectories are skipped, so `.system` is not scanned twice. | `host_roots.rs` comment "Deprecated user skills location"; probe:J | CONFIRMED |
| `$HOME/.agents/skills/` | user | session start | — | `host_roots.rs` (`AGENTS_DIR_NAME`); probe:J | CONFIRMED |
| `$CODEX_HOME/skills/.system/` | system (bundled) | session start | **Codex writes this directory itself** on startup (embedded system skills): imagegen, openai-docs, skill-creator, skill-installer (+ review-agent locally). `[skills.bundled] enabled = false` turns them off. | `codex-rs/skills/src/lib.rs` `system_cache_root_dir`, install of embedded skills; probe (a fresh `CODEX_HOME` got `skills/.system` created) | CONFIRMED |
| `/etc/codex/skills/` | admin | session start | — | `host_roots.rs` (`ConfigLayerSource::System`) | CONFIRMED (source) |
| plugin skill roots: the manifest `skills` paths, default `<plugin_root>/skills/` | plugin | session start, for enabled plugins | Skill names are namespaced `<plugin>:<skill>`. | `codex-rs/core-plugins/src/loader.rs` `DEFAULT_SKILLS_DIR_NAME`; `ext/skills/src/loader/namespace.rs` | CONFIRMED (source) |
| `<dir>/.agents/skills/` for each `<dir>` from the skills project root down to cwd | repo | session start | **Not gated by trust.** Directories below cwd are not scanned. Project root here means the nearest ancestor with any `project_root_markers` entry, from non-project layers. | `host_roots.rs` `repo_agents_skill_roots`, `find_project_root`; probe:J | CONFIRMED |

Loader rules. Source: `codex-rs/ext/skills/src/loader/mod.rs` (`MAX_SCAN_DEPTH = 6`, `MAX_SKILLS_DIRS_PER_ROOT = 2000`, `MAX_NAME_LEN = 64`), `loader/discovery.rs` (`MAX_SKILLS_ENTRIES_PER_ROOT = 20_000`), `loader/host.rs`, `codex-rs/skills/src/parser.rs`; probe:J (a nested skill at depth 4 was found, and one without a description was rejected). **CONFIRMED**

- Codex scans each root recursively for files named `SKILL.md`, up to depth 6.
- Hidden directories under a root are skipped.
- Directory symlinks are followed for user, repo and admin roots and ignored for system roots.
- The frontmatter must be YAML between `---` lines. `description` is **required** and the skill is rejected without it. `name` is optional and defaults to a derived name, with a 64-character maximum. `metadata.short-description` is optional.
- An optional `agents/openai.yaml` next to SKILL.md holds `interface`, `policy.allow_implicit_invocation` and `dependencies`.

In context versus loaded on use. Source: `codex-rs/ext/skills/src/render.rs` (`DEFAULT_SKILL_METADATA_CHAR_BUDGET`, `SKILL_METADATA_CONTEXT_WINDOW_PERCENT`, `MAX_CONFIGURED_SKILL_METADATA_TOKEN_BUDGET`, `MAX_CATALOG_SKILL_DESCRIPTION_CHARS`); `ext/skills/src/catalog_prompt.rs`; probe:J; https://learn.chatgpt.com/docs/build-skills. **CONFIRMED**

- **Always in context:** a `<skills_instructions>` **developer** message with a skill-roots alias table and one line per skill, `- <name>: <description> (file: rN/<rel>/SKILL.md)`.
- The budget is 2% of the model context window. It falls back to 8,000 characters when the window is unknown, and `skills.max_context_tokens` raises it to at most 10,000 tokens.
- Each description is capped at 1,024 characters. Over budget, descriptions shrink first, then skills are dropped with a warning.
- **On use:** the model reads the full SKILL.md body. The body is never in the catalog.

Config:

- `[[skills.config]] { path = "<abs SKILL.md>" | name = "<name>", enabled = bool }`. The last matching entry wins. **It is honored only from user config and session flags**, not from project layers. Source: `codex-rs/config/src/skills_config.rs` `skill_config_rules_from_stack`. **CONFIRMED**
- `skills.include_instructions = false` drops the whole block. **CONFIRMED** (source `core/src/config/mod.rs`)
- Codex writes `[[skills.config]]` itself when the user toggles a skill (`ConfigEdit::SetSkillConfig`). **CONFIRMED**

Live reload: app-server watches skill roots and emits a skills-changed notification (`codex-rs/app-server/src/skills_watcher.rs`). Whether the in-session catalog shown to the model is re-rendered is **UNVERIFIED**.

### 5.2 Custom prompts (`~/.codex/prompts/*.md`)

- The docs page says "Custom prompts are deprecated. Use skills". Source: https://learn.chatgpt.com/docs/custom-prompts. **CONFIRMED (docs)**
- **The 0.165.0 source has no loader for `$CODEX_HOME/prompts`**: a grep of all `.rs`, `.ts` and `.md` files found no reference. The matches named `CustomPrompt` are a TUI text-input widget and the compaction `compact_prompt`. Plugin `commands/` directories are migrated into skills (`codex-rs/core-plugins/src/command_migration/`). This machine has no `~/.codex/prompts`. **CONFIRMED** that the 0.165.0 source has no loader. A third-party blog claims removal in v0.117.0, which is **UNVERIFIED**.
- Recommendation: show this location as "legacy, not loaded by codex ≥ 0.165.0".

### 5.3 Subagents (custom agent roles)

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `<config_folder>/agents/**/*.toml` for each **enabled** layer: `/etc/codex/agents/`, `$CODEX_HOME/agents/`, and trusted `<dir>/.codex/agents/` | system/user/project | session start | Recursive, `.toml` only, sorted. Each file is one role with `name`, `description` (**required** once fields merge) and `nickname_candidates`, plus **any `config.toml` key** flattened in (`developer_instructions`, `model`, `model_reasoning_effort`, `features.*`, `skills.*`, ...). Unknown keys are an error and the role is skipped with a startup warning. The `name` field wins over the filename. **Project agents need trust.** | `codex-rs/agent-roles/src/loader.rs` `load_agent_roles`, `discover_agent_roles_in_dir`; `agent-roles/src/discovery.rs`; `agent-roles/src/agent_role_config.rs` `RawAgentRoleFileToml` (`deny_unknown_fields`, `#[serde(flatten)] config: ConfigToml`) | CONFIRMED (source) |
| `[agents.<name>]` with `config_file = "<path>"`, `description`, `nickname_candidates` in any config layer | user/project/system | session start | A file named in `config_file` is not discovered a second time. A higher-precedence layer overrides a lower one for the same role name, and missing fields are filled from the lower layer. A duplicate name inside one layer produces a warning, and the later definition is dropped. On this machine, `config.toml` has `[agents.<name>] config_file` entries for the 14 files in `~/.codex/agents/`. | `loader.rs` `merge_missing_role_fields`; local | CONFIRMED (source + local keys) |

Model visibility:

- The role `description` goes into the spawn-agent tool guidance. It is **not** in the prompt messages (probe:M), and is sent as tool spec text according to the `AgentRoleConfig` doc comment.
- `developer_instructions` reach only the spawned subagent, as its developer message. They are not in the parent prompt (probe:M).
- **CONFIRMED** (source + probe:M). That the description appears specifically in the spawn tool description is **UNVERIFIED**, because tool specs are not shown by `prompt-input`.
- Built-in roles are `default`, `worker` and `explorer`. A custom role with the same name overrides the built-in. **CONFIRMED (docs)** https://learn.chatgpt.com/docs/agent-configuration/subagents.
- Files that are not `.toml` in `agents/` are ignored, such as this machine's `.lazycodex-agent-models.json`. **CONFIRMED** (source)

### 5.4 Plugins

| path pattern | scope | when loaded | limits and notes | evidence | status |
|---|---|---|---|---|---|
| `$CODEX_HOME/plugins/cache/<marketplace>/<plugin>/<version>/` | plugin | session start | The **active version** is `local` if that directory exists, else the highest version by `compare_plugin_versions`. Older version directories are inert. The manifest is the first of `.codex-plugin/plugin.json`, `.claude-plugin/plugin.json` or `.cursor-plugin/plugin.json`. It contributes `skills` (default `./skills`), `hooks` (default `hooks/hooks.json`), `mcpServers` (path or inline) and `apps`. Plugin `commands/` are migrated to skills. Per-plugin data lives in `$CODEX_HOME/plugins/data/<plugin>-<marketplace>/`. | `codex-rs/core-plugin-common/src/installed.rs` `active_plugin_version`, `DEFAULT_PLUGIN_VERSION = "local"`, `PLUGINS_CACHE_DIR`; `codex-rs/exec-server-protocol/src/protocol.rs` `DISCOVERABLE_PLUGIN_MANIFEST_PATHS`; `core-plugins/src/store.rs`; local (`omo` has 4 versions cached) | CONFIRMED (source + local) |
| `[plugins."<plugin>@<marketplace>"] enabled = bool`, `[plugins._default]`; `[marketplaces.<name>]` | user config | session start | An explicit setting overrides `_default`, but **cannot enable an installation its source disabled**. Feature `plugins` is stable and on by default. `recommended_plugins` adds a plugin catalog to context and is off here. | `codex-rs/config/src/types.rs` `PluginsConfigToml`, `PluginConfig`; `codex features list` | CONFIRMED (source); full effective-enable resolution (source and account enablement) UNVERIFIED |

### 5.5 Config keys that inject instructions

| key (any config layer unless noted) | effect | evidence | status |
|---|---|---|---|
| `model_instructions_file = "<path>"` | **Replaces** the built-in base (system) instructions with the file's contents. It must be non-empty. Precedence: a runtime override, then this file, then the `instructions` key. Also valid inside legacy `[profiles.<n>]`. | `codex-rs/core/src/config/mod.rs` (around line 4180, `try_read_non_empty_file`); `config/src/profile_toml.rs`; probe:L | CONFIRMED |
| `instructions = "<text>"` | Base instructions text, used only when there is no override or file. | `config/src/config_toml.rs`; `core/src/config/mod.rs` | CONFIRMED (source) |
| `developer_instructions = "<text>"` | Added as a **developer**-role message. Allowed in trusted project `.codex/config.toml`. | `config_toml.rs` doc comment; probe:I | CONFIRMED |
| `experimental_instructions_file` | **Does not exist in 0.165.0.** It is not a field, so it has no effect. In default (non-strict) mode the key is silently ignored: exit 0, no stderr, and the prompt is unchanged. | grep of the whole repo at the tag: no match; probe run in `probe_runs/oq` | CONFIRMED absent and ignored |
| `compact_prompt`, `experimental_compact_prompt_file` | Prompt used during history compaction only. | `config_toml.rs` | CONFIRMED (source) |
| `include_permissions_instructions`, `include_apps_instructions`, `include_collaboration_mode_instructions`, `include_environment_context`, `include_environment_context_time` (all default true), `skills.include_instructions` | Toggle the built-in developer blocks. | `core/src/config/mod.rs` | CONFIRMED (source) |
| `additional_developer_instructions` in `requirements.toml` | Admin-managed developer instructions. | `codex-rs/config/src/config_requirements.rs`; `core/src/context/world_state/managed_developer_instructions.rs` | CONFIRMED (source) |
| `[auto_review] policy` and similar guardian keys | Instructions for the auto-review (guardian) model, not the main agent. | `config_toml.rs` | CONFIRMED (source) |
| hook `additionalContext` | See 2.4. | — | CONFIRMED (docs) |

---

## 6. Relocation, layering and trust

### 6.1 CODEX_HOME

- `$CODEX_HOME` is used when it is set and non-empty. It must exist and be a directory, otherwise startup fails. The path is canonicalized. When it is unset, Codex uses `$HOME/.codex`, which is not canonicalized. Source: `codex-rs/utils/home-dir/src/lib.rs` `find_codex_home_from_env`. **CONFIRMED**
- A project `.codex/` that resolves to the same directory as `CODEX_HOME` is skipped as a project layer. Source: `config/src/loader/mod.rs` `discover_project_layers`. **CONFIRMED**

### 6.2 Config layers, lowest to highest precedence

Source: `codex-rs/config/src/loader/mod.rs` `load_config_layers_state` and its doc comment; `config/src/config_layer_source.rs` `precedence`.

| layer | path | precedence | notes | status |
|---|---|---|---|---|
| packaged defaults | embedded `config/defaults.toml` | -10 | — | CONFIRMED |
| MDM | macOS managed prefs domain `com.openai.codex`, key `config_toml_base64` | 0 | — | CONFIRMED (`loader/macos.rs`) |
| system | `/etc/codex/config.toml` (Windows `%ProgramData%\OpenAI\Codex\config.toml`) | 10 | Absent on this machine. | CONFIRMED |
| enterprise cloud bundle | remote | 15 | — | CONFIRMED |
| user | `$CODEX_HOME/config.toml` | 20 | — | CONFIRMED |
| profile v2 | `$CODEX_HOME/<name>.config.toml`, chosen with `--profile <name>` / `-p` | 21 | Holds only overrides. `--profile X` **errors** if `config.toml` still has legacy `profile = "X"` or `[profiles.X]`. Legacy profiles are `[profiles.<n>]` tables chosen by the top-level `profile = "<n>"`. | CONFIRMED (`core/src/config/mod.rs` `CONFIG_PROFILE_V2_SUFFIX`; loader) |
| project | `<dir>/.codex/config.toml` for each `<dir>` from the config project root down to cwd (deeper wins) | 25 | Needs trust (6.3). Some keys are stripped, listed after this table. | CONFIRMED |
| session flags | `-c key=value`, `--enable/--disable`, UI selections | 30 | — | CONFIRMED |
| legacy managed | `/etc/codex/managed_config.toml`, then MDM legacy | 40 / 50 | — | CONFIRMED |

- **Keys stripped from project layers** (`PROJECT_LOCAL_CONFIG_DENYLIST`): `openai_base_url`, `chatgpt_base_url`, `apps_mcp_product_sku`, `responses_api_metadata`, `model_provider`, `model_providers`, `notify`, `profile`, `profiles`, `experimental_realtime_webrtc_call_base_url`, `experimental_realtime_ws_base_url` and `otel`. Codex prints a startup warning when it strips them. **CONFIRMED**
- **Requirements** (admin constraints, not config) are read from `/etc/codex/requirements.toml`, the cloud bundle, legacy `/etc/codex/managed_config.toml` and MDM `requirements_toml_base64`. They can set `[hooks]`, `allow_managed_hooks_only`, `[rules]`, `[features]` pins, `additional_developer_instructions`, the allowed approval and sandbox modes, and more. Source: `config/src/loader/mod.rs` doc comment; `config/src/config_requirements.rs`. **CONFIRMED**
- Relative paths inside a project layer resolve against that layer's `.codex/` folder. Source: `load_project_layers` → `resolve_relative_paths_in_config_toml(..., dot_codex_folder)`. **CONFIRMED**
- In the user layer they resolve against `CODEX_HOME`. A probe set `model_instructions_file = "rel.md"` in `$CODEX_HOME/config.toml` with cwd elsewhere, and the file was loaded. **CONFIRMED** (probe run in `probe_runs/oq`)

### 6.3 Project root and trust for config layers (differs from AGENTS.md)

- **Root.** The config project root is the nearest ancestor with a marker. For the marker `.git`, a **`.git` directory counts only if it contains `HEAD`**. A `.git` file, as in worktrees and submodules, counts as is. Source: `config/src/loader/mod.rs` `discover_project_root`; probe:Q. **CONFIRMED**
- **Trust lookup** for each `.codex/` directory checks, in order: the directory itself, then the project root, then the git repo root. For linked worktrees the repo root is the main checkout. Each lookup tries the canonical and the literal path, and Windows lowercases. The first `[projects."<key>"] trust_level` found wins. Source: `ProjectTrustContext::decision_for_dir`, `normalized_project_trust_keys`. **CONFIRMED**

Trust matrix:

| source | trusted | unknown (no entry) | `untrusted` | evidence |
|---|---|---|---|---|
| project `.codex/config.toml` | loaded | **disabled** (parsed, then ignored with "add ... as a trusted project") | disabled | `disabled_reason_for_decision`; probe:I |
| project `.codex/hooks.json` and `[hooks]` | loaded (each hook still needs per-hook trust) | skipped | skipped | probe:hooks |
| project `.codex/rules/*.rules` | loaded | skipped | skipped | `load_exec_policy` |
| project `.codex/agents/*.toml` | loaded | skipped | skipped | `load_agent_roles` |
| project `AGENTS.md` chain | loaded | **loaded** | **skipped** | probe:A, probe:D |
| project `.codex/skills/`, `.agents/skills/` | loaded | **loaded** | **loaded** | probe:J |

- **Who writes `[projects]`.** Codex writes `[projects."<path>"] trust_level = "trusted"|"untrusted"` itself, through the TUI or app trust prompt (`ConfigEdit::SetProjectTrustLevel` in `codex-rs/core/src/config/edit.rs`). **CONFIRMED**
- **Edge case.** A `.codex/config.toml` that fails to parse is a hard error only when the layer is trusted. An untrusted one is recorded as an empty disabled layer. **CONFIRMED** (source)

---

## 7. Notes for a tool that edits these files

### 7.1 How Codex writes `config.toml`

- **Codex rewrites `config.toml` itself.** It uses `toml_edit::DocumentMut`, which preserves formatting and comments. It writes a temp file in the same directory and then renames it (`NamedTempFile::persist`), and it follows symlinks.
- There is **no file lock**: Codex reads, modifies and writes. A concurrent external write can be lost, on either side.
- The temp-file write leaves the file at mode **0600**, which matches this machine.
- Source: `codex-rs/core/src/config/edit.rs` `apply_blocking_to_resolved_file`, `ConfigEdit`; `codex-rs/utils/path-utils/src/lib.rs` `write_atomically`. **CONFIRMED**
- Codex writes these keys: `model`, `service_tier`, `notice.*`, `mcp_servers`, `[[skills.config]]`, `tui.*`, `[projects."<path>"].trust_level`, `[hooks.state."<key>"]` (trust and enable), `[plugins.*]`, `[marketplaces.*]` (`config/src/plugin_edit.rs`, `marketplace_edit.rs`), `features.*` (through `codex features enable/disable`), and arbitrary paths via `SetPath`.

### 7.2 Per file type

| file | format | Codex rewrites it? | tool policy |
|---|---|---|---|
| `$CODEX_HOME/AGENTS.md`, `AGENTS.override.md` | Markdown (UTF-8; lossy decode) | No. The only writer is the user-triggered external-agent migration (`codex-rs/external-agent-migration/src/service.rs`). | Editable. Warn that an empty override still shadows **at project level**, but falls through at global level. |
| project `AGENTS.md`, `AGENTS.override.md`, fallbacks | Markdown | No (migration only) | Editable. Show a byte counter against the remaining `project_doc_max_bytes` budget along the chain, and the truncation point. |
| `$CODEX_HOME/config.toml` | TOML | **Yes**, often | Edit with a format-preserving TOML library. Re-read immediately before writing, write atomically by temp file and rename in the same directory, and keep 0600. **Never write `hooks.state.*.trusted_hash` yourself**, because that silently trusts code. |
| `$CODEX_HOME/<name>.config.toml` (profile v2) | TOML | Possibly, when Codex runs with that profile. Assumed to be the same writer as `config.toml`, which is UNVERIFIED. | Same as above. |
| `<repo>/.codex/config.toml` | TOML | Only through migration | Editable. Mark denylisted keys as "ignored here", and say the file is inert until the project is trusted. |
| `hooks.json` (user, project) | JSON | No (migration only) | Editable. **Any edit, reorder or move makes affected hooks `modified` or `untrusted`, and they stop running until re-trusted via `/hooks`.** Show this before saving. |
| `config.toml` `[hooks]` | TOML | Codex writes only the `state` sub-table | Same caveat as `hooks.json`. Preserve `hooks.state`. |
| `/etc/codex/*`, MDM, cloud | TOML or JSON | No (admin) | **Read-only.** Managed by policy. |
| `$CODEX_HOME/rules/*.rules` | Starlark | **Codex appends to `default.rules`** | Editable, but race-prone. Write atomically and re-read first. Label the files "command policy". |
| `$CODEX_HOME/agents/*.toml`, `.codex/agents/*.toml` | TOML (strict: unknown keys reject the role) | No (migration only; third-party installers may write them) | Editable. Validate against the `ConfigToml` keys plus `name`, `description` and `nickname_candidates`. |
| `$CODEX_HOME/skills/<name>/SKILL.md`, `~/.agents/skills/**`, `.agents/skills/**`, `.codex/skills/**` | Markdown + YAML frontmatter | No | Editable. Require `description` and enforce `name` ≤ 64 characters. |
| `$CODEX_HOME/skills/.system/**` | Markdown | **Yes**, reinstalled at startup | **Read-only.** Edits are overwritten. |
| `$CODEX_HOME/plugins/cache/**`, `plugins/data/**` | mixed | **Yes** (installer, updater) | **Read-only.** Edits change the plugin content hash, which invalidates hook trust, and updates overwrite them. Show only the active version. |
| `$CODEX_HOME/memories*/MEMORY.md`, `memory_summary.md`, `raw_memories.md`, `rollout_summaries/**`, `skills/**`, `.git/` | Markdown | **Yes** (Phase 2 consolidation) | **Read-only.** Treat as generated. Offer "add note" instead, writing a new Markdown file in `<root>/extensions/ad_hoc/notes/`. |
| `memories_1.sqlite`, `memories_v2_1.sqlite`, `state_5.sqlite` and the other `*.sqlite` files | SQLite (WAL) | **Yes** | **Never open for write.** Show at most the schema and a status line. |
| `$CODEX_HOME/vendor_imports/**` | Markdown/JSON | Assumed to be the desktop app or skill-installer, which is UNVERIFIED | **Read-only.** Not referenced by the 0.165.0 CLI source, so it is not a context source for the CLI. |
| `$CODEX_HOME/prompts/*.md` | Markdown | No | Show as legacy and not loaded (5.2). |

### 7.3 Use Codex as the oracle

Ground truth is available without re-implementing the rules:

1. `CODEX_HOME=<home> codex debug prompt-input`, run from the target cwd, prints the exact model-visible messages as JSON. This covers AGENTS.md, the skills catalog, developer_instructions and base instructions. It runs offline and needs no login (probe 1). It does **not** include memories (3.4), tool specs or hook output.
2. `codex app-server` over stdio, with `initialize`, `initialized` and then `hooks/list {cwds:[...]}`, returns every hook with `key`, `source`, `sourcePath`, `trustStatus`, `enabled`, `currentHash` and `warnings` (probe 2). Use it for trust status instead of recomputing hashes.
3. `codex features list` gives each feature's stage and effective value.

---

## Open questions (UNVERIFIED items and what would settle them)

1. **Global AGENTS.md mid-session.** It is re-read each turn. Is a changed file re-sent into an existing conversation? To settle: run a TUI or `exec` session, edit `$CODEX_HOME/AGENTS.md` between turns, and inspect the rollout or request log.
2. **Hook reload after an external edit** to `hooks.json` or `[hooks]` mid-session. To settle: edit while a session runs, then check `hooks/list` on the same app-server and whether the hook fires.
3. **Skills catalog refresh after `skills/changed`**: is the model-visible catalog re-rendered mid-session? To settle: add a skill during a session and inspect the next request.
4. **Memory injection observed end to end.** It is confirmed from source only, because `debug prompt-input` lacks the memories extension. To settle: enable memories in a throwaway `CODEX_HOME`, seed `memories/memory_summary.md`, run `codex exec` with request logging, and look for the developer message.
5. **The exact spawn-tool location of an agent role `description`.** To settle: capture the tool specs from a real request, or read `codex-rs/core/src/agent/role.rs` and the spawn tool builder.
6. **Effective plugin enablement** (source enablement, account and remote plugins combined with `[plugins]` and `_default`). To settle: read `codex-rs/core-plugins/src/manager.rs` or call app-server `plugin/list`.
7. **`compare_plugin_versions` ordering** for non-semver directory names. To settle: read `codex-rs/core-plugin-common/src/installed.rs` in full.
8. **Which component writes `$CODEX_HOME/vendor_imports/`**, and whether the desktop app reads it as a skill root. To settle: read the desktop app or skill-installer, or watch the directory while installing a curated skill.
9. **When custom prompts were removed.** One blog says v0.117.0. To settle: run `git log -S "prompts" -- codex-rs/core/src/custom_prompts.rs` in a full clone.
10. **Profile v2 writes**: does Codex write UI changes into `<name>.config.toml` when a profile is active, or into `config.toml`? To settle: read `core/src/config/edit.rs` callers for `user_config_path`.

Settled during this pass:

- The user-layer relative path base is `CODEX_HOME` (probe).
- An unknown key such as `experimental_instructions_file` is silently ignored (probe).
- The compaction trigger `auto` was found in `core/src/hook_runtime.rs`.
