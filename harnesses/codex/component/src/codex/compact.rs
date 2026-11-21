//! Codex's local ("inline") compaction, vendored from openai/codex 0.162.0
//! (c1382380), Apache-2.0: the prompts from
//! `codex-rs/prompts/templates/compact/`, and from `codex-rs/core/src/compact.rs`
//! `collect_user_messages`, `is_summary_message` and
//! `build_compacted_history_with_limit`; `get_last_assistant_message_from_turn`
//! from `codex-rs/core/src/session/turn.rs`.
//!
//! How it works upstream, and here: send the whole history plus
//! [`SUMMARIZATION_PROMPT`] as a user message, take the model's last reply as
//! the summary, then replace the history with the most recent user messages
//! (newest first, up to 20,000 estimated tokens) followed by one user message
//! holding [`SUMMARY_PREFIX`] and the summary. Assistant replies, tool calls
//! and tool output are not kept; the summary stands in for them.
//!
//! Dropped: remote compaction (OpenAI's `/responses/compact` endpoint and the
//! encrypted `compaction` item it returns), initial-context re-injection (this
//! agent keeps its instructions out of the history, so there is nothing to
//! re-inject), the harness metadata on retained messages.

use super::models::{content_text, ContentItem, ResponseItem};
use super::truncate::{approx_token_count, truncate_text, TruncationPolicy};

pub const SUMMARIZATION_PROMPT: &str = include_str!("prompts/compact_prompt.md");
pub const SUMMARY_PREFIX: &str = include_str!("prompts/summary_prefix.md");
pub const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;
/// Added to the summarization prompt when the person said what the summary
/// should keep (`compact`'s `focus`), which follows as their own message. Not
/// upstream's: Codex's `/compact` takes no such text.
pub const FOCUS_NOTE: &str =
    "The person asked for this summary. Their message, next, says what it should keep; keep that.";

pub fn is_summary_message(message: &str) -> bool {
    message.starts_with(format!("{SUMMARY_PREFIX}\n").as_str())
}

/// The person's messages, oldest first, without earlier summaries.
pub fn collect_user_messages(items: &[ResponseItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let text = content_text(content);
                (!is_summary_message(&text)).then_some(text)
            }
            _ => None,
        })
        .collect()
}

pub fn build_compacted_history(user_messages: &[String], summary_text: &str) -> Vec<ResponseItem> {
    build_compacted_history_with_limit(user_messages, summary_text, COMPACT_USER_MESSAGE_MAX_TOKENS)
}

pub fn build_compacted_history_with_limit(
    user_messages: &[String],
    summary_text: &str,
    max_tokens: usize,
) -> Vec<ResponseItem> {
    let mut selected = Vec::new();
    if max_tokens > 0 {
        let mut remaining = max_tokens;
        for message in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let tokens = approx_token_count(message);
            if tokens <= remaining {
                selected.push(message.clone());
                remaining = remaining.saturating_sub(tokens);
            } else {
                selected.push(truncate_text(message, TruncationPolicy::Tokens(remaining)));
                break;
            }
        }
        selected.reverse();
    }
    let mut history: Vec<ResponseItem> = selected.into_iter().map(ResponseItem::user_text).collect();
    let summary_text = if summary_text.is_empty() { "(no summary available)".to_string() } else { summary_text.to_string() };
    history.push(ResponseItem::user_text(summary_text));
    history
}

/// The text of the last assistant message among `items`.
pub fn get_last_assistant_message_from_turn<'a>(items: impl DoubleEndedIterator<Item = &'a ResponseItem>) -> Option<String> {
    items.rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. } if role == "assistant" => {
            let text = content_text(content);
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    })
}

/// All assistant text in `items` after the last user message: one model
/// reply may be several text parts, each its own message item here.
pub fn assistant_text_since_last_user(items: &[ResponseItem]) -> String {
    let start = items
        .iter()
        .rposition(|i| matches!(i, ResponseItem::Message { role, .. } if role == "user"))
        .map_or(0, |p| p + 1);
    let mut out = String::new();
    for item in &items[start..] {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "assistant" {
                for c in content {
                    if let ContentItem::OutputText { text } = c {
                        out.push_str(text);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_user_messages_then_the_summary() {
        let msgs = vec!["old".to_string(), "x".repeat(100), "new".to_string()];
        let h = build_compacted_history_with_limit(&msgs, &format!("{SUMMARY_PREFIX}\nsum"), 10);
        // "new" (1 token) fits, the 100-byte message is truncated to fit, "old" is dropped.
        assert_eq!(h.len(), 3);
        let ResponseItem::Message { content, .. } = h.last().unwrap() else { panic!() };
        assert!(is_summary_message(&content_text(content)));
    }

    #[test]
    fn summaries_are_not_collected_as_user_messages() {
        let items = vec![
            ResponseItem::user_text("hi"),
            ResponseItem::user_text(format!("{SUMMARY_PREFIX}\nearlier")),
        ];
        assert_eq!(collect_user_messages(&items), vec!["hi".to_string()]);
    }
}
