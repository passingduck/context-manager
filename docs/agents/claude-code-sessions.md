# Claude Code: session transcripts and the request record (spec for context-manager)

Target: Claude Code **2.1.329**, macOS arm64 (`~/.local/share/claude/versions/2.1.329`). Researched 2026-11-20 and 2026-11-21. The local transcripts that the counts come from were written by versions 2.1.278 to 2.1.329. Purpose: enough detail to build a reader that shows what the model received in a past session, request by request, and to say which parts of that view are exact.

The short version: from 2.1.302 on, Claude Code writes every main-thread and subagent request into the transcript in a deduplicated form. A reader can rebuild each recorded request body byte for byte and check it against a SHA-256 digest that the same record carries. Section 4 gives the algorithm, and section 17 lists what is exact, what is approximate and what is unknown.

## How to read this spec

**Status.**

| Status | Meaning |
| :- | :- |
| CONFIRMED | The oracle showed it: a run of the real 2.1.329 binary against a loopback endpoint, where the request the endpoint received was compared with the request rebuilt from the transcript. |
| OBSERVED | Counted over the local transcripts. The count is given. |
| INFERRED | Read from the binary's code, or reasoned from other rules, and not exercised. |
| UNKNOWN | Not settled. Repeated under [Open questions](#open-questions). |

**Evidence keys.**

| Key | Source |
| :- | :- |
| BIN | Code in the 2.1.329 binary. Quoted strings are exact. Minified names (`UXt`, `Gnn`) are build-specific and only help you find the code again with `tools/oracle/claude/bgrep.py`. |
| RUN:&lt;scenario&gt; | `python3 tools/oracle/claude/session_oracle.py <scenario>` (section 18). |
| COUNT | `python3 tools/oracle/claude/session_survey.py <config>/projects` over the local transcripts: 16 main transcripts and 139 subagent transcripts, 9,923 request records. The bracketed tag names the section of the survey output, such as COUNT[4 chains]. Two of the main transcripts belonged to live sessions and grew while the counts ran, so totals differ by a few between tables. |
| SHAPE | `python3 tools/oracle/claude/session_shape.py <transcript>`, per file. |
| DOC | Public documentation, cited by URL. |

**Placeholders.** `<config>` is `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`. `<key>` is a project folder name (section 1.2). `<session>` is a session id, `<agent>` a subagent id, `<request-N>` a request record's `id`, `<hash-X>` a SHA-256 in hex. Every example comes from a sandbox run of the oracle with prompts written for it. Ids and sandbox paths in the examples are replaced with placeholders.

**Privacy.** Transcripts hold the operator's conversations, file contents and account identifiers. The tools in `tools/oracle/claude/` that read a real transcript print counts and categorical values only. Section 15 lists the fields that identify an account, an organisation, a host or an email address.

## 1. Files and folders

### 1.1 Layout

| Path | What it holds | evidence | status |
| :- | :- | :- | :- |
| `<config>/projects/<key>/<session>.jsonl` | The main transcript. One JSON object per line, each with a `type`. | RUN:all, COUNT[1 files] | CONFIRMED |
| `<config>/projects/<key>/<session>/subagents/agent-<agent>.jsonl` | One subagent's transcript (section 9). The file name carries the subagent's `agentId`. | RUN:subagent, COUNT[8 subagents] (139 files) | CONFIRMED |
| `<config>/projects/<key>/<session>/subagents/agent-<agent>.meta.json` | The subagent's type, description, parent tool-use id and settings (section 9). | RUN:subagent, COUNT[1 files] (139 of 139 transcripts have one) | CONFIRMED |
| `<config>/projects/<key>/<session>/subagents/<dir>/agent-<agent>.jsonl` | A subagent grouped under a subdirectory. | BIN (the subagent path builder takes an optional subdirectory); none seen locally | INFERRED |
| `<config>/projects/<key>/<session>/tool-results/<id>.txt` | The full output of a tool result that was too large to send (section 10). Binary results are saved with their own extension. | RUN:bigbash; COUNT[1 files]: 34 `.txt`, 10 `.pdf` | CONFIRMED |
| `<config>/projects/<key>/<session>/mcp-tasks/` | MCP task metadata. | BIN `w(n,e,"mcp-tasks")`; none seen locally | INFERRED |
| `<config>/projects/<key>/memory/` | Auto memory. Its `<key>` comes from a different root (section 1.2). See `docs/agents/claude-code.md` section 2. | | |

### 1.2 Which folder a session goes to

| Rule | evidence | status |
| :- | :- | :- |
| The folder is `<config>/projects/` + the key of the session's **original working directory**. The key function is the one in `docs/agents/claude-code.md` section 2.3: every UTF-16 code unit outside `[A-Za-z0-9]` becomes `-`, and a name over 200 characters is cut and given a hash suffix. `tools/oracle/claude/project_key.py` implements it. | BIN `function cP(e){return xzn()??ZH(e)}`, `function C5e(){let t=ns();if(t!==null)return DS(t);return Ivt()??DS(Ae())}`; COUNT: 16 of 16 main transcripts sit in the folder that `project_key.py` computes from the `cwd` of their first conversation record | CONFIRMED |
| The original working directory is the physical path: symbolic links are resolved, so a session started in `/var/folders/...` on macOS lands in `-private-var-folders-...`. The path is normalized to NFC before the key is taken. | RUN:all (every sandbox folder starts with `-private-var-`); BIN `originalCwd:e.originalCwd?.normalize("NFC")` | CONFIRMED |
| The transcript key is **not** the canonical repository root that auto memory uses. A session started in a subdirectory, or in a git worktree nested in the checkout, gets a folder of its own. The main checkout's folder holds neither. | RUN:projectkey (sessions in `proj/sub` and in `proj/.claude/worktrees/oracle-wt` produced two folders, one per working directory) | CONFIRMED |
| `CLAUDE_CODE_PROJECT_DIR_NAME` replaces the key for every session, but only when `CLAUDE_CONFIG_DIR` is also set, the value matches `^[A-Za-z0-9_-]{1,64}$`, and it is not a Windows device name such as `con` or `lpt1`. | BIN `var T=/^[A-Za-z0-9_-]{1,64}$/,C=/^(?:con\|prn\|aux\|nul\|com[0-9]\|lpt[0-9])$/i` and `xzn=_(()=>MCt()\|\|Vwe()?FZi(p()):void 0,I)` | INFERRED |
| A resumed session keeps writing to the file it started in, even when it is resumed from another directory. The session's project folder is carried with the session. | BIN `function Tre(){let e=h();return e?e.sessionProjectDir:n().identity.projectDir()}` and `function Kte(){return Tre()??C5e()}` | INFERRED |
| The `cwd` field of records can change inside one file, so the folder is named after the first value, not the last. | COUNT: the `cwd` changes within 7 of 16 main transcripts | OBSERVED |

**For a reader given a project directory `P`** (for example with `--path`): the sessions started exactly in `P` are in `<config>/projects/<key of realpath(P), NFC>/`. Sessions started in a subdirectory of `P` or in one of its worktrees are in other folders. To list every session of a project, read the first `cwd` of each transcript under `<config>/projects/*/` and match it against `P`, its subdirectories and the paths that `git worktree list` prints. The folder name alone cannot be reversed: the key maps `/`, `.`, `_` and `-` to the same character, and a long name ends in a hash. Two directories can share one folder, so always confirm with `cwd`. (INFERRED from the rules above.)

## 2. Records

### 2.1 Record types

2.1.329 lists its record types in one table (BIN `NLl`). The table assigns each type to one of three write paths: a conversation record that is deduplicated by `uuid`, a metadata record that is always written, and a record routed by `agentId` to a subagent file.

| Group | Types | Local count (COUNT[2 record types]) |
| :- | :- | :- |
| Conversation | `user`, `assistant`, `attachment`, `system`, `progress` | 13,061 / 22,240 / 16,172 / 333 / 0 |
| Request record (section 3) | `api-request-shape`, `api-request-blob`, `api-request`, `api-request-resumed` | 245 / 7,307 / 9,923 / 0 |
| Other routed by agent | `content-replacement`, `fork-context-ref`, `observer-ref`, `data-flow-records`, `data-flow-incomplete` | 0 |
| Session metadata | `summary`, `custom-title`, `ai-title`, `last-prompt`, `tag`, `agent-name`, `agent-color`, `agent-setting`, `relocated`, `continued-in`, `ended-by-model`, `derived-from`, `mode`, `permission-mode`, `memory-mode`, `isolation-latch`, `atis-latch`, `worktree-state`, `cost-state`, `tools-used`, `queue-operation`, `pr-link`, `frame-link`, `file-history-snapshot`, `file-history-delta`, `attribution-snapshot`, `history-suppression`, `resume-head-vouch`, `dev-mods`, `artifact-comment-monitor`, `artifact-autoreact-ledger`, `session-owner`, `bridge-session`, plus `repl-mode` and `hybrid-organization` | 17 of these types seen; the most frequent are `last-prompt` 1,475, `atis-latch` 1,463, `mode` 1,395, `ai-title` 1,384, `session-owner` 1,178 |

Status: the list is INFERRED from BIN. The counts are OBSERVED.

Metadata records such as `ai-title`, `custom-title`, `last-prompt`, `tag`, `agent-name` and `session-owner` are written again and again during a session (1,384 `ai-title` records in 16 files). **The last one in the file wins.** (BIN: the metadata writer re-stamps them; OBSERVED.)

### 2.2 Conversation record fields a reader needs

| Field | On | Meaning |
| :- | :- | :- |
| `uuid`, `parentUuid` | conversation records | The record and its parent in the conversation tree. |
| `sessionId` | every record | The session. A subagent's records carry the parent's session id. |
| `timestamp`, `version` | most records | ISO 8601 time, and the Claude Code version that wrote the record. |
| `cwd`, `gitBranch`, `entrypoint`, `userType` | `user`, `assistant`, `attachment` | Working directory, branch, `cli` or `sdk-cli`, and `external` in the sandbox. |
| `isSidechain`, `agentId` | subagent records | `true` and the subagent id. |
| `message` | `user`, `assistant` | An API message. An `assistant` record holds **one** content block (section 6). |
| `requestRef`, `apiBlockIndex` | `assistant` | The `id` of the request this block answers, and the block's index in the reply. |
| `toolUseResult` | `user` with a tool result | The tool's structured result, which is not what the model received. |
| `isMeta`, `isCompactSummary`, `isVisibleInTranscriptOnly` | `user` | A record the CLI added (a skill body, a reminder), and the compaction summary (section 8). |
| `attachment`, `rendered`, `renderedRole` | `attachment` | Section 11. |

Status: OBSERVED (RUN:all shows the keys, COUNT[2 user record flags] counts the flags).

## 3. The request record

### 3.1 Three record types

A recorded request is split into three kinds of record, each written once and referred to by hash.

**`api-request-shape`**: the system blocks and the tool definitions.

```json
{"type":"api-request-shape","sessionId":"<session>","shapeHash":"<hash-S>","timestamp":"2026-11-20T15:26:19.190Z","version":"2.1.329",
 "shape":{"system":[{"type":"text","text":"You are a Claude agent, ..."},{"type":"text","text":"\nYou are an agent working ..."}],
          "tools":["... 12 tool definitions, each with name, description and input_schema ..."]}}
```

**`api-request-blob`**: one message of a request whose content does not appear in the conversation records (section 4.2).

```json
{"type":"api-request-blob","sessionId":"<session>","hash":"<hash-B>",
 "message":{"role":"system","content":[{"type":"text","text":"# Environment\nYou have been invoked in the following environment: ..."}]}}
```

**`api-request`**: one request. From RUN:bigbash, the second request of the session:

```json
{"type":"api-request","sessionId":"<session>","id":"<request-2>","timestamp":"2026-11-20T15:26:19.657Z","version":"2.1.329",
 "querySource":"sdk","shapeHash":"<hash-S>",
 "params":{"model":"claude-opus-5-5","betas":["claude-code-20250219","interleaved-thinking-2025-05-14","context-management-2025-06-27","per-turn-control-2026-07-01","mid-conversation-tool-changes-2026-07-01","thinking-token-count-2026-05-13","advanced-tool-use-2025-11-20","effort-2025-11-24"],
           "max_tokens":128000,"thinking":{"type":"adaptive","display":"omitted"},
           "context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},"output_config":{"effort":"medium"}},
 "base":"<request-1>","keep":2,"tail":["<hash-C>","<hash-D>"],
 "cacheControl":[{"path":["system",0],"value":{"type":"ephemeral"}},{"path":["system",1],"value":{"type":"ephemeral"}},
                 {"path":["messages",3,"content",0],"value":{"type":"ephemeral"}}],
 "requestDigest":"<hash-R>"}
```

| Field of `api-request` | Meaning | evidence | status |
| :- | :- | :- | :- |
| `id` | A random UUID for this request. Reply records point at it with `requestRef`. | RUN:all | CONFIRMED |
| `agentId` | Present on a subagent's request (section 9). | RUN:subagent; COUNT[3.2 optional request fields]: 7,661 records, all in subagent files | CONFIRMED |
| `querySource` | Which part of Claude Code made the request (section 7). | RUN:all (`sdk`), COUNT[3.4 query sources] | CONFIRMED |
| `shapeHash` | The `api-request-shape` that holds this request's system blocks and tools. | RUN:all | CONFIRMED |
| `params` | Every top-level body key except `system`, `tools`, `messages` and the unrecorded keys of 3.3. Observed keys: `model`, `betas`, `max_tokens`, `thinking`, `context_management`, `output_config`. | RUN:all | CONFIRMED |
| `base`, `keep`, `tail` | The messages: the first `keep` messages of request `base`, then the messages whose hashes `tail` lists. `base` is absent when `keep` is 0. Section 4.3. | RUN:all, COUNT[4 chains] | CONFIRMED |
| `cacheControl` | Where the body carried `cache_control`, and its value (section 5). | RUN:all | CONFIRMED |
| `requestDigest` | SHA-256 of the whole recorded body (3.3). | RUN:all, COUNT[4.3 requestDigest] | CONFIRMED |
| `withheld` | Keys from `CLAUDE_CODE_EXTRA_BODY` that were sent but not recorded, because their value equals the injected value. | BIN `Request not recorded for replay: it would write CLAUDE_CODE_EXTRA_BODY's ...`; none seen locally | INFERRED |
| `cut`, `cutRequestDigest` | Present instead of `requestDigest` when text was cut out of the record (3.6). | BIN; none seen locally | INFERRED |

### 3.2 Canonical JSON and the three hashes

Every hash is SHA-256, in lowercase hex, of the UTF-8 bytes of a **canonical JSON** text:

- No whitespace. Object keys sorted by UTF-16 code units (JavaScript's default sort). Keys whose value is `undefined` are left out.
- Strings and numbers as JavaScript's `JSON.stringify` writes them: only `"`, `\`, control characters and lone surrogates are escaped; `/`, non-ASCII and U+2028 are not.
- With the option `omitCacheControl`, every `cache_control` key is left out at any depth.

`session_record.canon` is a Python port. The binary streams the text into the hasher and replaces base64 strings of 16 KiB or more with a hash of their bytes while it does so. That is a memory optimization: the resulting digest is the same.

| Hash | Input | evidence | status |
| :- | :- | :- | :- |
| Message hash (`tail` entries, blob `hash`) | The message, with string `content` first turned into `[{"type":"text","text":<string>}]`, serialized with `omitCacheControl`. Every key of the message counts, not only `role` and `content`. | BIN `function UXt(e){return typeof e.content==="string"?{...e,content:[{type:"text",text:e.content}]}:e}` and `function ear(e){let o=T9("sha256"),{hasCacheControl:r}=uBe([o],UXt(e),{omitCacheControl:!0})...}`; COUNT[4.3 message hashes]: 7,307 of 7,307 blob hashes match | CONFIRMED |
| `shapeHash` | `{"system": <system blocks>, "tools": <tools>}` with `omitCacheControl`. The system blocks exclude the billing block (3.4). | BIN `Y7e`; SHAPE (every shape hash matches) | CONFIRMED |
| `requestDigest` | Section 3.3. | | |

**Why a naive hash misses some blobs.** A blob stores the message as it was sent, minus `cache_control` (BIN `function Gnn(n){... filter(([t,s])=>t!=="cache_control"&&s!==void 0) ...}`). It keeps string content as a string, but the hash is taken over the block form. And a message can carry keys besides `role` and `content`: a `system`-role message can carry `output_config`. A hash over the stored message as is, or over `{role, content}` only, therefore misses those blobs. Locally, 579 blobs have string content and 219 carry `output_config` (OBSERVED). The lead's eight mismatches are of these two kinds.

### 3.3 `requestDigest`

`requestDigest` is the SHA-256 of the canonical JSON of the request body as recorded:

- every key of the body except `metadata`, `stream`, `thread`, `diagnostics`, `safeguards` and `data_flow`, and except keys listed in `withheld`;
- `system` without the billing block;
- `messages` with string content turned into one text block, as for the message hash;
- **with** every `cache_control` in place.

So the digest checks the cache marks as well as the content. BIN `var eCe=new Set(["metadata","stream","thread","diagnostics","safeguards","data_flow"])`. RUN:all: the digest of the request the endpoint received, with these keys removed, equals the recorded digest for every request. COUNT[4.3 requestDigest]: the rebuilt body matches the digest for every request record on this machine (table in 4.4). CONFIRMED.

### 3.4 What was sent but is not in the record

| Item | What happens | evidence | status |
| :- | :- | :- | :- |
| The billing block | The request's first system block is a text block that starts with `x-anthropic-billing-header:` (`cc_version=...; cc_entrypoint=...`). It is filtered out of the shape, the digest and the `cacheControl` paths. Paths such as `["system",0]` index the system list **after** this block is removed. | BIN `function Mnl(e){let t=e.text;return typeof t==="string"&&t.startsWith(hFt)}` with `hFt` = `"x-anthropic-billing-header:"`; RUN:all (the endpoint received one more system block than the shape holds) | CONFIRMED |
| `betas` | Recorded in `params`, but sent as the `anthropic-beta` HTTP header, not in the body. | RUN:all (the oracle adds the header's list to the received body before it compares digests) | CONFIRMED |
| `metadata` (holds `user_id`), `stream` and the other keys of 3.3 | Sent, not recorded. | BIN `eCe`; RUN:all | CONFIRMED |
| HTTP headers other than `anthropic-beta` | Not recorded. | RUN:all | CONFIRMED |

### 3.5 How the writer works

| Rule | evidence | status |
| :- | :- | :- |
| For each request the writer appends, in this order: an `api-request-shape` if its hash is new to the file, an `api-request-blob` for each `tail` message whose hash is new and does not resolve from conversation records, then the `api-request`. | BIN `writeRequestReplayRecords`; RUN:all (record order in every sandbox file) | CONFIRMED |
| The writer keeps an index of hashes already in the file: shape and blob hashes, and the hashes of the conversation runs of 4.2. A message whose hash is in the index is not written again. | BIN class with `recordHashes` and `noteLine`; RUN:bash, RUN:subagent (later requests resolve earlier messages from the conversation records) | CONFIRMED |
| A `compact_boundary` clears that index, so blobs written before the boundary are written again after it when needed. | BIN `forgetClaims` on a compact boundary; SHAPE: one main transcript holds 1,244 blob records for 998 distinct hashes | OBSERVED |
| When a session resumes, the index is seeded from the last 50 MB (52,428,800 bytes) of the file. | BIN `Ypr=52428800` and `seedReplayIndex` | INFERRED |
| The chain state of 4.3 lives only in memory, per `agentId ?? sessionId`, for at most 64 chains (the oldest is dropped). A new process, a compaction, a file rewrite and a resume replay all start every chain again with `keep` 0. | BIN `oar=64`; COUNT[4 chains] (section 4.3) | INFERRED |
| Records of a session are held until its first `user` record is written, then flushed. | BIN `awaitingFirstUserRow` | INFERRED |
| The request record is written asynchronously. The request's `id` is handed to the reply before the write ends, so a reply can name a request whose record was never written (section 6). | BIN; COUNT[6 reply records per request]: 18 `requestRef` values name no request record | OBSERVED |

### 3.6 When a request is not recorded

The request is still sent. Only the record is skipped.

| Reason (BIN string) | When |
| :- | :- |
| (gate) | `querySource` is neither main nor subagent (section 7), or the recording flag is off (`CLAUDE_CODE_ELEGANT_MEADOW`, else the feature flag `tengu_elegant_meadow`, default on), or the session has no transcript file yet, or the file starts with a `fork-context-ref`. |
| `shape_too_large` | The shape record would exceed 2,000,000 bytes (BIN `Vpr=2000000`). |
| `stale_hash` | A memoized message hash, checked again at a random index, no longer matches its message. |
| `extra_body_unrecordable` | `CLAUDE_CODE_EXTRA_BODY` sets a key whose sent value cannot be recorded faithfully. |
| `cut_refused`, `laptop_memory_cutter_unregistered` | The account-memory cut below failed. |

**Cuts.** When a user message carries an account-memory snapshot, the record replaces that text with `[Request-replay record: an account-memory snapshot was sent here. It is never saved, so it is not in this record.]` and adds `cut: {kind: "laptop-account-memory", cuts, placeholders, messages}` and `cutRequestDigest`. A second kind, `tool-mark-address`, exists. A rebuilt request with a cut is exact except for the cut text. (BIN; not seen locally; INFERRED.)

**`api-request-resumed`.** After a paused request is answered, the writer adds `{type, sessionId, agentId, timestamp, version, requestFields?, messageId, pausedRequestId, resumedRequestId}`. It is not written for `hook_agent`. (BIN; not seen locally; INFERRED.)

## 4. Rebuilding a request

### 4.1 Algorithm

```
read the file once:
  shapes[shapeHash]   = shape          for each api-request-shape
  blobs[hash]         = message        for each api-request-blob
  requests[id]        = record         for each api-request
  runs[hash]          = message        for each prefix of each run (4.2)

messages(R):
  if R.base is absent:            hashes = R.tail
  elif R.base not in requests:    unresolvable (base is in another file or lost)
  else:                           hashes = messages(requests[R.base])[0:R.keep] + R.tail

resolve(h):
  if h in blobs: return blobs[h]   (as stored, so string content stays a string)
  if h in runs:  return runs[h]    ({role, content: blocks})
  else: unresolved

body(R):
  body = copy of R.params
  body.system, body.tools = shapes[R.shapeHash].system, .tools
  body.messages = [resolve(h) for h in messages(R)]
  for {path, value} in R.cacheControl:
      walk path from body; when the path enters a message's content, turn string content into blocks
      set cache_control = value on the object it reaches
  check: sha256(canonical_json(digest_view(body))) == R.requestDigest   (3.3)
```

`session_record.Transcript.rebuild` implements this. `base` always names a request in the same file: a subagent's requests chain only within the subagent's file (COUNT: no request names a base that is missing from its file).

### 4.2 Runs: messages that come from conversation records

A message that the conversation records already hold is not stored as a blob. Its hash is the hash of a **run**:

- A run is a maximal sequence of `user` records, or of `assistant` records that share one `message.id`. Records of other types, such as `attachment` and `system`, sit between them without ending the run. A `user` or `assistant` record without `message.role` or `message.content` ends it.
- The run's message is `{"role": <role>, "content": <the content blocks of its records, in file order>}`, with string content turned into one text block.
- Every prefix counts. After each record, the hash of the run so far is a valid message hash. A request sent while a reply is half written, or before a second user record joined the run, names a prefix.

| Rule | evidence | status |
| :- | :- | :- |
| A tool result record and the `isMeta` record that follows it with a skill's body are one API user message. | RUN:skill (the second request's last message resolves from a run of two `user` records) | CONFIRMED |
| An assistant reply split over several records (one content block each) is one API message. | RUN:bash (two reply records, one `message.id`; the next request resolves the assistant message from the transcript) | CONFIRMED |
| Messages that never appear as conversation records are always blobs: every `system`-role message (attachments, section 11), and a user message that carries rendered attachments beside the prompt or the tool result. | RUN:all (the first request's two messages are blobs); COUNT: 1,912 of 7,313 blobs have role `system` | CONFIRMED |

### 4.3 `base`, `keep` and `tail`

Claude Code compares each request with the previous request of the same chain (the same `agentId`, or the main thread). `keep` is the length of their common prefix of message hashes, `base` is that previous request, and `tail` holds the rest. A request's message list is therefore `messages(base)[0:keep] + tail`, never longer than needed.

| Case (COUNT[4 chains] and COUNT[7 compaction], 9,923 requests) | Count | Explanation | status |
| :- | -: | :- | :- |
| `keep` equals the length of the base's list: the request extends its base. | 9,504 | The normal turn. | OBSERVED |
| `keep` 0, no `base`: the first request of a file (16 main, 139 subagent files). | 155 | A chain starts empty. | OBSERVED |
| `keep` 0, no `base`: the first request after a compact boundary. | 50 | Compaction clears every chain (3.5). | OBSERVED |
| `keep` 0, no `base`, elsewhere. | 191 | A resume (a new process starts every chain empty), or a chain dropped from the 64-chain table. | INFERRED |
| `keep` is less than the base's length. | 23 | See below. | OBSERVED |

**Requests that keep fewer messages than their base holds.** In all 23 local cases the request drops the base's last message (a `user` message), or its last two (a `user` message and the `system` message after it), and sends in their place a longer version of the same user message: the old blocks with the last text block extended or one or more blocks appended. In 22 of 23 every old block is unchanged or extended; in one the last text block differs. No other request of the same chain lies between the base and the request, and the query source is the same. The base had a reply record in 11 of the 23 and none in 12. The pattern fits text typed or queued while the base request was in flight and then folded into the pending user message. A reader must not assume that a chain only grows: rebuild every request from its own `base`, `keep` and `tail`. (OBSERVED; the cause is INFERRED.)

### 4.4 Results on this machine

Main transcripts, from `session_shape.py --row` (index and version only):

| # | versions | requests | distinct message hashes | resolved | blobs whose hash matches | digests that match |
| -: | :- | -: | -: | -: | :- | :- |
| 0 | 2.1.328 | 484 | 999 | 999 | 363 of 363 | 484 of 484 |
| 1 | 2.1.318 | 89 | 189 | 189 | 83 of 83 | 89 of 89 |
| 2 | 2.1.318 | 9 | 18 | 18 | 13 of 13 | 9 of 9 |
| 3 | 2.1.319 to 2.1.322 | 941 | 2034 | 2034 | 1244 of 1244 | 941 of 941 |
| 4 | 2.1.319 to 2.1.322 | 207 | 354 | 354 | 213 of 213 | 207 of 207 |
| 5 | 2.1.319 | 52 | 107 | 107 | 74 of 74 | 52 of 52 |
| 6 | 2.1.304 | 260 | 457 | 457 | 184 of 184 | 260 of 260 |
| 7 | 2.1.329 | 37 | 77 | 77 | 14 of 14 | 37 of 37 |
| 8 | 2.1.329 | 32 | 67 | 67 | 5 of 5 | 32 of 32 |
| 9 | 2.1.278 to 2.1.318 | 64 | 1449 | 1449 | 571 of 571 | 64 of 64 |
| 10 | 2.1.319 | 6 | 17 | 17 | 13 of 13 | 6 of 6 |
| 11 | 2.1.319 | 5 | 14 | 14 | 11 of 11 | 5 of 5 |
| 12 | 2.1.319 | 17 | 50 | 50 | 27 of 27 | 17 of 17 |
| 13 | 2.1.319 | 31 | 92 | 92 | 48 of 48 | 31 of 31 |
| 14 | 2.1.319 | 15 | 44 | 44 | 20 of 20 | 15 of 15 |
| 15 | 2.1.319 | 13 | 38 | 38 | 20 of 20 | 13 of 13 |

Total: 2,262 requests, every message hash resolved, every blob hash and every digest matched. Files 0 and 7 belonged to live sessions.

Subagent transcripts (139 files, versions 2.1.318 to 2.1.328, aggregated): 7,654 requests, 16,016 distinct message hashes, 16,016 resolved, 4,404 of 4,404 blob records whose hash matches, 7,654 of 7,654 digests that match.

No message hash stayed unresolved, and every rebuilt request matched its digest. The turns that versions 2.1.278 to 2.1.293 wrote hold no request records (section 13).

## 5. `cacheControl`

| Rule | evidence | status |
| :- | :- | :- |
| `path` is a JSON path, as an array, to the object that held `cache_control` in the sent body: `["system", i]` (a system block, indexed without the billing block), `["tools", i]`, or `["messages", n, "content", m]` (block `m` of message `n`). `value` is the `cache_control` object. | RUN:all (the oracle puts each mark back at its path and compares with the received body) | CONFIRMED |
| Two system layouts occur. In the 5-minute layout the shape has 2 system blocks and both carry `{"type":"ephemeral"}`. In the 1-hour layout it has 3 blocks, and blocks 1 and 2 carry `{"scope":"global","ttl":"1h","type":"ephemeral"}` and `{"ttl":"1h","type":"ephemeral"}`. Locally every main-thread request (2,262) used the 1-hour layout and every subagent request (7,661) the 5-minute one. The sandbox, which authenticates with an API key, got the 5-minute layout on the main thread, so the layout depends on the account as well as the thread. | COUNT[5 system marks], COUNT[5 mark values], a count by thread; RUN:all | OBSERVED |
| No tool definition carries a mark. | COUNT[5 tool marks]: 0 in 9,923 requests | OBSERVED |
| Exactly one message mark per request, on the last block of the last message (8,141 on a `user` message, 1,774 on a `system` message). In 8 requests it sits on the last `user` message, and one `system` message follows. Its value is `{"type":"ephemeral"}`, or `{"ttl":"1h","type":"ephemeral"}` in the 1-hour layout. | COUNT[5 message mark] | OBSERVED |
| The marks are breakpoints of the prompt cache. Along a chain, the next request reads what the previous one read and wrote: `cache_read_input_tokens(next) = cache_read_input_tokens + cache_creation_input_tokens (previous)` in 9,218 of 9,312 consecutive pairs. The other 94 are UNKNOWN (cache expiry is one candidate). | COUNT[5 cache arithmetic] | OBSERVED |
| `cache_creation.ephemeral_1h_input_tokens + ephemeral_5m_input_tokens = cache_creation_input_tokens` in every reply. Usage splits written tokens by TTL, not by breakpoint, so which mark wrote how many tokens is not recorded. | COUNT[5 cache arithmetic]: 9,312 of 9,312 | OBSERVED |

## 6. Replies and usage

| Rule | evidence | status |
| :- | :- | :- |
| A reply is written as one `assistant` record per content block, in order, with `apiBlockIndex` and `requestRef`. All records of one reply share `message.id`. | RUN:bash (a text block and a tool_use block gave two records); COUNT[6 reply records per request]: 9,764 of 9,764 replies share one id | CONFIRMED |
| **Every record of a reply carries the final usage**, the numbers of the stream's `message_delta`, not the partial numbers of `message_start`. Any record of the reply can be read. | RUN:bash (the endpoint sent `output_tokens` 1 at the start and the final number at the end; both records hold the final number); COUNT: 9,764 of 9,764 replies have one usage across their records | CONFIRMED |
| Usage keys: `input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `cache_creation.{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}`, `output_tokens`, `output_tokens_details.thinking_tokens`, `server_tool_use`, `service_tier`, `inference_geo`, `speed`, `fallback_credit`, `iterations`. | COUNT (key names) | OBSERVED |
| `usage.iterations` is a list with one entry of type `message` whose numbers equal the top-level ones (22,187 records), or an empty list (15). The top level is the number to show. | COUNT[6 usage.iterations] | OBSERVED |
| A record whose `message.model` is `<synthetic>` is an error the CLI wrote in place of a reply: rate limit 13, server error 6, authentication failed 2, invalid request 1. None has a `requestRef`. | COUNT[6 synthetic records] | OBSERVED |
| A request without a reply record (177 locally) was interrupted, failed, or was answered by a `<synthetic>` error. | COUNT[6 reply records per request] | OBSERVED |
| A `requestRef` that names no request record (18 locally) points at a request whose record was refused (3.6) or not yet written (3.5). | COUNT | INFERRED |

## 7. Which requests are recorded

The gate classifies `querySource` (BIN `function Er(e){if(e===void 0)return;if(e.startsWith("repl_main_thread")||e==="sdk")return"main";if(e.startsWith("agent:")||e==="hook_agent")return"subagent";return"auxiliary"}`). Main and subagent requests are recorded. Auxiliary requests are not.

| `querySource` | Class | Recorded | Local count (COUNT[3.4 query sources]) | evidence | status |
| :- | :- | :- | -: | :- | :- |
| `repl_main_thread`, `repl_main_thread:outputStyle:<style>` | main | yes | 2,175 | COUNT | OBSERVED |
| `sdk` (print mode and the SDK) | main | yes | 87 | RUN:all | CONFIRMED |
| `agent:builtin:general-purpose`, `agent:builtin:fork`, `agent:custom`, `agent:custom:<plugin>:<agent>` | subagent | yes, in the subagent's file | 1,842 / 23 / 4,929 / 867 | RUN:subagent, COUNT | CONFIRMED |
| `hook_agent` | subagent | yes | 0 | BIN | INFERRED |
| `compact` | auxiliary | no | 0 | RUN:compact (the endpoint received the summary request, the transcript has no record of it) | CONFIRMED |
| `generate_session_title`, `side_question`, `web_search_tool`, `web_fetch_apply`, `auto_mode`, `auto_mode_critique`, `hook_prompt`, `compact_fab_check`, `chrome_mcp`, `artifact_comment_reply`, and the other auxiliary sources | auxiliary | no | 0 | BIN | INFERRED |

A reader can show that an auxiliary call happened only through its result: the compaction summary (section 8) or an `ai-title` record.

## 8. Compaction

| Rule | evidence | status |
| :- | :- | :- |
| A compaction writes a `system` record with `subtype: "compact_boundary"` and `compactMetadata`. The manual compaction of the sandbox wrote `trigger` (`manual`), `preTokens`, `postTokens`, `durationMs`, `cumulativeDroppedTokens` and `preCompactOrganizationsCoverStart`. The local automatic ones (`trigger: "auto"`) also carry `precomputed`, `preservedMessages{anchorUuid, uuids, allUuids}`, `preservedSegment{anchorUuid, headUuid, tailUuid}`, `preCompactDiscoveredTools`, `preCompactArtifactReadVersions` and `preCompactOrganizations`. | RUN:compact; COUNT[7 compaction]: 54 boundaries, all `auto` | CONFIRMED |
| Within the next few records comes a `user` record with `isCompactSummary` and `isVisibleInTranscriptOnly` that holds the summary text. In the sandbox it came first, followed by the `/compact` command's records. In the local automatic compactions, attachments (instructions, session context, date) came before it and file attachments (`compact_file_reference`, `file`) after it. | RUN:compact; COUNT: a summary record follows 54 of 54 boundaries | CONFIRMED |
| The summarizing request itself is not recorded (section 7). | RUN:compact | CONFIRMED |
| The first recorded request after the boundary starts a new chain: `keep` 0, no `base`. Its first message is a `user` message that holds the rendered user attachments, the summary text, the records that followed it (in the sandbox the `/compact` command and its output) and the next prompt. A `system`-role message follows. | RUN:compact; COUNT: 50 of 53 boundaries followed by a request | CONFIRMED |
| In 3 of 53 the next request record keeps earlier messages. | COUNT | UNKNOWN |

## 9. Subagents

| Rule | evidence | status |
| :- | :- | :- |
| A subagent writes its own file under the parent session (1.1) with conversation records and its own request records. Its records carry the parent's `sessionId`, `isSidechain: true` and `agentId`. | RUN:subagent; COUNT[8 subagents]: 139 of 139 files hold request records, and every request record carries the file's `agentId` | CONFIRMED |
| The parent's `tool_use` that started the subagent has the id that `meta.json`'s `toolUseId` holds. The parent's tool result record has `toolUseResult.agentId` equal to the file's `<agent>`. A later parent `user` record with a task notification names the same id. | RUN:subagent | CONFIRMED |
| `meta.json` keys include `agentType`, `description`, `isBuiltIn`, `toolUseId`, `spawnDepth`, `parentAgentId`, `model`, `requestShape`, `requestNonInteractive`, `pickedEffortModel`, `finished`. | RUN:subagent | CONFIRMED |
| A subagent's chain is separate from the parent's and from other subagents' (keyed by `agentId`). | BIN; RUN:subagent (its first request has `keep` 0) | CONFIRMED |
| In 2.1.329 print mode the `Agent` tool ran the subagent in the background and the parent received a task notification later. | RUN:subagent | CONFIRMED |

## 10. Tool output saved to `tool-results/`

| Rule | evidence | status |
| :- | :- | :- |
| When a tool result is too large, the message sent to the model holds a stand-in instead: `<persisted-output>\nOutput too large (<size>). Full output saved to: <path>\n\nPreview (first 2KB):\n...` (or a preview of the start and the end), and the full output goes to `<config>/projects/<key>/<session>/tool-results/<id>.txt`. | RUN:bigbash: 96,000 bytes of output, a 2,368-character stand-in in the request, the file in `tool-results/` holds all 96,000 bytes | CONFIRMED |
| **The request carried the stand-in, not the full text.** The rebuilt request is exact without reading the file. The file is what the reader shows when the operator asks for the full output. | RUN:bigbash (digest match); COUNT[9 persisted output]: 32 request messages carry a stand-in | CONFIRMED |
| The conversation record of such a result also holds the stand-in, with `toolUseResult` as an object. | A count during the research: 33 of 33 such records | OBSERVED |

## 11. Attachments

An `attachment` record holds `attachment: {type, ...}`. When the attachment was sent to the model, the record also holds:

- `rendered`: a list of `{content}` pieces, the text that was sent;
- `renderedRole`: `user` or `system`, the role it was sent as (from 2.1.293; section 13);
- `renderedBesideToolResult` (`remote_session_change`) and `renderedInHumanTurn` (`queued_command`): placement hints.

| Rule | evidence | status |
| :- | :- | :- |
| A piece with `renderedRole: "user"` appears verbatim inside a `user` message of the next request, as a text block beside the prompt or the tool result. | RUN:all (2 of 2 per scenario); COUNT[10 rendered attachments in the next request] | CONFIRMED |
| Pieces with `renderedRole: "system"` are sent in a `system`-role message (a message in `messages`, not the `system` parameter). The `<system-reminder>\n...\n</system-reminder>` wrapper is removed and the pieces of one turn are joined with a blank line into **one** text block. | RUN:all (6 of 6, 7 of 7 with a SessionStart hook); COUNT | CONFIRMED |
| Attachments are attribution, not the source of truth. The request record already holds the exact message. A reader can use `rendered` to label which part of a message came from which attachment, by text containment. | RUN:all; COUNT (a few pieces per type are not found in the last 8 messages of the next recorded request; open question 6) | OBSERVED |

The 35 attachment types seen locally (COUNT[10 attachment fields] and a per-type count):

| Type | Records | Sent as | Notes |
| :- | -: | :- | :- |
| `hook_success` | 6,669 | system, 12 only | A hook's result. Usually not sent. |
| `async_hook_response` | 2,558 | not sent | |
| `deferred_tools_record` | 1,582 | not sent | Bookkeeping for deferred tools. |
| `hook_additional_context` | 752 | system (750), user (1) | `additionalContext` from a hook. |
| `total_tokens_reminder` | 490 | system (70); 420 without a role | |
| `environment` | 466 | system | Working directory, platform, model. |
| `prompt_snapshot` | 418 | not sent | A snapshot of the system prompt and tools. Not equal to the request (section 13). |
| `date` | 243 | system | |
| `model` | 210 | system | |
| `session_context` | 209 | user (64) | Git status and similar context. |
| `credential_org` | 208 | not sent | Holds an organisation id (section 15). |
| `budget_usd` | 195 | system | |
| `edited_text_file` | 194 | system | A file changed outside the conversation. |
| `remote_session_change` | 191 | user, beside a tool result | |
| `deferred_tools_delta` | 183 | system | |
| `mcp_instructions_delta` | 183 | system | |
| `silent_turn_reminder` | 183 | system | |
| `agent_listing_delta` | 178 | system | Subagent types for the Agent tool. |
| `skill_listing` | 177 | system | |
| `file` | 155 | user (144), system (10) | An `@`-mentioned or re-read file. |
| `instructions` | 132 | user | CLAUDE.md and rules. |
| `task_reminder` | 118 | system | |
| `queued_command` | 114 | system | A prompt queued while the model worked; `renderedInHumanTurn` holds its text. |
| `auto_mode` | 96 | system | |
| `compact_file_reference` | 92 | system | After a compaction. |
| `command_permissions` | 52 | not sent | |
| `thinking_drop` | 51 | not sent | Which thinking blocks were dropped. |
| `task_status` | 32 | system | |
| `invoked_skills` | 25 | system | |
| `read_truncation_notice` | 7 | system | |
| `skill_mention` | 5 | system | |
| `hook_system_message` | 4 | not sent | |
| `team_context` | 3 | system | Holds a team name and config path. |
| `session_cron_carry` | 2 | not sent | |
| `nested_memory` | 2 | user | A nested CLAUDE.md loaded on demand. |

Status: OBSERVED. "Sent as" counts records with `rendered`; "not sent" means no record of that type had `rendered`.

## 12. What a session list needs

| Item | Where | evidence | status |
| :- | :- | :- | :- |
| Session id | The file name, and `sessionId` on every record. | RUN:all | CONFIRMED |
| Title | Claude Code's session picker shows `customTitle ?? aiTitle ?? summary ?? firstPrompt`, else "untitled session": the last `custom-title` record (set by the operator), the last `ai-title` (written after an auxiliary request), a `summary` record, the first prompt. Titles and prompts are the operator's text. | BIN `function Ho(e){return e.customTitle??e.aiTitle??e.summary??(e.firstPrompt!==""?e.firstPrompt:"untitled session")}`; COUNT (1,384 `ai-title` records; none in the print-mode sandbox) | INFERRED |
| Last prompt | The last `last-prompt.lastPrompt`. | RUN:all | CONFIRMED |
| First and last time | The smallest and largest `timestamp` in the file. | RUN:all | CONFIRMED |
| Working directory | `cwd` of the first conversation record (1.2). | COUNT: 16 of 16 | OBSERVED |
| Branch | `gitBranch` on conversation records. It can change during a session. | RUN:all | CONFIRMED |
| Version | `version` on records. One file can hold several versions (a session resumed after an update). | COUNT[12 by version] | OBSERVED |
| Model | `params.model` of the request records, or `message.model` of reply records. | RUN:all | CONFIRMED |
| Entry point | `entrypoint`: `cli` for interactive sessions (3,278 main records), `sdk-cli` for print mode and the SDK (105). | RUN:all, COUNT[11 entrypoint] | CONFIRMED |
| Subagents | The `subagents/` directory and each `meta.json`. | RUN:subagent | CONFIRMED |
| Continued elsewhere | `continued-in` records name the session that continues this one. | COUNT (1 record) | OBSERVED |
| Mode | The last `permission-mode` and `mode` records. | COUNT | OBSERVED |

## 13. Versions

From COUNT[12 by version]. Each file can mix versions, so the table counts records, not files.

| Versions | Request records | `requestRef` on replies | `rendered` on attachments | `renderedRole` |
| :- | :- | :- | :- | :- |
| 2.1.278, 2.1.281, 2.1.283 | no | no | yes | no |
| 2.1.293 | no | no | yes | yes |
| 2.1.302 to 2.1.329 | yes | yes | yes | yes |

The first local version with request records is 2.1.302. Every main transcript on this machine holds request records, because each was written or resumed by 2.1.302 or later, but the turns written before that version have none.

**What a reader can recover from turns without request records:** the conversation (user and assistant runs, exactly as stored), the usage of each reply (exact), and the attachments with their `rendered` text. It cannot recover the exact system blocks, tool definitions or the assembly of messages. `prompt_snapshot` attachments exist from 2.1.278, but their system text never equalled the system blocks of a request recorded beside them (0 of 392 compared), so a request rebuilt from them is approximate. (OBSERVED.)

## 14. Print mode and interactive mode

| Rule | evidence | status |
| :- | :- | :- |
| `claude -p` writes the same record types as an interactive session: conversation records, request records, `last-prompt`, `atis-latch`, `queue-operation`, `tools-used`, `cost-state`. Its requests have `querySource: "sdk"` and its records `entrypoint: "sdk-cli"`. | RUN:all | CONFIRMED |
| Interactive requests have `querySource` `repl_main_thread` and `entrypoint: "cli"`. | COUNT[3.4], COUNT[11] | OBSERVED |
| The sandbox wrote no `ai-title`, `session-owner` or `bridge-session` records. The likely reason is that they need a login or network access the sandbox does not have. | RUN:all | OBSERVED |
| The oracle does not drive an interactive session, so the interactive rules rest on the local counts only. | | |

## 15. Fields that identify an account, an organisation, a host or a person

A reader must not show these by default, and a log or test must never print them. Listed by name only (COUNT[14 identity-like key paths] and BIN).

| Record | Field |
| :- | :- |
| `session-owner` | `accountUuid`, `organizationUuid` |
| `bridge-session` | `ownerAccountUuid`, `ownerOrganizationUuid`, `bridgeSessionId` |
| `hybrid-organization` | `organizationUuid` |
| `artifact-autoreact-ledger` | `accountUuid` |
| `attachment` of type `credential_org` | `attachment.organizationUuid` |
| `attachment` of type `session_context` | `attachment.context.userEmail` |
| `attachment` of type `team_context` | `teamName`, `teamConfigPath` |
| `system` (`compact_boundary`) | `compactMetadata.preCompactOrganizations`, `preCompactOrganizationsCoverStart` |
| `user` | `serverClassifierContext.context.git_state.visibility.{origin, remotes[], visibility_cache[]}.host` |
| subagent `meta.json` | `teamName` |
| every conversation record | `cwd` and other paths, which contain the home directory and so the user name |

The request body's `metadata.user_id` is not recorded (3.4).

## 16. Codex CLI and Gemini CLI, for comparison

Not run. From `docs/agents/codex.md`, `docs/agents/gemini-cli.md` and public pages.

**Codex CLI** stores each session as a "rollout": `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl` (`CODEX_HOME` defaults to `~/.codex`), with older sessions moved to `archived_sessions/`. Each line is `{timestamp, type, payload}` with types such as `session_meta`, `turn_context` (model, working directory, approval and sandbox policy, effort), `response_item` (messages, reasoning, function calls and their outputs) and `event_msg`, plus `compacted`. Prompt history goes to `$CODEX_HOME/history.jsonl`, and SQLite state files such as `state_5.sqlite` sit in `CODEX_HOME` (`docs/agents/codex.md` section 3.3). The rollout records the conversation items that make up the model input and the per-turn context, **not** the request body and no digest of it. The exact prompt for a directory can be produced with `codex debug prompt-input`, which `docs/agents/codex.md` uses as its oracle. (DOC: third-party descriptions of the rollout format, such as https://docs.rs/crate/toolpath-codex/0.5.0/source/src/types.rs; the paths agree across them, field details vary by version.)

**Gemini CLI** stores each session under `~/.gemini/tmp/<slug>/chats/` as `session-<date>-<id>.json` (some versions write JSON Lines). `<slug>` comes from `~/.gemini/projects.json`, which maps an absolute working directory to a slug (`docs/agents/gemini-cli.md` section 4); older versions and the public page call it a hash of the project root. A session holds `sessionId`, `projectHash`, `startTime`, `lastUpdated` and `messages`, each message with its text, tool calls and a `tokens` object (`input`, `output`, `cached`, `thoughts`, `tool`, `total`). `general.sessionRetention` deletes sessions after 30 days by default. Prompt history goes to `logs.json`. It records turns and token counts, **not** the request body (system instruction, tool declarations). (DOC: https://geminicli.com/docs/cli/session-management/; third-party schema notes.)

**For the types:** only Claude Code records the request. Codex and Gemini record the conversation and the token counts. A model that has "messages with usage" as its base and "rebuilt request" as an optional, exact layer fits all three.

## 17. Exact, approximate, unknown

**Exact** (for recorded requests, 2.1.302 and later):

- The request body: system blocks, tool definitions, every message with its role and extra keys, the parameters, and the position and value of every cache mark. The record's own `requestDigest` checks it, and the oracle compared it with the bytes received.
- The order of requests, their model, effort, `max_tokens` and thinking settings, and their chain (which request a request continues).
- The usage of each reply.
- Which messages came from conversation records and which only from the request record.
- A large tool output as the model saw it (the stand-in), and the full output in `tool-results/`.

**Approximate:**

- The parts of the request that are never recorded: the billing block (its format is known), `metadata`, `stream`, HTTP headers.
- Which attachment or source produced which part of a message (text containment, not a recorded link).
- Turns written before 2.1.302: the conversation and usage are exact, the system, tools and message assembly are not.
- Calls that are not recorded (compaction, title generation and the other auxiliary sources), known only by their result.
- How many cached tokens each cache mark accounts for.

**Unknown:**

- `api-request-resumed`, `cut` and `withheld` in practice: none occurs locally, so their effect on rebuilding is untested.
- Why 3 requests after a compaction keep earlier messages, and why 94 chain pairs break the cache arithmetic.
- Whether a request refused for `shape_too_large`, `stale_hash` or another reason leaves a trace besides a `requestRef` with no record.
- Interactive mode under the oracle.
- Future versions: the record is new (2.1.302) and gated by a feature flag that defaults to on.

## 18. The oracle and the tools

All three tools are in `tools/oracle/claude/` and share `session_record.py`, which implements sections 3 and 4.

**`session_oracle.py [scenario ...] [--keep]`** runs the installed `claude -p` against a loopback endpoint that speaks enough of the streaming Messages API to finish a turn, in a sandbox under `$TMPDIR`: its own `HOME` and `CLAUDE_CONFIG_DIR`, a dummy API key, every proxy variable pointed at a tripwire that counts connections, and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`. The sandbox also needs a stub `security` first on `PATH` (`<sandbox>/bin/security`, then `/usr/bin:/bin:/usr/sbin:/sbin`). Claude Code asks the macOS `security` tool, found by name on `PATH`, for its credentials: `find-generic-password` twice per start, and `-i` with commands on standard input in other flows. Under a sandbox home there is no keychain, so the real tool makes macOS show a "keychain not found" dialog on the operator's screen and the CLI waits on it. The stub reads standard input when called with `-i` and exits 44, which the CLI takes as "not found", so no real `security` process starts and no keychain is read or written. The CLI authenticates with the dummy key either way. Never link the real `~/Library/Keychains` into a sandbox instead. For every request the endpoint received it finds the request record with the same digest, rebuilds the body from the transcript, and compares the system blocks, tools, messages, cache marks, parameters and digest. It checks the usage on every reply record against what the endpoint sent, lists received requests that were not recorded, and checks each scenario's own facts. It removes the sandbox unless `--keep` is given, and prints `SKIP` when `claude` is not installed.

| Scenario | What it shows | Result on 2.1.329 |
| :- | :- | :- |
| `oneshot` | One prompt, one reply. | PASS |
| `bash` | A Bash `echo` round trip; a reply of two blocks. | PASS |
| `bigbash` | A Bash output large enough to be saved under `tool-results/`. | PASS |
| `skill` | A skill invoked through the Skill tool. | PASS |
| `mention` | A file mentioned with `@`. | PASS |
| `hook` | A SessionStart hook that prints a line. | PASS |
| `subagent` | The Agent tool with the general-purpose subagent. | PASS |
| `compact` | A turn, `/compact` on resume, a turn after it. | PASS |
| `projectkey` | Sessions started in a subdirectory and in a nested worktree. | PASS |

Every scenario of the brief was reached. The tripwire counted 0 connections in every run.

**`session_shape.py [--no-digest] [--row] <transcript> ...`** prints, for one transcript, the numbers a reader must reproduce: records by type, versions, requests by query source, chain cases, shapes and blobs with their hash checks, how the referenced message hashes resolve, reply counts, cache mark forms, and how many rebuilt requests match their digest. `--row` prints the table of 4.4.

**`session_survey.py <config>/projects [--digest]`** prints the counts cited as COUNT in this spec.

The shape and survey tools print counts and categorical values only (record types, versions, roles, query sources with custom agent names replaced by `*`, key names), so they are safe to run on private transcripts.

## Open questions

1. `api-request-resumed`, `cut` and `withheld` have not been seen. Rebuilding a request that has them follows the binary, untested.
2. In 3 of 53 compactions the next request record keeps earlier messages. Find out whether it belongs to a request that was in flight across the boundary.
3. In 94 of 9,312 chain pairs the cache read of the next request is not the read plus the write of the previous one. Cache expiry between requests is the first candidate.
4. The oracle runs print mode only. An interactive run would confirm `repl_main_thread`, `ai-title` and the records that need a login.
5. `CLAUDE_CODE_PROJECT_DIR_NAME` and the subagent subdirectory layout were read from the binary, not run.
6. 420 `total_tokens_reminder` attachments have `rendered` but no `renderedRole`, and their text was not found in the next request. A few pieces of other types were not found either. Find out whether they were sent.

## Provenance

- Binary: `~/.local/share/claude/versions/2.1.329`, searched with `tools/oracle/claude/bgrep.py`.
- Oracle: `python3 tools/oracle/claude/session_oracle.py`, every scenario PASS on 2.1.329.
- Counts: `python3 tools/oracle/claude/session_survey.py ~/.claude/projects` and `python3 tools/oracle/claude/session_shape.py --row <each main transcript>`, run on 2026-11-21 over the transcripts on the research machine. No transcript content was printed or copied.
