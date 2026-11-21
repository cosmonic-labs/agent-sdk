//! The parts of Codex this agent keeps, vendored from openai/codex 0.162.0
//! (commit c1382380de69521303b416720a52f42d51af6248, Apache-2.0; see
//! LICENSE-codex). Each file names the upstream files it came from and what
//! it leaves out. The turn loop that drives them is `crate::turn`, ported
//! from `codex-rs/core/src/session/turn.rs`.

pub mod compact;
pub mod history;
pub mod models;
pub mod truncate;

/// Codex's `gpt_5_codex_prompt.md`, rewritten for an agent with no shell:
/// the `rg` hint, `apply_patch`, terminal commands, sandboxing and approval
/// modes are gone, and `## General` says how to work through the separate
/// tools the host offers (search before reading, granted folders, truncated
/// output) without naming any, since which ones are offered varies. The plan
/// tool section, editing, review and final-answer guidance are Codex's own.
pub const BASE_INSTRUCTIONS: &str = include_str!("prompts/base_instructions.md");

/// Codex's default tool-output budget for models without their own
/// (`TruncationPolicyConfig::bytes(10_000)` in `models-manager`).
pub const TOOL_OUTPUT_POLICY: truncate::TruncationPolicy = truncate::TruncationPolicy::Bytes(10_000);
