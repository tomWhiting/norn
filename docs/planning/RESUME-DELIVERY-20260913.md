# Resumed mailbox delivery identity — 13 September 2026, Melbourne

Tom reports Mercury cannot accept typing in Herdr stack/mercury. The named pane wB:p1D is actually at zsh after a TUI AgentMismatch failure. Read-only export confirms agent_message.dequeued names current runtime 50230b20-cf91-44a4-8893-9f10ba4db566 while agent_message.delivered for the same messages names historical runtime 74de3765-f357-420c-839a-ba50ae150d92.

R1. Preserve queued messages, original recipient provenance and canonical delivery IDs across resume. Do not rewrite the session or bypass projection identity checks.
R2. Delivered audits and live notifications must identify the current mailbox consumer, already established by the flush guard and loop context.
R3. Test a canonical queue restored under a new runtime through delivery, live projection, and subsequent replay. Assert one conversation input and no repeated delivery.
R4. Run formatting, strict workspace Clippy, AST/LOC scans, regression tests and a terminal resume/typing probe before installation. Commit and push; source-bound 205 battery gates main landing.

File wall: crates/norn/src/loop/delivery_pending.rs; its new resume regression module; Cargo.toml/Cargo.lock version; this brief and release notes. No mutations to Mercury's session data.

Owner-directed source review (Ripley): the current recipient comes from the same loop identity that owns the flush guard, pending lookup, commit and dequeue. Only the delivered observation/audit changes; immutable queue and UserMessage framing keep their original provenance. Strict workspace/all-target Clippy, fmt and changed-file AST/LOC scans passed; full tests and terminal probe pending.
