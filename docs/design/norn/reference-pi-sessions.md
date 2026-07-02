Key findings:

**Pi Tree Session Format**

- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:821): `Session` stores `header`, `entries: Vec<SessionEntry>`, `leaf_id`, `entry_index`, and `is_linear`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:3417): `SessionHeader` persists `leafId` as `current_leaf` and parent fork provenance as `branchedFrom` / alias `parentSession`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:3552): `SessionEntry` is a tagged enum: `message`, `model_change`, `thinking_level_change`, `compaction`, `branch_summary`, `label`, `session_info`, `custom`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:3598): `EntryBase` is the tree edge: serialized `id`, `parentId`, `timestamp`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2240): appends set `parentId = current leaf`, push the entry, then move `leaf_id` to the new ID.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2793): `entries_for_current_path()` walks parent links from `leaf_id` to root, then reverses to chronological order.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:5061): `finalize_loaded_entries()` rebuilds IDs, indexes, message count, orphan diagnostics, and detects branching.

**Persistence**

- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:40): current JSONL format version is `3`.
- [docs/session.md](/Users/tom/Developer/tools/pi_agent_rust/docs/session.md:18): JSONL layout is first-line `SessionHeader`, then one `SessionEntry` per line.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:4261): JSONL load parses header first, then entry lines, then resolves active leaf from persisted `leafId`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:113): JSONL full save writes temp file, fsyncs, atomically persists, then updates session index.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:165): JSONL incremental save appends only serialized new entries.
- [src/session_sqlite.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_sqlite.rs:9): SQLite stores JSON blobs in `pi_session_header`, `pi_session_entries`, and `pi_session_meta`.
- [src/session_sqlite.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_sqlite.rs:770): SQLite full save deletes/reinserts header, entries, and meta inside `BEGIN IMMEDIATE`.
- [src/session_sqlite.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_sqlite.rs:875): SQLite incremental append inserts only new entry JSON rows and upserts meta.
- [src/session_store_v2.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_store_v2.rs:44): V2 sidecar uses `SegmentFrame` with `entry_id`, `parent_entry_id`, `entry_type`, payload hash, and raw JSON payload.
- [src/session_store_v2.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_store_v2.rs:88): V2 `OffsetIndexEntry` maps entry sequence/ID to segment byte offsets.
- [src/session_store_v2.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session_store_v2.rs:549): V2 `read_active_path()` reconstructs a branch by following `parent_entry_id` from leaf to root.

**Fork / Branch / Walk**

- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2675): `navigate_to(entry_id)` changes current leaf to an existing entry.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2771): `create_branch_from(entry_id)` is just `navigate_to`; the next append creates the actual branch by using that entry as `parentId`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2760): `reset_leaf()` moves to root-before-first-entry so the next append has `parentId = null`.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2939): `sibling_branches()` finds the nearest fork point where a parent has multiple children.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:908): `ForkPlan` describes `/fork`: copied entries, new leaf ID, selected user text.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2515): `plan_fork_from_user_message()` copies the path up to the selected user message’s parent and returns the selected text for resubmission.
- [src/session.rs](/Users/tom/Developer/tools/pi_agent_rust/src/session.rs:2703): `init_from_fork_plan()` installs that copied path into a new session.
- [src/interactive/tree.rs](/Users/tom/Developer/tools/pi_agent_rust/src/interactive/tree.rs:278): interactive `/fork` builds the plan, creates a new session, sets `branchedFrom`, initializes from the plan, and saves.
- [src/rpc.rs](/Users/tom/Developer/tools/pi_agent_rust/src/rpc.rs:1809): RPC `fork` implements the same primitive for external clients.
- [docs/tree.md](/Users/tom/Developer/tools/pi_agent_rust/docs/tree.md:29): tree selection behavior: user/custom message selection sets leaf to parent for edit/resubmit; non-user selection sets leaf to the selected node.
tokens used
