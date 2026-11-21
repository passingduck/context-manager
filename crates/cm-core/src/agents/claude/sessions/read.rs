//! One session record read into a [`TraceData`] (session spec, sections 3 to 11).
//!
//! The record is read once, line by line. Each line adds to tables keyed by hash: content blocks,
//! runs of conversation records (4.2), blobs, shapes, requests and replies. Then each request is
//! rebuilt from its own `base`, `keep` and `tail` (4.3) and its digest is checked (3.3). A request
//! is exact when every message it names resolves and the rebuilt body hashes to its digest.

use std::collections::HashMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::model::{AbsPath, ApiError, Boundary, PieceKind, Role, TraceInfo, TraceProblem, Usage};
use crate::probe_fs::ProbeFs;
use crate::trace::{
    Block, BlockId, MarkAt, Message, MessageId, Request, Span, StepData, TraceData,
};

use super::canon::{self, Hash};
use super::label::{self, Source};
use super::record::{
    Attachment, CompactMetadata, Line, Mark, MessageRec, NestedContent, Rendered, ToolUseResult,
    UsageRec, get,
};

/// Top-level body keys that the record and `requestDigest` leave out (section 3.3).
const UNRECORDED: [&str; 6] = [
    "metadata",
    "stream",
    "thread",
    "diagnostics",
    "safeguards",
    "data_flow",
];

/// Text a command record holds: the `/command` the operator typed and its output.
const COMMAND_TAGS: [&str; 4] = [
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
];

/// Reads the record at `info.file`, or only its first `prefix` bytes.
pub(super) fn read(
    fs: &ProbeFs,
    info: TraceInfo,
    prefix: Option<u64>,
) -> Result<TraceData, ApiError> {
    let mut reader = Reader::default();
    let mut line_no = 0usize;
    let lines = fs.read_lines(&info.file, prefix, &mut |bytes, ended| {
        reader.line(line_no, bytes, ended);
        line_no += 1;
    })?;
    let mut problems = Vec::new();
    let unreadable = reader.unreadable + lines.long_lines;
    if unreadable > 0 {
        problems.push(TraceProblem::UnreadableLines { lines: unreadable });
    }
    if let Some(bytes) = lines.stopped_at_limit {
        problems.push(TraceProblem::RecordTooLarge {
            bytes,
            limit: fs.line_limits().record,
        });
    }
    let (blocks, messages, steps, boundaries) = reader.finish();
    Ok(TraceData {
        info,
        read: lines.bytes,
        blocks,
        messages,
        steps,
        boundaries,
        parts: Vec::new(),
        problems,
    })
}

/// Where a block was read. Part of the key a block is stored under, so the same JSON read as a
/// system block and as message content stays two blocks with their own kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Origin {
    System,
    Tool,
    Record,
    Blob,
}

/// Every distinct block once: its spans, and its canonical JSON for the digest.
#[derive(Default)]
struct Blocks {
    blocks: Vec<Block>,
    /// The canonical JSON of every block, end to end, `cache_control` left out.
    canon: Vec<u8>,
    at: Vec<(usize, usize)>,
    index: HashMap<(Origin, Hash), BlockId>,
}

impl Blocks {
    fn find(&self, origin: Origin, hash: &Hash) -> Option<BlockId> {
        self.index.get(&(origin, *hash)).copied()
    }

    fn add(&mut self, origin: Origin, hash: Hash, canon: &[u8], block: Block) -> BlockId {
        let id = self.blocks.len() as BlockId;
        let start = self.canon.len();
        self.canon.extend_from_slice(canon);
        self.at.push((start, self.canon.len()));
        self.blocks.push(block);
        self.index.insert((origin, hash), id);
        id
    }

    fn canon(&self, id: BlockId) -> &[u8] {
        let (start, end) = self.at[id as usize];
        &self.canon[start..end]
    }
}

/// A run being read: consecutive `user` records, or `assistant` records of one `message.id`.
struct OpenRun {
    run: u32,
    role: String,
    message_id: Option<String>,
    hasher: Sha256,
}

struct Run {
    role: String,
    blocks: Vec<BlockId>,
}

/// A message of a blob, with the canonical JSON of its keys around `content`.
struct Blob {
    role: String,
    blocks: Vec<BlockId>,
    /// `{` and the keys that sort before `content`, each followed by a comma.
    before: Vec<u8>,
    /// The keys that sort after `content`, each after a comma, and `}`.
    after: Vec<u8>,
}

#[derive(Default)]
struct Shape {
    system: Option<Vec<BlockId>>,
    tools: Option<Vec<BlockId>>,
}

struct RequestRec {
    id: String,
    line: usize,
    at: Option<String>,
    shape: Option<Hash>,
    params: Map<String, Value>,
    base: Option<String>,
    keep: usize,
    tail: Vec<Option<Hash>>,
    marks: Vec<Mark>,
    digest: Option<Hash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReplyKey {
    Request(String),
    Message(String),
}

struct Reply {
    line: usize,
    at: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    blocks: Vec<BlockId>,
}

#[derive(Default)]
struct Reader {
    blocks: Blocks,
    runs: Vec<Run>,
    /// The hash of every prefix of every run, to the run and the prefix length. The first wins.
    run_prefixes: HashMap<Hash, (u32, u32)>,
    open: Option<OpenRun>,
    blobs: HashMap<Hash, Blob>,
    shapes: HashMap<Hash, Shape>,
    requests: Vec<RequestRec>,
    request_index: HashMap<String, usize>,
    replies: Vec<Reply>,
    reply_index: HashMap<ReplyKey, usize>,
    /// `tool_use` id to the tool's name.
    tool_names: HashMap<String, String>,
    /// `tool_use` id to the structured result recorded beside its `tool_result`.
    tool_results: HashMap<String, ToolUseResult>,
    /// Every text a record says was sent, in record order, with the requests read before it.
    sources: Vec<Source>,
    source_turns: Vec<u32>,
    /// The text blocks of blobs, with the requests read before each. Their spans come from the
    /// sources once the whole record is read (see [`Reader::label_blobs`]).
    deferred: Vec<(BlockId, u32)>,
    requests_read: u32,
    boundaries: Vec<(usize, Boundary)>,
    unreadable: u32,
}

impl Reader {
    fn line(&mut self, no: usize, bytes: &[u8], ended: bool) {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let line: Line = match serde_json::from_slice(bytes) {
            Ok(line) => line,
            // Section 14 of the design: an unterminated last line that does not parse is still
            // being written.
            Err(_) if !ended => return,
            Err(_) => {
                self.unreadable += 1;
                return;
            }
        };
        match line.ty().as_deref() {
            Some("user") => self.conversation(no, &line, "user"),
            Some("assistant") => self.conversation(no, &line, "assistant"),
            Some("attachment") => self.attachment(&line),
            Some("system") => self.system(no, &line),
            Some("api-request-shape") => self.shape(&line),
            Some("api-request-blob") => self.blob(&line),
            Some("api-request") => self.request(no, &line),
            _ => {}
        }
    }

    /// A `user` or `assistant` record: a part of a run (4.2), and for `assistant` a part of a
    /// reply (6).
    fn conversation(&mut self, no: usize, line: &Line, ty: &str) {
        let Some(mut message) = get::<MessageRec>(line.message) else {
            self.open = None;
            return;
        };
        let (Some(Value::String(role)), Some(content)) =
            (message.role.clone(), message.content.take())
        else {
            self.open = None;
            return;
        };
        let role = role.as_str();
        let blocks = as_blocks(content);
        if role == "assistant" {
            for block in &blocks {
                if let (Some(id), Some(name)) = (str_at(block, "id"), str_at(block, "name"))
                    && is_tool_call(block)
                {
                    self.tool_names.insert(id.to_owned(), name.to_owned());
                }
            }
        } else if let Some(result) = get::<ToolUseResult>(line.tool_use_result) {
            for block in &blocks {
                if let Some(id) = str_at(block, "tool_use_id") {
                    self.tool_results.insert(id.to_owned(), result.clone());
                }
            }
        }
        let text_as = if role == "assistant" {
            (PieceKind::ModelText, None)
        } else {
            self.user_text_kind(line, &blocks)
        };
        let mut ids = Vec::with_capacity(blocks.len());
        let mut canons = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let canon = canon::text(block, true);
            ids.push(self.intern(Origin::Record, block, &canon, role, &text_as));
            canons.push(canon);
        }
        if role == "user" && ty == "user" {
            let sources: Vec<Source> = blocks
                .iter()
                .filter_map(text_of)
                .map(|text| Source::new(text.to_owned(), text_as.0, text_as.1.clone()))
                .collect();
            self.add_sources(sources);
        }

        // 4.2: a run continues for the same role, and for `assistant` the same message id.
        let message_id = match &message.id {
            Some(Value::String(id)) => Some(id.clone()),
            _ => None,
        };
        let continues = self.open.as_ref().is_some_and(|open| {
            open.role == *role
                && (role == "user" || (message_id.is_some() && open.message_id == message_id))
        });
        if !continues {
            self.runs.push(Run {
                role: role.to_owned(),
                blocks: Vec::new(),
            });
            self.open = Some(OpenRun {
                run: self.runs.len() as u32 - 1,
                role: role.to_owned(),
                message_id: message_id.clone(),
                hasher: Sha256::new_with_prefix(b"{\"content\":["),
            });
        }
        let open = self.open.as_mut().expect("a run is open");
        let run = &mut self.runs[open.run as usize];
        for (id, canon) in ids.iter().zip(&canons) {
            if !run.blocks.is_empty() {
                open.hasher.update(b",");
            }
            open.hasher.update(canon);
            run.blocks.push(*id);
        }
        let mut end = Vec::new();
        end.extend_from_slice(b"],\"role\":");
        canon::write_str(&mut end, role);
        end.push(b'}');
        let mut hasher = open.hasher.clone();
        hasher.update(&end);
        let hash: Hash = hasher.finalize().into();
        let len = run.blocks.len() as u32;
        self.run_prefixes.entry(hash).or_insert((open.run, len));

        if ty == "assistant" {
            self.reply(no, line, &message, ids);
        }
    }

    /// Section 6: every record of a reply carries the final usage, so the last one read wins.
    fn reply(&mut self, no: usize, line: &Line, message: &MessageRec, blocks: Vec<BlockId>) {
        let model = match &message.model {
            Some(Value::String(model)) => Some(model.clone()),
            _ => None,
        };
        // A `<synthetic>` record is an error the CLI wrote in place of a reply.
        if model.as_deref() == Some("<synthetic>") {
            return;
        }
        let key = match (get::<String>(line.request_ref), &message.id) {
            (Some(request), _) => ReplyKey::Request(request),
            (None, Some(Value::String(id))) => ReplyKey::Message(id.clone()),
            _ => return,
        };
        let usage = message
            .usage
            .clone()
            .and_then(|u| serde_json::from_value::<UsageRec>(u).ok())
            .map(|u| Usage {
                input: u.input_tokens.unwrap_or(0),
                cache_read: u.cache_read_input_tokens.unwrap_or(0),
                cache_write: u.cache_creation_input_tokens.unwrap_or(0),
                output: u.output_tokens.unwrap_or(0),
                thinking: u.output_tokens_details.and_then(|d| d.thinking_tokens),
            });
        let index = *self.reply_index.entry(key).or_insert_with(|| {
            self.replies.push(Reply {
                line: no,
                at: get(line.timestamp),
                model: model.clone(),
                usage: None,
                blocks: Vec::new(),
            });
            self.replies.len() - 1
        });
        let reply = &mut self.replies[index];
        reply.blocks.extend(blocks);
        if usage.is_some() {
            reply.usage = usage;
        }
        if reply.model.is_none() {
            reply.model = model;
        }
    }

    /// The kind of the text of a `user` record, from its flags and the records before it.
    fn user_text_kind(&self, line: &Line, blocks: &[Value]) -> (PieceKind, Option<String>) {
        if Line::flag(line.is_compact_summary) {
            return (PieceKind::Summary, None);
        }
        if Line::flag(line.is_meta) {
            // Section 2.2 and RUN:skill: the body of a skill follows the Skill call's result and
            // names it with `sourceToolUseID`.
            let skill = get::<String>(line.source_tool_use_id)
                .and_then(|id| self.tool_results.get(&id))
                .and_then(|result| result.command_name.clone());
            return match skill {
                Some(name) => (PieceKind::ExtensionBody, Some(name)),
                None => (PieceKind::Reminder, None),
            };
        }
        let first = blocks.iter().find_map(text_of).unwrap_or("");
        if COMMAND_TAGS
            .iter()
            .any(|tag| first.trim_start().starts_with(tag))
        {
            return (PieceKind::Command, None);
        }
        (PieceKind::UserText, None)
    }

    /// Section 11: an attachment's rendered pieces are what a later blob holds.
    fn attachment(&mut self, line: &Line) {
        let Some(rendered) = get::<Vec<Rendered>>(line.rendered) else {
            return;
        };
        let rendered: Vec<String> = rendered.into_iter().filter_map(|r| r.content).collect();
        let system = get::<String>(line.rendered_role).as_deref() == Some("system");
        let attachment = line
            .attachment
            .and_then(|raw| serde_json::from_str::<Attachment>(raw.get()).ok());
        let sources = match attachment {
            Some(attachment) => {
                let nested = (attachment.ty.as_deref() == Some("nested_memory"))
                    .then(|| get::<NestedContent>(attachment.content))
                    .flatten()
                    .and_then(|c| c.content);
                label::attachment_sources(&attachment, nested, rendered, system)
            }
            None => rendered
                .into_iter()
                .map(|text| Source::new(text, PieceKind::Other, None))
                .collect(),
        };
        self.add_sources(sources);
    }

    fn add_sources(&mut self, sources: Vec<Source>) {
        let turn = self.requests_read;
        self.source_turns
            .extend(std::iter::repeat_n(turn, sources.len()));
        self.sources.extend(sources);
    }

    /// Section 8: a compaction boundary.
    fn system(&mut self, no: usize, line: &Line) {
        if get::<String>(line.subtype).as_deref() != Some("compact_boundary") {
            return;
        }
        let meta = get::<CompactMetadata>(line.compact_metadata);
        self.boundaries.push((
            no,
            Boundary {
                before: 0,
                pre_tokens: meta.as_ref().and_then(|m| m.pre_tokens),
                post_tokens: meta.as_ref().and_then(|m| m.post_tokens),
            },
        ));
    }

    /// Section 3.1: the system blocks and the tool definitions.
    fn shape(&mut self, line: &Line) {
        let Some(hash) = get::<String>(line.shape_hash).and_then(|h| canon::parse_hash(&h)) else {
            return;
        };
        if self.shapes.contains_key(&hash) {
            return;
        }
        let Some(Value::Object(shape)) = get::<Value>(line.shape) else {
            return;
        };
        let mut out = Shape::default();
        if let Some(Value::Array(system)) = shape.get("system") {
            let ids = system
                .iter()
                .map(|block| self.definition(Origin::System, block))
                .collect();
            out.system = Some(ids);
        }
        if let Some(Value::Array(tools)) = shape.get("tools") {
            let ids = tools
                .iter()
                .map(|tool| self.definition(Origin::Tool, tool))
                .collect();
            out.tools = Some(ids);
        }
        self.shapes.insert(hash, out);
    }

    /// A system block or a tool definition. The shape is used as recorded, so its canonical
    /// JSON keeps any `cache_control` it holds (4.1).
    fn definition(&mut self, origin: Origin, value: &Value) -> BlockId {
        let canon = canon::text(value, false);
        let hash = canon::sha256(&canon);
        if let Some(id) = self.blocks.find(origin, &hash) {
            return id;
        }
        let block = match origin {
            Origin::Tool => Block::new(
                vec![
                    Span::new(PieceKind::ToolDefinition, json_text(value))
                        .labelled(str_at(value, "name").map(str::to_owned)),
                ],
                PieceKind::ToolDefinition,
            ),
            _ => Block::new(
                vec![Span::new(
                    PieceKind::System,
                    text_of(value).map_or_else(|| json_text(value), str::to_owned),
                )],
                PieceKind::System,
            ),
        };
        self.blocks.add(origin, hash, &canon, block)
    }

    /// Section 3.1: one message that the conversation records do not hold.
    fn blob(&mut self, line: &Line) {
        let Some(hash) = get::<String>(line.hash).and_then(|h| canon::parse_hash(&h)) else {
            return;
        };
        if self.blobs.contains_key(&hash) {
            return;
        }
        let Some(Value::Object(mut message)) = get::<Value>(line.message) else {
            return;
        };
        let role = match message.get("role") {
            Some(Value::String(role)) => role.clone(),
            _ => String::new(),
        };
        let content = message.remove("content").unwrap_or(Value::Null);
        let mut keys: Vec<&String> = message.keys().collect();
        keys.sort_by(|a, b| canon::utf16_order(a, b));
        let mut before = vec![b'{'];
        let mut after = Vec::new();
        for key in keys {
            let mut field = Vec::new();
            canon::write_str(&mut field, key);
            field.push(b':');
            canon::write(&mut field, &message[key], true);
            if canon::utf16_order(key, "content").is_lt() {
                before.extend_from_slice(&field);
                before.push(b',');
            } else {
                after.push(b',');
                after.extend_from_slice(&field);
            }
        }
        after.push(b'}');
        let mut ids = Vec::new();
        for block in as_blocks(content) {
            let canon = canon::text(&block, true);
            let text = text_of(&block).filter(|_| role != "assistant");
            let Some(text) = text else {
                let other = (PieceKind::Other, None);
                ids.push(self.intern(Origin::Blob, &block, &canon, &role, &other));
                continue;
            };
            let hash = canon::sha256(&canon);
            let id = match self.blocks.find(Origin::Blob, &hash) {
                Some(id) => id,
                None => {
                    let whole =
                        Block::new(vec![Span::new(PieceKind::Other, text)], PieceKind::Other);
                    let id = self.blocks.add(Origin::Blob, hash, &canon, whole);
                    self.deferred.push((id, self.requests_read));
                    id
                }
            };
            ids.push(id);
        }
        self.blobs.insert(
            hash,
            Blob {
                role,
                blocks: ids,
                before,
                after,
            },
        );
    }

    /// Section 3.1: one request.
    fn request(&mut self, no: usize, line: &Line) {
        self.requests_read += 1;
        let Some(id) = get::<String>(line.id) else {
            return;
        };
        let tail = get::<Vec<Value>>(line.tail)
            .unwrap_or_default()
            .iter()
            .map(|h| h.as_str().and_then(canon::parse_hash))
            .collect();
        let rec = RequestRec {
            id: id.clone(),
            line: no,
            at: get(line.timestamp),
            shape: get::<String>(line.shape_hash).and_then(|h| canon::parse_hash(&h)),
            params: get(line.params).unwrap_or_default(),
            base: get(line.base),
            keep: get::<u64>(line.keep).unwrap_or(0) as usize,
            tail,
            marks: get(line.cache_control).unwrap_or_default(),
            digest: get::<String>(line.request_digest).and_then(|h| canon::parse_hash(&h)),
        };
        self.request_index.insert(id, self.requests.len());
        self.requests.push(rec);
    }

    /// The block's id, storing it with its spans the first time its JSON is seen here.
    fn intern(
        &mut self,
        origin: Origin,
        block: &Value,
        canon: &[u8],
        role: &str,
        text_as: &(PieceKind, Option<String>),
    ) -> BlockId {
        let hash = canon::sha256(canon);
        if let Some(id) = self.blocks.find(origin, &hash) {
            return id;
        }
        let (spans, kind) = self.spans(block, role, text_as);
        self.blocks
            .add(origin, hash, canon, Block::new(spans, kind))
    }

    /// The spans of one content block (design section 14, "A kind comes from the record").
    fn spans(
        &self,
        block: &Value,
        role: &str,
        text_as: &(PieceKind, Option<String>),
    ) -> (Vec<Span>, PieceKind) {
        let ty = str_at(block, "type").unwrap_or("");
        match ty {
            "text" => {
                let text = text_of(block).unwrap_or("");
                let (kind, label) = match role {
                    "assistant" => (PieceKind::ModelText, None),
                    _ => text_as.clone(),
                };
                (vec![Span::new(kind, text).labelled(label)], kind)
            }
            // The record keeps a thinking block's signature, and its text only when the request
            // asked to display it (session spec, section 6).
            "thinking" | "redacted_thinking" => {
                let text = str_at(block, "thinking").unwrap_or("");
                let opaque = str_at(block, "signature")
                    .or_else(|| str_at(block, "data"))
                    .map_or(0, str::len);
                (
                    vec![Span::new(PieceKind::Thinking, text).with_opaque(opaque as u64)],
                    PieceKind::Thinking,
                )
            }
            _ if is_tool_call(block) => {
                let input = block.get("input").map_or_else(String::new, json_text);
                let name = str_at(block, "name").map(str::to_owned);
                (
                    vec![Span::new(PieceKind::ToolCall, input).labelled(name)],
                    PieceKind::ToolCall,
                )
            }
            _ if ty == "tool_result" || ty.ends_with("_tool_result") => {
                let id = str_at(block, "tool_use_id").unwrap_or("");
                let name = self.tool_names.get(id).cloned();
                let file = self
                    .tool_results
                    .get(id)
                    .and_then(|r| r.persisted_output_path.as_deref())
                    .and_then(|p| AbsPath::parse(p).ok());
                let text = block.get("content").map_or_else(String::new, result_text);
                (
                    vec![
                        Span::new(PieceKind::ToolResult, text)
                            .labelled(name)
                            .from_file(file),
                    ],
                    PieceKind::ToolResult,
                )
            }
            _ => (Vec::new(), PieceKind::Other),
        }
    }

    /// The spans of every blob text block (session spec, section 11): where the texts of the
    /// records around it occur in it. The request record is written on its own schedule (3.5),
    /// so a turn's records can come after its blobs as well as before them. A blob's sources are
    /// the records from the request before its own to the request after it, and each record
    /// explains one place, the first blob in record order that holds it.
    fn label_blobs(&mut self) {
        let mut used = vec![false; self.sources.len()];
        for &(id, turn) in &self.deferred {
            let start = self.source_turns.partition_point(|&t| t + 1 < turn);
            let end = self.source_turns.partition_point(|&t| t <= turn + 1);
            let candidates: Vec<usize> = (start..end).filter(|&i| !used[i]).collect();
            let near: Vec<&Source> = candidates.iter().map(|&i| &self.sources[i]).collect();
            let block = &mut self.blocks.blocks[id as usize];
            let text = std::mem::take(&mut block.spans[0].text);
            let (spans, chosen) = label::split(&text, &near);
            for n in chosen {
                used[candidates[n]] = true;
            }
            *block = Block::new(spans, PieceKind::Other);
        }
        self.sources = Vec::new();
        self.source_turns = Vec::new();
    }

    /// Phase two: label the blobs, rebuild every request, pair replies with requests, order the
    /// steps.
    fn finish(mut self) -> (Vec<Block>, Vec<Message>, Vec<StepData>, Vec<Boundary>) {
        self.label_blobs();
        let mut built = Built::new(&self);
        let lists = built.message_lists();
        let mut steps: Vec<(usize, StepData)> = Vec::new();
        let mut answered = vec![false; self.replies.len()];
        for (rec, (messages, complete)) in self.requests.iter().zip(lists) {
            let (marks, digest) = built.digest(rec, &messages);
            let exact = complete && digest.is_some() && digest == rec.digest;
            let shape = rec.shape.and_then(|h| self.shapes.get(&h));
            let request = Request {
                exact,
                system: shape.and_then(|s| s.system.clone()).unwrap_or_default(),
                tools: shape.and_then(|s| s.tools.clone()).unwrap_or_default(),
                messages,
                marks,
            };
            let reply = self
                .reply_index
                .get(&ReplyKey::Request(rec.id.clone()))
                .copied();
            let mut step = StepData {
                at: rec.at.clone().unwrap_or_default(),
                model: rec
                    .params
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                usage: None,
                sent: Some(request),
                reply: Vec::new(),
            };
            let mut line = rec.line;
            if let Some(r) = reply {
                answered[r] = true;
                let reply = &self.replies[r];
                line = line.min(reply.line);
                step.usage = reply.usage.clone();
                step.reply = reply.blocks.clone();
                if step.model.is_none() {
                    step.model = reply.model.clone();
                }
                if step.at.is_empty() {
                    step.at = reply.at.clone().unwrap_or_default();
                }
            }
            steps.push((line, step));
        }
        for (r, reply) in self.replies.iter().enumerate() {
            if answered[r] {
                continue;
            }
            steps.push((
                reply.line,
                StepData {
                    at: reply.at.clone().unwrap_or_default(),
                    model: reply.model.clone(),
                    usage: reply.usage.clone(),
                    sent: None,
                    reply: reply.blocks.clone(),
                },
            ));
        }
        steps.sort_by_key(|(line, _)| *line);
        let boundaries = self
            .boundaries
            .iter()
            .map(|(line, boundary)| Boundary {
                before: steps.partition_point(|(at, _)| at < line) as u32,
                ..boundary.clone()
            })
            .collect();
        let messages = built.messages;
        let steps = steps.into_iter().map(|(_, step)| step).collect();
        (self.blocks.blocks, messages, steps, boundaries)
    }
}

/// The messages that requests name, each once, and what the digest needs to write them.
struct Built<'a> {
    reader: &'a Reader,
    messages: Vec<Message>,
    /// Per message: the blocks and the canonical JSON around them.
    frames: Vec<(Vec<u8>, Vec<u8>)>,
    by_hash: HashMap<Hash, Option<MessageId>>,
    buffer: Vec<u8>,
}

impl<'a> Built<'a> {
    fn new(reader: &'a Reader) -> Built<'a> {
        Built {
            reader,
            messages: Vec::new(),
            frames: Vec::new(),
            by_hash: HashMap::new(),
            buffer: Vec::new(),
        }
    }

    /// Section 4.1, `resolve`: a blob first, then a prefix of a run.
    fn resolve(&mut self, hash: &Hash) -> Option<MessageId> {
        if let Some(found) = self.by_hash.get(hash) {
            return *found;
        }
        let reader = self.reader;
        let found = if let Some(blob) = reader.blobs.get(hash) {
            Some((
                Message {
                    role: role_of(&blob.role),
                    blocks: blob.blocks.clone(),
                },
                (blob.before.clone(), blob.after.clone()),
            ))
        } else {
            reader.run_prefixes.get(hash).map(|&(run, len)| {
                let run = &reader.runs[run as usize];
                let mut after = b",\"role\":".to_vec();
                canon::write_str(&mut after, &run.role);
                after.push(b'}');
                (
                    Message {
                        role: role_of(&run.role),
                        blocks: run.blocks[..len as usize].to_vec(),
                    },
                    (b"{".to_vec(), after),
                )
            })
        };
        let id = found.map(|(message, frame)| {
            self.messages.push(message);
            self.frames.push(frame);
            self.messages.len() as MessageId - 1
        });
        self.by_hash.insert(*hash, id);
        id
    }

    /// Section 4.3: each request's messages, the first `keep` of its base's and then `tail`,
    /// with whether every one of them resolved.
    fn message_lists(&mut self) -> Vec<(Vec<Option<MessageId>>, bool)> {
        let reader = self.reader;
        let requests = &reader.requests;
        let mut lists: Vec<Option<(Vec<Option<MessageId>>, bool)>> = vec![None; requests.len()];
        let mut visiting = vec![false; requests.len()];
        for start in 0..requests.len() {
            let mut stack = vec![start];
            while let Some(&i) = stack.last() {
                if lists[i].is_some() {
                    stack.pop();
                    continue;
                }
                let rec = &requests[i];
                let base = rec.base.as_ref().map(|b| reader.request_index.get(b));
                let prefix = match base {
                    None => Some((Vec::new(), true)),
                    Some(Some(&b)) if lists[b].is_some() => {
                        let (list, complete) = lists[b].as_ref().expect("checked");
                        let mut kept: Vec<Option<MessageId>> =
                            list.iter().take(rec.keep).copied().collect();
                        let short = kept.len() < rec.keep;
                        kept.resize(rec.keep, None);
                        Some((kept, *complete && !short))
                    }
                    Some(Some(&b)) if !visiting[b] => {
                        visiting[i] = true;
                        stack.push(b);
                        None
                    }
                    // A base that is not in this file, or a chain that loops.
                    _ => Some((vec![None; rec.keep], false)),
                };
                let Some((mut list, mut complete)) = prefix else {
                    continue;
                };
                for hash in &rec.tail {
                    let id = hash.as_ref().and_then(|h| self.resolve(h));
                    complete &= id.is_some();
                    list.push(id);
                }
                lists[i] = Some((list, complete));
                stack.pop();
            }
        }
        lists
            .into_iter()
            .map(|l| l.expect("every list built"))
            .collect()
    }

    /// Section 3.3: the cache marks of the request, and the digest of its rebuilt body, or
    /// `None` when the body cannot be rebuilt.
    fn digest(
        &mut self,
        rec: &RequestRec,
        messages: &[Option<MessageId>],
    ) -> (Vec<MarkAt>, Option<Hash>) {
        let mut marks: HashMap<MarkAt, &Value> = HashMap::new();
        let mut placed = true;
        for mark in &rec.marks {
            match mark_at(&mark.path) {
                Some(at) => {
                    marks.insert(at, &mark.value);
                }
                None => placed = false,
            }
        }
        let mut order: Vec<MarkAt> = marks.keys().copied().collect();
        order.sort_by_key(|m| match *m {
            MarkAt::System { block } => (0, block, 0),
            MarkAt::Tool { index } => (1, index, 0),
            MarkAt::Message { message, block } => (2, message, block),
        });
        let reader = self.reader;
        let shape = rec.shape.and_then(|h| reader.shapes.get(&h));
        if !placed || shape.is_none() || messages.iter().any(Option::is_none) {
            return (order, None);
        }
        let shape = shape.expect("checked");
        let digest = self.body_digest(rec, shape, messages, &marks);
        (order, digest)
    }

    fn body_digest(
        &mut self,
        rec: &RequestRec,
        shape: &Shape,
        messages: &[Option<MessageId>],
        marks: &HashMap<MarkAt, &Value>,
    ) -> Option<Hash> {
        enum Part<'v> {
            Param(&'v Value),
            System(&'v [BlockId]),
            Tools(&'v [BlockId]),
            Messages,
        }
        let mut parts: Vec<(&str, Part)> = rec
            .params
            .iter()
            .filter(|(k, _)| !UNRECORDED.contains(&k.as_str()))
            .filter(|(k, _)| match k.as_str() {
                "system" => shape.system.is_none(),
                "tools" => shape.tools.is_none(),
                "messages" => false,
                _ => true,
            })
            .map(|(k, v)| (k.as_str(), Part::Param(v)))
            .collect();
        if let Some(system) = &shape.system {
            parts.push(("system", Part::System(system)));
        }
        if let Some(tools) = &shape.tools {
            parts.push(("tools", Part::Tools(tools)));
        }
        parts.push(("messages", Part::Messages));
        parts.sort_by(|a, b| canon::utf16_order(a.0, b.0));

        let reader = self.reader;
        let blocks = &reader.blocks;
        let mut out = std::mem::take(&mut self.buffer);
        out.clear();
        let mut applied = 0;
        let mut ok = true;
        out.push(b'{');
        for (i, (key, part)) in parts.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            canon::write_str(&mut out, key);
            out.push(b':');
            let mut list = |out: &mut Vec<u8>, ids: &[BlockId], at: &dyn Fn(u32) -> MarkAt| {
                out.push(b'[');
                for (n, &id) in ids.iter().enumerate() {
                    if n > 0 {
                        out.push(b',');
                    }
                    match marks.get(&at(n as u32)) {
                        Some(mark) => {
                            applied += 1;
                            ok &= write_marked(out, blocks.canon(id), mark);
                        }
                        None => out.extend_from_slice(blocks.canon(id)),
                    }
                }
                out.push(b']');
            };
            match part {
                Part::Param(value) => canon::write(&mut out, value, false),
                Part::System(ids) => list(&mut out, ids, &|block| MarkAt::System { block }),
                Part::Tools(ids) => list(&mut out, ids, &|index| MarkAt::Tool { index }),
                Part::Messages => {
                    out.push(b'[');
                    for (n, message) in messages.iter().enumerate() {
                        if n > 0 {
                            out.push(b',');
                        }
                        let id = message.expect("checked") as usize;
                        let (before, after) = &self.frames[id];
                        out.extend_from_slice(before);
                        out.extend_from_slice(b"\"content\":");
                        let n = n as u32;
                        list(&mut out, &self.messages[id].blocks, &|block| {
                            MarkAt::Message { message: n, block }
                        });
                        out.extend_from_slice(after);
                    }
                    out.push(b']');
                }
            }
        }
        out.push(b'}');
        let digest = canon::sha256(&out);
        self.buffer = out;
        (ok && applied == marks.len()).then_some(digest)
    }
}

/// A block with `cache_control` set to `mark`, as canonical JSON.
fn write_marked(out: &mut Vec<u8>, canon: &[u8], mark: &Value) -> bool {
    let Ok(Value::Object(mut block)) = serde_json::from_slice::<Value>(canon) else {
        return false;
    };
    block.insert("cache_control".to_owned(), mark.clone());
    canon::write(out, &Value::Object(block), false);
    true
}

/// Section 5: `["system", i]`, `["tools", i]` or `["messages", n, "content", m]`.
fn mark_at(path: &[Value]) -> Option<MarkAt> {
    let index = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    match path {
        [Value::String(s), i] if s == "system" => Some(MarkAt::System { block: index(i)? }),
        [Value::String(s), i] if s == "tools" => Some(MarkAt::Tool { index: index(i)? }),
        [Value::String(s), n, Value::String(c), m] if s == "messages" && c == "content" => {
            Some(MarkAt::Message {
                message: index(n)?,
                block: index(m)?,
            })
        }
        _ => None,
    }
}

/// Section 3.2: string content counts as one text block; any other value as one block.
fn as_blocks(content: Value) -> Vec<Value> {
    match content {
        Value::String(text) => {
            let mut block = Map::new();
            block.insert("type".to_owned(), Value::String("text".to_owned()));
            block.insert("text".to_owned(), Value::String(text));
            vec![Value::Object(block)]
        }
        Value::Array(blocks) => blocks,
        other => vec![other],
    }
}

fn role_of(role: &str) -> Role {
    match role {
        "assistant" => Role::Model,
        "system" => Role::System,
        "developer" => Role::Developer,
        _ => Role::User,
    }
}

fn str_at<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

/// The text of a `text` block.
fn text_of(block: &Value) -> Option<&str> {
    (str_at(block, "type") == Some("text"))
        .then(|| str_at(block, "text"))
        .flatten()
}

fn is_tool_call(block: &Value) -> bool {
    matches!(
        str_at(block, "type"),
        Some("tool_use" | "server_tool_use" | "mcp_tool_use")
    )
}

/// A tool result's content as text: a string as it is, a list of blocks as their texts.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(text_of)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => json_text(other),
    }
}

/// JSON as the record holds it, keys in their recorded order.
fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
