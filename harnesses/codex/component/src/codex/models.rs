//! Codex's conversation model, vendored from openai/codex 0.162.0 (c1382380),
//! Apache-2.0: `codex-rs/protocol/src/models.rs`, trimmed to the items a
//! host-tools agent produces.
//!
//! The JSON is Codex's own Responses-API item shape (`{"type": "message", ...}`,
//! `function_call`, `function_call_output`, `reasoning`), so a saved history
//! reads like a Codex rollout. What is left out: the item kinds only Codex's
//! own tools or OpenAI's server tools produce (`local_shell_call`,
//! `custom_tool_call`, `tool_search_*`, `web_search_call`,
//! `image_generation_call`, `compaction`, the multi-agent and configuration
//! items), audio, file-id images, and the internal metadata passthrough.
//!
//! What is added, all `x_cosmonic`-prefixed or optional so a Codex reader
//! skips them: the host's per-part continuation data (`assistant-part.opaque`)
//! and the sources a provider cited (`assistant-part.citations`) on messages
//! and function calls, and an item for a provider block the host passes
//! through unmodelled (`assistant-content.other`). A model's refusal is the
//! Responses API's own `refusal` content part. Reasoning
//! continuation rides in Codex's own `encrypted_content`, which is exactly what
//! it is for upstream: data the client returns verbatim and never reads.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    /// Upstream flattens an `ImageReference`; only the inline (data URL or
    /// remote URL) case is kept.
    InputImage { image_url: String },
    OutputText { text: String },
    /// The model declined; the Responses API's `refusal` output part.
    Refusal { refusal: String },
}

/// Cosmonic: a source the provider attached to reply text
/// (`inference-types.citation`). Untrusted provider output for display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Citation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemReasoningSummary {
    SummaryText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningItemContent {
    ReasoningText { text: String },
    Text { text: String },
}

/// The subset of content a function call output may carry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FunctionCallOutputBody {
    Text(String),
    ContentItems(Vec<FunctionCallOutputContentItem>),
}

/// Upstream serializes only `body` on the wire and keeps `success` in memory.
/// This is a state format, not a wire format, so both are stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCallOutputPayload {
    pub body: FunctionCallOutputBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

impl FunctionCallOutputPayload {
    pub fn from_text(text: String) -> Self {
        Self { body: FunctionCallOutputBody::Text(text), success: None }
    }

    /// Codex's `text_content`: the text of the output, ignoring media.
    pub fn text(&self) -> String {
        match &self.body {
            FunctionCallOutputBody::Text(text) => text.clone(),
            FunctionCallOutputBody::ContentItems(items) => items
                .iter()
                .filter_map(|item| match item {
                    FunctionCallOutputContentItem::InputText { text } => Some(text.as_str()),
                    FunctionCallOutputContentItem::InputImage { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
        /// Cosmonic: the host's continuation data for this part.
        #[serde(default, rename = "x_cosmonic_opaque", skip_serializing_if = "Option::is_none")]
        opaque: Option<String>,
        /// Cosmonic: the sources the provider cited for this part's text.
        #[serde(default, rename = "x_cosmonic_citations", skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        summary: Vec<ReasoningItemReasoningSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningItemContent>>,
        /// Upstream: the provider's encrypted reasoning, replayed verbatim.
        /// Here: the host's `assistant-part.opaque` for the reasoning part,
        /// replayed byte for byte, which is the same contract.
        encrypted_content: Option<String>,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        /// A string holding JSON, as the Responses API returns it, or a
        /// freeform tool's raw text.
        arguments: String,
        call_id: String,
        /// Cosmonic: the host's continuation data for this part.
        #[serde(default, rename = "x_cosmonic_opaque", skip_serializing_if = "Option::is_none")]
        opaque: Option<String>,
        /// Cosmonic: `arguments` is a freeform tool's raw text
        /// (`tool-arguments.text`), not JSON. Upstream has a separate custom
        /// tool call item for this.
        #[serde(default, rename = "x_cosmonic_freeform", skip_serializing_if = "std::ops::Not::not")]
        freeform: bool,
    },
    FunctionCallOutput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        output: FunctionCallOutputPayload,
    },
    /// Cosmonic: a provider block the host passes through without modelling it
    /// (`assistant-content.other`), returned unmodified on the next request.
    #[serde(rename = "x_cosmonic_block")]
    HostBlock {
        kind: String,
        json: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opaque: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl ResponseItem {
    pub fn user_text(text: impl Into<String>) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: text.into() }],
            opaque: None,
            citations: Vec::new(),
        }
    }

    /// Whether the model produced this item (as opposed to the person or a tool).
    pub fn is_model_generated(&self) -> bool {
        match self {
            ResponseItem::Message { role, .. } => role == "assistant",
            ResponseItem::Reasoning { .. } | ResponseItem::FunctionCall { .. } | ResponseItem::HostBlock { .. } => {
                true
            }
            ResponseItem::FunctionCallOutput { .. } | ResponseItem::Other => false,
        }
    }
}

/// The text of a message's content parts, joined (Codex's `content_items_to_text`).
pub fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text.as_str()),
            ContentItem::InputImage { .. } | ContentItem::Refusal { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_codex_rollout_items() {
        let items: Vec<ResponseItem> = serde_json::from_value(json!([
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "thinking"}], "encrypted_content": "gAAA"},
            {"type": "function_call", "name": "echo", "arguments": "{}", "call_id": "c1"},
            {"type": "function_call_output", "call_id": "c1", "output": {"body": "ok"}},
            {"type": "local_shell_call", "status": "completed"},
        ]))
        .unwrap();
        assert_eq!(items.len(), 5);
        assert!(matches!(items[4], ResponseItem::Other));
        assert!(matches!(&items[1], ResponseItem::Reasoning { encrypted_content: Some(e), .. } if e == "gAAA"));
    }

    #[test]
    fn opaque_round_trips_and_is_omitted_when_absent() {
        let item = ResponseItem::FunctionCall {
            id: None,
            name: "echo".into(),
            arguments: "{}".into(),
            call_id: "c1".into(),
            opaque: Some("sig".into()),
            freeform: false,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["x_cosmonic_opaque"], "sig");
        assert_eq!(serde_json::from_value::<ResponseItem>(v).unwrap(), item);
        let plain = serde_json::to_value(ResponseItem::user_text("x")).unwrap();
        assert!(plain.get("x_cosmonic_opaque").is_none());
    }
}
