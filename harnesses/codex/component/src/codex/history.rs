//! Codex's conversation history, vendored from openai/codex 0.162.0
//! (c1382380), Apache-2.0, and trimmed:
//!
//! - `ContextManager::record_items`, `for_prompt`, `replace` and the token
//!   estimate from `codex-rs/core/src/context_manager/history.rs`;
//! - `ensure_call_outputs_present` and `remove_orphan_outputs` from
//!   `codex-rs/core/src/context_manager/normalize.rs`.
//!
//! Dropped: the item envelope and its harness metadata (turn ids, retained
//! sources, user-authorization tracking, the guardian review copy), image and
//! audio stripping by input modality, world-state and window bookkeeping. The
//! synthetic output ids upstream derives with a UUIDv5 are left out (ids are
//! optional on the items, and nothing here sends them to a model).

use std::collections::HashSet;

use super::models::{ContentItem, FunctionCallOutputPayload, ResponseItem};
use super::truncate::{
    approx_token_count, approx_tokens_from_byte_count, truncate_function_output_payload,
    with_serialization_allowance, TruncationPolicy,
};

#[derive(Debug, Clone, Default)]
pub struct ContextManager {
    items: Vec<ResponseItem>,
}

/// `is_api_message` upstream: system messages and unknown items never reach
/// the model.
fn is_api_message(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, .. } => role.as_str() != "system",
        ResponseItem::Other => false,
        _ => true,
    }
}

impl ContextManager {
    pub fn new(items: Vec<ResponseItem>) -> Self {
        Self { items }
    }

    pub fn raw_items(&self) -> &[ResponseItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Record items in order, truncating tool output to the model's policy
    /// plus the serialization allowance, as upstream does for live history.
    pub fn record_items<'a>(&mut self, items: impl IntoIterator<Item = &'a ResponseItem>, policy: TruncationPolicy) {
        for item in items {
            if !is_api_message(item) {
                continue;
            }
            let mut processed = item.clone();
            if let ResponseItem::FunctionCallOutput { output, .. } = &mut processed {
                truncate_function_output_payload(output, with_serialization_allowance(policy));
            }
            self.items.push(processed);
        }
    }

    /// The history as the next model request takes it: every call paired
    /// with an output, and no output without its call.
    pub fn for_prompt(&self) -> Vec<ResponseItem> {
        let mut items = self.items.clone();
        ensure_call_outputs_present(&mut items);
        remove_orphan_outputs(&mut items);
        items
    }

    pub fn replace(&mut self, items: Vec<ResponseItem>) {
        self.items = items;
    }

    /// Drop the oldest item; compaction does this when even the compaction
    /// request does not fit.
    pub fn remove_first_item(&mut self) {
        if !self.items.is_empty() {
            let removed = self.items.remove(0);
            // Removing a call without its output (or the reverse) would leave
            // a half pair for normalization to patch; take its partner too.
            remove_corresponding_for(&mut self.items, &removed);
        }
    }

    /// The coarse estimate upstream compares against the auto-compact limit:
    /// model-visible bytes over four, plus the base instructions.
    pub fn estimate_token_count(&self, base_instructions: &str) -> i64 {
        let base = i64::try_from(approx_token_count(base_instructions)).unwrap_or(i64::MAX);
        self.items.iter().map(estimate_item_token_count).fold(base, i64::saturating_add)
    }
}

pub fn estimate_item_token_count(item: &ResponseItem) -> i64 {
    let bytes = estimate_response_item_model_visible_bytes(item);
    if bytes <= 0 {
        return 0;
    }
    i64::try_from(approx_tokens_from_byte_count(usize::try_from(bytes).unwrap_or(usize::MAX))).unwrap_or(i64::MAX)
}

/// Upstream's resized-image estimate (7,373 bytes, about 1,844 tokens).
const RESIZED_IMAGE_BYTES_ESTIMATE: i64 = 7373;

fn text_bytes(text: &str) -> i64 {
    i64::try_from(text.len()).unwrap_or(i64::MAX)
}

fn estimate_reasoning_length(encoded_len: usize) -> usize {
    encoded_len.saturating_mul(3).checked_div(4).unwrap_or(0).saturating_sub(650)
}

fn estimate_response_item_model_visible_bytes(item: &ResponseItem) -> i64 {
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .map(|part| match part {
                ContentItem::InputText { text }
                | ContentItem::OutputText { text }
                | ContentItem::Refusal { refusal: text } => text_bytes(text),
                ContentItem::InputImage { .. } => RESIZED_IMAGE_BYTES_ESTIMATE,
            })
            .fold(0, i64::saturating_add),
        ResponseItem::Reasoning { encrypted_content: Some(content), .. } => {
            i64::try_from(estimate_reasoning_length(content.len())).unwrap_or(i64::MAX)
        }
        ResponseItem::Reasoning { summary, .. } => summary
            .iter()
            .map(|s| match s {
                super::models::ReasoningItemReasoningSummary::SummaryText { text } => text_bytes(text),
            })
            .fold(0, i64::saturating_add),
        ResponseItem::FunctionCall { name, arguments, .. } => text_bytes(name).saturating_add(text_bytes(arguments)),
        ResponseItem::FunctionCallOutput { output, .. } => payload_bytes(output),
        ResponseItem::HostBlock { json, .. } => text_bytes(json),
        ResponseItem::Other => 0,
    }
}

fn payload_bytes(output: &FunctionCallOutputPayload) -> i64 {
    use super::models::{FunctionCallOutputBody as B, FunctionCallOutputContentItem as C};
    match &output.body {
        B::Text(text) => text_bytes(text),
        B::ContentItems(items) => items
            .iter()
            .map(|item| match item {
                C::InputText { text } => text_bytes(text),
                C::InputImage { .. } => RESIZED_IMAGE_BYTES_ESTIMATE,
            })
            .fold(0, i64::saturating_add),
    }
}

// ----- codex-rs/core/src/context_manager/normalize.rs -----

/// Every function call gets an output, inserted right after it; upstream's
/// placeholder text is "aborted".
pub fn ensure_call_outputs_present(items: &mut Vec<ResponseItem>) {
    let output_ids: HashSet<String> = items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCallOutput { call_id: Some(call_id), .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    let mut missing: Vec<(usize, ResponseItem)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if let ResponseItem::FunctionCall { call_id, .. } = item {
            if !output_ids.contains(call_id) {
                missing.push((
                    idx,
                    ResponseItem::FunctionCallOutput {
                        id: None,
                        call_id: Some(call_id.clone()),
                        output: FunctionCallOutputPayload::from_text("aborted".to_string()),
                    },
                ));
            }
        }
    }
    // Insert in reverse so earlier indices stay valid.
    for (idx, output) in missing.into_iter().rev() {
        items.insert(idx + 1, output);
    }
}

/// An output whose call is gone is dropped (upstream also logs it, and
/// panics in debug builds).
pub fn remove_orphan_outputs(items: &mut Vec<ResponseItem>) {
    let call_ids: HashSet<String> = items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    items.retain(|item| match item {
        ResponseItem::FunctionCallOutput { call_id: Some(call_id), .. } => call_ids.contains(call_id),
        _ => true,
    });
}

/// Remove the first item that pairs with `item` (its output, or its call).
pub fn remove_corresponding_for(items: &mut Vec<ResponseItem>, item: &ResponseItem) {
    let position = match item {
        ResponseItem::FunctionCall { call_id, .. } => items.iter().position(
            |i| matches!(i, ResponseItem::FunctionCallOutput { call_id: Some(c), .. } if c == call_id),
        ),
        ResponseItem::FunctionCallOutput { call_id: Some(call_id), .. } => {
            items.iter().position(|i| matches!(i, ResponseItem::FunctionCall { call_id: c, .. } if c == call_id))
        }
        _ => None,
    };
    if let Some(p) = position {
        items.remove(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "t".into(),
            arguments: "{}".into(),
            call_id: id.into(),
            opaque: None,
            freeform: false,
        }
    }

    fn output(id: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some(id.into()),
            output: FunctionCallOutputPayload::from_text("ok".into()),
        }
    }

    #[test]
    fn missing_outputs_are_aborted_and_orphans_dropped() {
        let h = ContextManager::new(vec![call("a"), output("b"), ResponseItem::user_text("hi")]);
        let p = h.for_prompt();
        assert_eq!(p.len(), 3);
        assert!(matches!(&p[1], ResponseItem::FunctionCallOutput { output, .. } if output.text() == "aborted"));
        assert!(matches!(&p[2], ResponseItem::Message { .. }));
    }

    #[test]
    fn tool_output_is_truncated_on_record() {
        let mut h = ContextManager::default();
        let big = ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("a".into()),
            output: FunctionCallOutputPayload::from_text("x".repeat(50_000)),
        };
        h.record_items([&big], TruncationPolicy::Bytes(10_000));
        let ResponseItem::FunctionCallOutput { output, .. } = &h.raw_items()[0] else { panic!() };
        assert!(output.text().len() < 13_000);
    }

    #[test]
    fn removing_the_first_call_takes_its_output() {
        let mut h = ContextManager::new(vec![call("a"), output("a"), ResponseItem::user_text("hi")]);
        h.remove_first_item();
        assert_eq!(h.len(), 1);
    }
}
