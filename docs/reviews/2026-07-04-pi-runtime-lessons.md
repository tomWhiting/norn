# pi runtime — session-tree lessons for norn (2026-07-04)

Research synthesis on the pi agent runtime by Mario Zechner (GitHub `badlogic`; repo formerly `badlogic/pi-mono`, now `earendil-works/pi`, MIT). Pi independently converged on an immutable JSONL session tree with append-only compaction — the closest production analogue to norn's session-tree design that exists. Facts below were verified against pi's docs and source (`session-manager.ts`, `agent-session.ts`) unless marked UNVERIFIED.

---

## 1. How pi actually does it

### Storage

- **One JSONL file per session; the entire tree lives in one file.** Branching never creates new files. No SQLite, no index files, no sidecar state — everything is re-derived by replaying the JSONL. ([session-format.md](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/session-format.md))
- Location: `~/.pi/agent/sessions/--<encoded-cwd>--/<timestamp>_<uuid>.jsonl`. Cwd encoding: strip leading slash, replace `/` `\` `:` with `-`, wrap in `--...--` — essentially Claude Code's directory scheme. (`session-manager.ts` `getDefaultSessionDirPath`)
- Line 1 is a `SessionHeader` (not a tree node): `{"type":"session","version":3,"id":"<uuid>","timestamp":...,"cwd":...}` plus optional `parentSession: "/path/to/source.jsonl"` when created via `/fork` or `/clone`.
- **Session id = UUIDv7** (time-ordered), embedded in both header and filename. CLI accepts a file path or a partial-id prefix match (`--session <partial>`). UNVERIFIED: exact prefix-matching semantics (documented, but the resolving CLI code was not read).
- **Versioned with in-place migration** (v1 linear → v2 tree → v3): migration runs on load and rewrites the whole file once. (`migrateToCurrentVersion`, `_rewriteFile`; [issue #316](https://github.com/badlogic/pi-mono/issues/316))
- **Lazy file creation**: nothing hits disk until the first assistant message exists; entries buffer in memory and flush together (open flag `"wx"`). After that, each append is one `appendFileSync` of one JSON line — append-only, crash-safe to within one line.

### Tree structure

- Every entry has `type`, `id` (**8-char random hex**, collision-checked per file only), `parentId` (null for root), and an ISO `timestamp`. Parent references are by random short id — not hash, not index — chosen (issue #316) so ids survive branch extraction to new files without remapping.
- **Branching** = move the in-memory leaf pointer to any earlier entry (`branch(entryId)`); the next append becomes a sibling child. UX: `/tree` navigation, or selecting an old user message sets the leaf to that message's parent and drops its text in the editor for edit-and-resubmit. (`agent-session.ts` ~2858; [sessions.md](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/sessions.md))
- **No convergence/merge.** Strictly single-parent. The only cross-branch link is a `BranchSummaryEntry` (`fromId` = abandoned leaf + LLM-generated summary) appended at the new position when leaving a branch — knowledge transfer, not structural merge. ([compaction.md](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/compaction.md))
- **The current-path pointer is implicit**: on load, `leafId` = the last entry in file order. The durable "active branch" marker is literally "last line appended." Branch switches persist only if something gets appended afterwards. `resetLeaf()` allows a second root → a file can technically hold a forest.

### Entry types

`SessionMessageEntry` (full `AgentMessage`: user/assistant/toolResult/bashExecution/custom; assistant messages carry `api`, `provider`, `model`, `stopReason`, full usage + cost breakdown; images inline base64), `model_change`, `thinking_level_change`, `compaction` (`summary`, `firstKeptEntryId`, `tokensBefore`, `details` incl. `readFiles`/`modifiedFiles`), `branch_summary` (`fromId`, `summary`), `custom` (extension state, **never enters LLM context**), `custom_message` (extension-injected, **does** enter context, `display` flag), `label` (`targetId`, `label` — user bookmarks, themselves tree entries), `session_info` (name). Bash executions carry `exitCode`, `truncated`, `fullOutputPath` (large output spilled to a side file), and `excludeFromContext` for `!!`-prefixed commands.

Deliberately **not** stored: the leaf pointer, the rendered LLM context, the system prompt and tool definitions (rebuilt from config), any per-node hash/integrity data, any index.

### Context reconstruction & compaction

- Context = walk **leaf→root** (`buildContextEntries`), then map entries→LLM messages. Compaction rule: emit the compaction summary first, then entries from `firstKeptEntryId` up to the compaction entry, then everything after. Model and thinking level are extracted from the path itself.
- **Compaction is a non-destructive view operation** — exactly norn's model. Nothing deleted; a `CompactionEntry` is appended and the context build reroutes through it. Chained compactions re-summarize from the previous `firstKeptEntryId`, so nothing is lost across generations. Cut points never split a toolCall/toolResult pair; a single oversized turn gets a "split turn" cut at an assistant message. File-op provenance accumulates across compactions and branch summaries via `details`.
- Fork variants: `/tree` = same file, in-place; `/fork` = new file from a chosen earlier user message; `/clone` = new file with only the active branch. `forkFrom` copies **all entries physically** into the new file (child survives parent deletion; O(history) per fork). Lineage across files is just the header's `parentSession` absolute path — a weak link.

### Philosophy (context for why the format is so lean)

Four tools (read, write, edit, bash), a system prompt + tool definitions totalling **under 1,000 tokens**, no MCP/sub-agents/plan-mode/permissions in core — everything else via a TypeScript extension system with ~25 blockable/mutable hook events (`tool_call` block/mutate, `before_agent_start` prompt chaining, `context` message rewriting, `appendEntry` for persisted extension state). Extensions rebuild their state on `session_start` by **replaying the branch** — event-log reconstruction, no separate store. ([blog](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/); [extensions docs](https://pi.dev/docs/latest/extensions))

---

## 2. What we should steal

Each item maps to a specific norn design decision.

1. **`firstKeptEntryId` compaction chaining → norn's compaction-as-view.** Pi's exact mechanism — summary node + kept-boundary id, with each new compaction re-summarizing from the *previous* boundary so no generation is lost — is the production-proven shape of our "summary node + path reroute" design. Also steal the two invariants: never split a toolCall/toolResult pair at a cut point, and handle single-oversized-turn splits explicitly at an assistant message.

2. **`BranchSummaryEntry` (`fromId` + LLM summary on branch-leave) → norn's convergence design.** Pi's cheap 80% version of convergence: when a path is abandoned, its knowledge is summarized into the new path with a pointer back. Norn's convergence nodes are strictly more expressive, but this should be the *degenerate case* of convergence — an abandoned branch converging informationally without a structural merge. Worth having even before full merge nodes ship.

3. **The `custom` / `custom_message` / `excludeFromContext` triad → road-sign annotation events.** Pi bakes a primitive in-context/out-of-context/transient distinction into entry types. It validates that the in-context-visibility axis is the load-bearing one. Norn's road-sign layer (important / superseded / garbage / transient) generalizes this — but pi confirms the annotations must be *durable events in the log*, not ephemeral UI state. This directly informs the "suppression marks not durable" gap in the 14-gap inventory.

4. **Labels as tree entries → annotation layer mechanics.** Pi's `LabelEntry` (`targetId` + label, clearing = append with `label: undefined`) shows that annotations targeting other nodes work cleanly as append-only events with latest-wins semantics. Same pattern applies to road signs: annotation event referencing a target id, never a mutation.

5. **UUIDv7 session ids + partial-prefix CLI resolution → session identity.** Time-ordered ids double as sort keys and support human-typeable prefix matching. Adopt for norn session ids (but not node ids — see Reject #4).

6. **Compaction/branch-summary `details` provenance (`readFiles`/`modifiedFiles`) accumulated across generations.** File-op provenance surviving compaction is exactly the kind of structured metadata norn's contract-driven forks want in structured returns. Steal the accumulation-across-summaries pattern.

7. **Lazy file creation + single-line append discipline → durable child sessions.** No file until there's real content; then strictly one `appendFileSync` per event, crash-safe to within a line. Directly applicable to fixing "forked children don't persist": children can open their spool lazily on first real event, avoiding litter from aborted spawns.

8. **`fullOutputPath` spill for oversized tool output → the truncated-forever gap.** Pi keeps truncated bash output in-log but spills the *full* output to a side file with a pointer. This is a ready-made answer to norn's "truncated-forever tool outputs" gap: the event stores the truncation plus a durable reference to the whole thing.

9. **State reconstruction by branch replay (extensions on `session_start`).** No secondary store for extension/plugin state — replay the path. Validates norn's "everything re-derived from the tree" stance and is the right pattern for any norn-internal subsystems that need per-session state.

10. **Directory layout precedent.** Pi independently arrived at Claude-Code-style per-cwd flattened directories with `<timestamp>_<uuid>.jsonl` filenames. Since we're adopting a CC-style layout anyway, pi confirms the scheme survives contact with real use (including its cross-project `listAll()` scan pattern).

---

## 3. What we should reject

1. **Leaf-as-last-line for the active path.** Zero-cost and clever, but it conflates "chronologically last write" with "active branch": switch branches without appending anything and quit → resume lands on the *old* branch; appending a label can silently move the resume point. For an embeddable runtime where multiple agents and an embedder observe the tree, the active path must be an explicit, durable event (or per-view state), not an accident of file order. Norn's explicit path layer is correct; keep it.

2. **Physical-copy forks (`forkFrom` duplicates every entry).** Fine for a human occasionally forking a chat; fatal for norn where fork-with-full-history is the *flagship, high-frequency* operation across many agents. O(history) copies per spawn, no structural sharing, base64 images inline making files fat. Norn's inherit-by-reference fork is the right call — we accept the cost that we must then solve parent-lifetime/GC, which pi never had to.

3. **In-place migration rewriting files on load.** A "read" operation that rewrites the whole file contradicts immutability and races if two processes open the same session — pi is single-user single-process; norn explicitly is not. Norn needs versioned *readers* (or explicit offline migration tooling), never rewrite-on-open.

4. **Per-file 8-hex random node ids.** Collision-checked only within one file. Norn's cross-session addressing (root/reviewer-x/verifier-y), branch-point events on parent timelines, and future convergence links all require ids that are resolvable across files. Node ids need global scope (or session-id-qualified addressing) from day one.

5. **`parentSession` as a bare absolute path.** Breaks if the source file moves; it's a path, not an identity. Norn's cross-session lineage (parent↔child, branch-point events) must link by session id + node id, with the path layout as a resolvable index, not the identity itself.

6. **No per-node integrity, silent skip of unreadable sessions.** Pi's loader does `catch → return null` and drops unreadable sessions from listings — a swallowed failure. For infrastructure-grade norn (and per CLAUDE.md's no-silent-failures rule), a truncated line or orphaned subtree must surface as a typed, logged error, and per-node integrity (at minimum length-prefixed/checksummed lines or a validation pass) is worth the cost pi declined to pay.

7. **File-level-only deletion.** `rm`/trash the .jsonl is the only delete. Norn's retention story (legal/healthcare settings) needs tombstones or policy-driven redaction events that preserve tree integrity — another thing an append-only tree can express that pi simply doesn't attempt.

8. **The no-sub-agents philosophy itself.** Pi's stance ("just run pi via bash"; sub-agent observability in other harnesses "makes no sense to me") is coherent for a personal tool, but its acknowledged consequence — poor sub-agent context transfer, community forks bolting it back on (pi-subagents, oh-my-pi) — is precisely the gap norn's fork-with-full-history + requirements contract exists to close. Pi has no structured returns, no agent addressing, no parallelism, no background processes ("use tmux"). This is norn's differentiation; don't dilute it chasing pi's minimalism. (Do keep pi's underlying *observability* argument: norn children must be first-class observable trees, which durable child sessions deliver.)

---

## 4. Open questions pi doesn't answer

Pi's format never faced these; our design must solve them without precedent.

1. **Convergence/merge semantics.** Pi is strictly single-parent. Norn's merge nodes need answers pi never gives: multi-parent context reconstruction order, deduplication when converging branches share ancestry, how road signs on either parent path apply post-merge, and what a compaction *across* a convergence point means. Pi's `BranchSummaryEntry` is only the informational degenerate case.

2. **A real annotation overlay.** Pi's labels and `excludeFromContext` are in-band flags on single entries. Norn's road signs are a taxonomy (important/superseded/garbage/transient) with *range* semantics, retroactive application by curation agents, and consent-based muting. Open: annotation events targeting ranges vs single ids; precedence when annotations conflict; whether curation-agent annotations live in the target session's log or the curator's.

3. **Multi-agent trees and path addressing.** Pi has one human, one file, one process. Norn: who may append to whose tree, how a child's branch-point manifests as an event on the *parent's* timeline (gap inventory), whether root/reviewer-x/verifier-y addresses are stored in events or derived from spawn lineage, and concurrent-writer safety on shared directory layouts. Pi's single-`appendFileSync` crash-safety story does not extend to concurrent writers.

4. **Durable children with reference-inheritance.** Because norn forks by reference, not copy: parent-file lifetime pinning while children exist, GC of abandoned parents, and context reconstruction that walks *across* session-file boundaries (leaf→root spanning child file → parent file). Pi's leaf→root walk is always intra-file.

5. **Contracts and structured returns as tree citizens.** No counterpart in pi at all. Open: is the requirements contract an event in the child's root, the parent's spawn event, or both; how a structured return is represented so the parent's context reconstruction ingests it; how contract violation/timeout appears on both timelines.

6. **Explicit multi-view state.** Once the active path is explicit rather than leaf-as-last-line, norn must decide: one active path per session, or per-consumer views (embedder view vs agent view vs curated view)? Pi's "compaction = view reroute" hints at views but hardcodes exactly one.

7. **Integrity and retention at infrastructure grade.** Per-node integrity, tamper evidence, policy-driven redaction that doesn't orphan subtrees, and multi-process read/write coordination — none exist in pi (see Reject #6/#7); all are table stakes for norn's target settings.

---

## 5. Sources

**Pi docs & source (primary, read directly):**
- https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/session-format.md
- https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/sessions.md
- https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/compaction.md
- https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/core/session-manager.ts
- https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/core/agent-session.ts
- https://github.com/badlogic/pi-mono/issues/316 (session tree format proposal)
- https://github.com/earendil-works/pi
- https://pi.dev/docs/latest/extensions · https://pi.dev/docs/latest/session-format · https://pi.dev/docs/latest/compaction · https://pi.dev/docs/latest/sessions · https://pi.dev/docs/latest/skills · https://pi.dev/docs/latest/packages · https://pi.dev/docs/latest/sdk

**Author & commentary:**
- https://mariozechner.at/posts/2025-11-30-pi-coding-agent/ (canonical design writeup; predates the tree format)
- https://lucumr.pocoo.org/2026/1/31/pi/ (Armin Ronacher)
- https://newsletter.pragmaticengineer.com/p/building-pi-and-what-makes-self-modifying
- https://news.ycombinator.com/item?id=46844822 · https://news.ycombinator.com/item?id=47143754
- https://github.com/can1357/oh-my-pi (fork adding subagents/background — evidence of demand)
- https://www.npmjs.com/package/@mariozechner/pi-coding-agent

**Uncertainty retained from source reports (UNVERIFIED):** OpenClaw star figures (250K+, secondary sources); "5-10x longer lasting token windows" user claims (anecdotal); the HN claim that pi dropped off the terminal-bench leaderboard; exact prefix-matching semantics of `--session <partial id>`; whether `/tree` no-summary branch switches were ever *intended* to persist (behavior derived from source; no doc statement).
