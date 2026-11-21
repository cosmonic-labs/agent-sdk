# Prompts

These files are sent to the model verbatim, so their notices live here.

All three come from openai/codex 0.162.0 (commit `c1382380de69521303b416720a52f42d51af6248`), licensed under the Apache License, Version 2.0 (see `../../../LICENSE-codex`).

| File | Upstream source | Changed |
| --- | --- | --- |
| `base_instructions.md` | `codex-rs/core/gpt_5_codex_prompt.md` | Yes, by Cosmonic, Inc.: rewritten for an agent harness with no shell. The `rg` hint, `apply_patch`, terminal commands, sandboxing and approval modes are removed, and the General section says how to work through the tools the host offers. The plan tool, editing, review and final answer guidance are Codex's own. |
| `compact_prompt.md` | `codex-rs/prompts/templates/compact/prompt.md` | No |
| `summary_prefix.md` | `codex-rs/prompts/templates/compact/summary_prefix.md` | No |
