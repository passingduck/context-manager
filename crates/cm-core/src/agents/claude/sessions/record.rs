//! The fields of a transcript line that the reader uses (session spec, sections 2, 3, 6, 8, 11
//! and 12). Every struct names only the fields it needs, so the fields that identify an
//! account, an organisation or a host (section 15) are skipped by construction and never become
//! a value of any type.
//!
//! A line borrows each named field as raw JSON and parses it only when it is used, each on its
//! own: a field of an unexpected type loses that field, not the line.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;

/// One line of a transcript. A record type uses a few of these fields; the rest stay `None`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Line<'a> {
    #[serde(rename = "type", borrow)]
    pub ty: Option<&'a RawValue>,
    #[serde(borrow)]
    pub subtype: Option<&'a RawValue>,
    #[serde(borrow)]
    pub timestamp: Option<&'a RawValue>,
    #[serde(borrow)]
    pub cwd: Option<&'a RawValue>,

    // Conversation records (2.2).
    #[serde(borrow)]
    pub message: Option<&'a RawValue>,
    #[serde(borrow)]
    pub is_meta: Option<&'a RawValue>,
    #[serde(borrow)]
    pub is_compact_summary: Option<&'a RawValue>,
    #[serde(rename = "sourceToolUseID", borrow)]
    pub source_tool_use_id: Option<&'a RawValue>,
    #[serde(borrow)]
    pub request_ref: Option<&'a RawValue>,
    #[serde(borrow)]
    pub tool_use_result: Option<&'a RawValue>,
    #[serde(borrow)]
    pub attachment: Option<&'a RawValue>,
    #[serde(borrow)]
    pub rendered: Option<&'a RawValue>,
    #[serde(borrow)]
    pub rendered_role: Option<&'a RawValue>,
    #[serde(borrow)]
    pub compact_metadata: Option<&'a RawValue>,

    // Request records (3.1).
    #[serde(borrow)]
    pub id: Option<&'a RawValue>,
    #[serde(borrow)]
    pub shape_hash: Option<&'a RawValue>,
    #[serde(borrow)]
    pub shape: Option<&'a RawValue>,
    #[serde(borrow)]
    pub hash: Option<&'a RawValue>,
    #[serde(borrow)]
    pub params: Option<&'a RawValue>,
    #[serde(borrow)]
    pub base: Option<&'a RawValue>,
    #[serde(borrow)]
    pub keep: Option<&'a RawValue>,
    #[serde(borrow)]
    pub tail: Option<&'a RawValue>,
    #[serde(borrow)]
    pub cache_control: Option<&'a RawValue>,
    #[serde(borrow)]
    pub request_digest: Option<&'a RawValue>,

    // Session metadata for the list (12).
    #[serde(borrow)]
    pub custom_title: Option<&'a RawValue>,
    #[serde(borrow)]
    pub ai_title: Option<&'a RawValue>,
    #[serde(borrow)]
    pub summary: Option<&'a RawValue>,
}

/// The field parsed as `T`, or `None` when it is absent or of another type.
pub(super) fn get<T: DeserializeOwned>(raw: Option<&RawValue>) -> Option<T> {
    serde_json::from_str(raw?.get()).ok()
}

impl Line<'_> {
    pub fn ty(&self) -> Option<String> {
        get(self.ty)
    }

    pub fn flag(raw: Option<&RawValue>) -> bool {
        get::<bool>(raw).unwrap_or(false)
    }
}

#[derive(Deserialize)]
pub(super) struct Rendered {
    pub content: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompactMetadata {
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

/// One entry of `cacheControl`: where the body carried a mark and its value (section 5).
#[derive(Deserialize)]
pub(super) struct Mark {
    pub path: Vec<Value>,
    pub value: Value,
}

/// An API message as a conversation record or a blob holds it. Every field is a loose value: a
/// record whose role is not a string ends a run instead of failing to parse (4.2).
#[derive(Deserialize)]
pub(super) struct MessageRec {
    pub role: Option<Value>,
    pub content: Option<Value>,
    pub id: Option<Value>,
    pub model: Option<Value>,
    pub usage: Option<Value>,
}

#[derive(Deserialize)]
pub(super) struct UsageRec {
    pub input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub output_tokens_details: Option<OutputDetails>,
}

#[derive(Deserialize)]
pub(super) struct OutputDetails {
    pub thinking_tokens: Option<u64>,
}

/// The two fields of a tool's structured result that label what the model received: the skill a
/// Skill call launched, and the file a large output was saved to (section 10).
#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolUseResult {
    pub command_name: Option<String>,
    pub persisted_output_path: Option<String>,
}

/// The fields of an attachment record's `attachment` that label its rendered text (section 11).
/// `session_context`, `credential_org` and `team_context` attachments hold identifying fields;
/// none is named here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Attachment<'a> {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    /// `file`, `compact_file_reference`, `edited_text_file`.
    pub filename: Option<String>,
    pub display_path: Option<String>,
    /// `nested_memory`.
    pub path: Option<String>,
    /// Hook results.
    pub hook_event: Option<String>,
    /// `skill_mention`.
    pub skill_name: Option<String>,
    /// `instructions`: one entry per instruction file.
    pub files: Option<Vec<Embedded>>,
    /// `invoked_skills`: one entry per skill body.
    pub skills: Option<Vec<Embedded>>,
    /// `nested_memory`: an object whose `content` is the file's text. Other types hold other
    /// shapes here, so it is parsed only for that type.
    #[serde(borrow)]
    pub content: Option<&'a RawValue>,
}

/// A text an attachment embeds whole in its rendered text, with where it came from.
#[derive(Deserialize)]
pub(super) struct Embedded {
    pub path: Option<String>,
    pub content: Option<String>,
    /// `instructions`: `User`, `Project` and the like.
    #[serde(rename = "type")]
    pub ty: Option<String>,
    /// `invoked_skills`.
    pub name: Option<String>,
}

/// `nested_memory`'s `content` object.
#[derive(Deserialize)]
pub(super) struct NestedContent {
    pub content: Option<String>,
}

/// The subagent's `meta.json` (section 9): its type and description. `teamName` is not named.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubagentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
}
