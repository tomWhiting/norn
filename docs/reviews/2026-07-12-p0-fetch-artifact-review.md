# Review of the P0 D1B fetched-artifact correction — 2026-07-12

**Reviewer:** Claude (Fable), external Gate D reviewer (same reviewer as the Gate D and openat-correction reviews).

**Range reviewed:** `ac7c8f3..dc6c2e3` (implementation `735db41`, records `795327c`/`dc6c2e3`). Correction record: `2026-07-12-p0-fetch-artifact-correction.md`.

**Verdict: the D1B slice is ACCEPTED.** The fetched-document workspace write is gone, the replacement implements the D1B ruling faithfully — immutable, session-owned, descriptor-confined, narrowly readable — and every claim in the correction record verified independently. Whole-phase P0 Gate D remains open, as the record itself says. Three non-blocking findings below, one of which needs owner coordination (Meridian).

---

## 1. Independent verification

| Claim | Independent result |
|---|---|
| `session::artifacts_tests` 3/3, `tools::web::fetch::tests` 27/27, builder wiring test, spawn/fork Arc-identity test | **All reran green, counts exact** |
| Inventory: no production `.norn/fetched` or workspace fetch path remains | **Re-derived exact** — the only occurrence is the negative assertion in the fetch tests; the D1B chain enumerates to precisely the eight files claimed |
| Production LOC (truncate-at-`#[cfg(test)]` + tokei): artifacts 78, session_open 73, assembly 472, spawn_context 181, fetch 405 | **All five exact** — and this is the single-method counting Gate D demanded |
| `fsync` polarity | Correct: `DurabilityPolicy::Flush` is the never-fsync policy, so `durability != Flush` enables sync; the artifact write syncs file → `fetched/` → `artifacts/` → session dir → data root, the full entry chain |
| Capability boundary | Verified by design and test: exempt roots are canonicalized at set time, request paths at check time (dotdot and symlink fixtures pre-existing in `confinement.rs`); the builder regression exercises the **real** `read` tool — artifact readable, sibling transcript `permission_denied`, session data root never exempted; spawn and fork children inherit both the store (Arc-identity pinned) and the exemption list |
| Fail-closed ordering | `require_extension` resolves before any network I/O; the wiremock sentinel proves zero requests reach the endpoint on an unowned run |

Ruling conformance: every fetch invocation gets a fresh UUID name through `PrivateRoot::create_new` (O_EXCL), so no byte referenced by an older transcript event can ever be rewritten — the old SHA-256-of-URL dedup file is gone, and the replacement test pins two same-URL fetches to two distinct immutable artifacts. Frontmatter injection via hostile URL is closed (JSON-escaped scalar, with a forged-key regression test).

## 2. Findings (none blocking this slice)

**F-A — Library embedders lose `web_fetch` until they adopt `open_session` (owner coordination required).** `SessionArtifactStore::for_session` is `pub(crate)`, and the only production wiring is `AgentBuilder::open_session` → `open_root_session`. That is the right shape — embedders must not mint artifact authority ad hoc — and the CLI is unaffected (it always routes through the one `open_session` front door, `from_cli.rs:191`). But **Meridian embeds norn without `open_session`** (its migration to `open_session`/`SessionManager` is a known open item), so after this change Meridian agents' `web_fetch` fails with typed `MissingExtension` before any request. Fail-closed and correct under the ruling — but it converts Meridian's pending migration from "nice to have" into "required for fetch to work," and neither the correction record nor MERIDIAN-HANDOFF names that consequence. Flagging it here so it's scheduled, not discovered.

**F-B — `strip_frontmatter` silently discards leading content (pre-existing, relocated this round; recommend fixing while the file is young).** `artifacts.rs:92`: any content whose conversion output starts with `---` has everything up to the next `\n---` silently dropped. Two reachable shapes: a page whose HTML→markdown conversion begins with a thematic break (leading `<hr>`), and a raw markdown document fetched with genuine frontmatter — its metadata is deleted from the archived copy. That contradicts the artifact's claim to be the fetched document, and silent byte-dropping sits poorly next to an immutability guarantee. Not stripping at all is safe (the store's own frontmatter block closes before the body; a body that begins with `---` renders as content), which would also delete the helper.

**F-C — New long-lived descriptor class (for the D1C inventory).** The store holds its `PrivateRoot` directory descriptor for the life of the root agent (+1 fd per open root session, shared by all descendants via Arc). Modest, but the D1C corrective item explicitly requires recording descriptor-allocation boundaries — this one now exists and should appear in that inventory.

**F-D — nit:** `#[cfg(test)] mod artifacts_tests;` lives in `session/mod.rs`, deviating from both the strict mod.rs-purity rule and the repo's own precedent (`private_fs.rs:514` attaches its test file via `#[path]` inside the module). One-line move.

## 3. Process notes

- The prior review was accepted correctly: my openat review doc committed byte-identical, the structural single-retry justification added to the correction record (with a sharper articulation of the unlink-churn residual than mine), and the plan checklist updated with the independent-acceptance citation.
- This record repeats the good pattern: scoped claim ("D1B covers the fetched-document omission and its access boundary only" — spool families explicitly *not* claimed), checked-in inventory commands, and a review request that invites attack on the boundary rather than defending it.

## 4. Standing state

**Closed:** Gate D F1 (accepted previously), F2/D1B (this review).
**Still open before whole-phase READY:** D1C NOFILE implementation (now including the F-C descriptor above), D1D mcp_servers decision, zombie helper deletion, workspace-wide LOC regeneration, 429/401 + loop-level sentinels, traceability records, Gate A exception decision, full Gate C rerun over the finished corrective candidate. Downstream: Meridian `open_session` migration (F-A).
