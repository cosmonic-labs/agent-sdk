//! Vendored from openai/codex 0.162.0 (c1382380), Apache-2.0:
//! `codex-rs/utils/string/src/truncate.rs`, unchanged but for this header and
//! the removed test module, plus `TruncationPolicy` and the function-output
//! truncation from `codex-rs/utils/output-truncation/src/lib.rs` and
//! `codex-rs/protocol/src/protocol.rs`, trimmed to text and images.

use super::models::{FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload};

const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Retain at most `max_bytes` from a string's ends and add a character-count
/// marker outside that budget.
///
/// Use [`truncate_middle_with_marker`] when the complete result must fit a hard
/// byte limit.
pub fn truncate_middle_chars(s: &str, max_bytes: usize) -> String {
    truncate_with_byte_estimate(s, max_bytes, /*use_tokens*/ false)
}

/// Truncate the middle of a UTF-8 string so the result, including `marker`, is
/// at most `max_bytes` bytes.
///
/// Returns the original string when it already fits. If the marker itself does
/// not fit, returns the longest original prefix that does.
pub fn truncate_middle_with_marker(s: &str, max_bytes: usize, marker: &str) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }

    let Some(content_budget) = max_bytes.checked_sub(marker.len()) else {
        return s[..s.floor_char_boundary(max_bytes)].to_string();
    };
    let (left_budget, right_budget) = split_budget(content_budget);
    let prefix_end = s.floor_char_boundary(left_budget);
    let suffix_start = s.ceil_char_boundary(s.len().saturating_sub(right_budget));

    assemble_truncated_output(&s[..prefix_end], &s[suffix_start..], marker)
}

/// Truncate the middle of a UTF-8 string to at most `max_tokens` approximate
/// tokens, preserving the beginning and the end. Returns the possibly
/// truncated string and `Some(original_token_count)` if truncation occurred;
/// otherwise returns the original string and `None`.
pub fn truncate_middle_with_token_budget(s: &str, max_tokens: usize) -> (String, Option<u64>) {
    if s.is_empty() {
        return (String::new(), None);
    }

    if max_tokens > 0 && s.len() <= approx_bytes_for_tokens(max_tokens) {
        return (s.to_string(), None);
    }

    let truncated = truncate_with_byte_estimate(
        s,
        approx_bytes_for_tokens(max_tokens),
        /*use_tokens*/ true,
    );
    let total_tokens = u64::try_from(approx_token_count(s)).unwrap_or(u64::MAX);

    if truncated == s {
        (truncated, None)
    } else {
        (truncated, Some(total_tokens))
    }
}

fn truncate_with_byte_estimate(s: &str, max_bytes: usize, use_tokens: bool) -> String {
    if s.is_empty() {
        return String::new();
    }

    if max_bytes == 0 {
        let total_chars = if use_tokens { 0 } else { s.chars().count() };
        return format_truncation_marker(
            use_tokens,
            removed_units(use_tokens, s.len(), total_chars),
        );
    }

    if s.len() <= max_bytes {
        return s.to_string();
    }

    let total_bytes = s.len();
    let (left_budget, right_budget) = split_budget(max_bytes);
    let (removed_chars, left, right) = if use_tokens {
        // Token markers depend only on byte counts; avoid scanning the discarded middle.
        let prefix_end = s.floor_char_boundary(left_budget);
        let suffix_start = s.ceil_char_boundary(total_bytes - right_budget);
        (0, &s[..prefix_end], &s[suffix_start..])
    } else {
        split_string(s, left_budget, right_budget)
    };
    let marker = format_truncation_marker(
        use_tokens,
        removed_units(
            use_tokens,
            total_bytes.saturating_sub(max_bytes),
            removed_chars,
        ),
    );

    assemble_truncated_output(left, right, &marker)
}

pub fn approx_token_count(text: &str) -> usize {
    let len = text.len();
    len.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN
}

pub const fn approx_bytes_for_tokens(tokens: usize) -> usize {
    tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

pub fn approx_tokens_from_byte_count(bytes: usize) -> u64 {
    let bytes_u64 = bytes as u64;
    bytes_u64.saturating_add((APPROX_BYTES_PER_TOKEN as u64).saturating_sub(1))
        / (APPROX_BYTES_PER_TOKEN as u64)
}

fn split_string(s: &str, beginning_bytes: usize, end_bytes: usize) -> (usize, &str, &str) {
    if s.is_empty() {
        return (0, "", "");
    }

    let len = s.len();
    let tail_start_target = len.saturating_sub(end_bytes);
    let mut prefix_end = 0usize;
    let mut suffix_start = len;
    let mut removed_chars = 0usize;
    let mut suffix_started = false;

    for (idx, ch) in s.char_indices() {
        let char_end = idx + ch.len_utf8();
        if char_end <= beginning_bytes {
            prefix_end = char_end;
            continue;
        }

        if idx >= tail_start_target {
            if !suffix_started {
                suffix_start = idx;
                suffix_started = true;
            }
            continue;
        }

        removed_chars = removed_chars.saturating_add(1);
    }

    if suffix_start < prefix_end {
        suffix_start = prefix_end;
    }

    let before = &s[..prefix_end];
    let after = &s[suffix_start..];

    (removed_chars, before, after)
}

fn split_budget(budget: usize) -> (usize, usize) {
    let left = budget / 2;
    (left, budget - left)
}

fn format_truncation_marker(use_tokens: bool, removed_count: u64) -> String {
    if use_tokens {
        format!("…{removed_count} tokens truncated…")
    } else {
        format!("…{removed_count} chars truncated…")
    }
}

fn removed_units(use_tokens: bool, removed_bytes: usize, removed_chars: usize) -> u64 {
    if use_tokens {
        approx_tokens_from_byte_count(removed_bytes)
    } else {
        u64::try_from(removed_chars).unwrap_or(u64::MAX)
    }
}

fn assemble_truncated_output(prefix: &str, suffix: &str, marker: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + marker.len() + suffix.len() + 1);
    out.push_str(prefix);
    out.push_str(marker);
    out.push_str(suffix);
    out
}


// ----- codex-rs/protocol/src/protocol.rs -----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

impl TruncationPolicy {
    pub fn token_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                usize::try_from(approx_tokens_from_byte_count(*bytes)).unwrap_or(usize::MAX)
            }
            TruncationPolicy::Tokens(tokens) => *tokens,
        }
    }

    pub fn byte_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => *bytes,
            TruncationPolicy::Tokens(tokens) => approx_bytes_for_tokens(*tokens),
        }
    }

    /// `impl Mul<f64>` upstream.
    pub fn scaled(self, multiplier: f64) -> Self {
        match self {
            TruncationPolicy::Bytes(bytes) => TruncationPolicy::Bytes((bytes as f64 * multiplier).ceil() as usize),
            TruncationPolicy::Tokens(tokens) => TruncationPolicy::Tokens((tokens as f64 * multiplier).ceil() as usize),
        }
    }
}

// ----- codex-rs/utils/output-truncation/src/lib.rs -----

/// Adds the existing 20% allowance for serialization and headers.
pub fn with_serialization_allowance(policy: TruncationPolicy) -> TruncationPolicy {
    policy.scaled(1.2)
}

pub fn truncate_text(content: &str, policy: TruncationPolicy) -> String {
    match policy {
        TruncationPolicy::Bytes(bytes) => truncate_middle_chars(content, bytes),
        TruncationPolicy::Tokens(tokens) => truncate_middle_with_token_budget(content, tokens).0,
    }
}

/// Applies the byte/token budget without changing success metadata or media ordering.
pub fn truncate_function_output_payload(output: &mut FunctionCallOutputPayload, policy: TruncationPolicy) {
    match &mut output.body {
        FunctionCallOutputBody::Text(text) => *text = truncate_text(text, policy),
        FunctionCallOutputBody::ContentItems(items) => {
            *items = truncate_function_output_items_with_policy(items, policy);
        }
    }
}

pub fn truncate_function_output_items_with_policy(
    items: &[FunctionCallOutputContentItem],
    policy: TruncationPolicy,
) -> Vec<FunctionCallOutputContentItem> {
    let mut out: Vec<FunctionCallOutputContentItem> = Vec::with_capacity(items.len());
    let mut remaining_budget = match policy {
        TruncationPolicy::Bytes(_) => policy.byte_budget(),
        TruncationPolicy::Tokens(_) => policy.token_budget(),
    };
    let mut omitted_text_items = 0usize;

    for item in items {
        match item {
            FunctionCallOutputContentItem::InputText { text } => {
                if text.is_empty() {
                    continue;
                }
                if remaining_budget == 0 {
                    omitted_text_items += 1;
                    continue;
                }
                let cost = match policy {
                    TruncationPolicy::Bytes(_) => text.len(),
                    TruncationPolicy::Tokens(_) => approx_token_count(text),
                };
                if cost <= remaining_budget {
                    out.push(FunctionCallOutputContentItem::InputText { text: text.clone() });
                    remaining_budget = remaining_budget.saturating_sub(cost);
                } else {
                    let snippet_policy = match policy {
                        TruncationPolicy::Bytes(_) => TruncationPolicy::Bytes(remaining_budget),
                        TruncationPolicy::Tokens(_) => TruncationPolicy::Tokens(remaining_budget),
                    };
                    let snippet = truncate_text(text, snippet_policy);
                    if snippet.is_empty() {
                        omitted_text_items += 1;
                    } else {
                        out.push(FunctionCallOutputContentItem::InputText { text: snippet });
                    }
                    remaining_budget = 0;
                }
            }
            FunctionCallOutputContentItem::InputImage { image_url } => {
                out.push(FunctionCallOutputContentItem::InputImage { image_url: image_url.clone() });
            }
        }
    }

    if omitted_text_items > 0 {
        out.push(FunctionCallOutputContentItem::InputText {
            text: format!("[omitted {omitted_text_items} text items ...]"),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_budget_truncates_the_middle() {
        let text = "a".repeat(100) + &"b".repeat(100);
        let (out, original) = truncate_middle_with_token_budget(&text, 10);
        assert_eq!(original, Some(50));
        assert!(out.starts_with("aaaa") && out.ends_with("bbbb"), "{out}");
        assert!(out.contains("tokens truncated"), "{out}");
    }

    #[test]
    fn under_budget_is_unchanged() {
        assert_eq!(truncate_text("short", TruncationPolicy::Bytes(10_000)), "short");
    }
}
