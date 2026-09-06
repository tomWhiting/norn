---
type: design
cluster: norn-verification
title: Truthful diagnostics and declared venue verification
---

# Truthful diagnostics and declared venue verification

## Intention

Make diagnostic failures truthful and declare the venue legs without treating diagnostics as landing authority.

## Problem

The legacy diagnostic script could hide failing legs or reset source, the pinned baseline has four Clippy findings, and a prompt-refresh test assumes a distinct filesystem timestamp, and an account-catalog recovery fixture applies its deliberate lock-failure deadline to successful setup. The live Aion battery checks HEAD without checking source cleanliness around each leg. Two commit-cancellation fixtures also impose a two-second cleanup deadline without a product SLA; NV-002 removes only those wrappers while preserving cleanup assertions. The large late-watch catch-up fixture also fails before watch attachment while polling for seq output; NV-003 replaces its fixed polling/child-sleep preconditions with committed-output/alert push and test-owned release. NV-004 corrects a descriptor-retention fixture that did not keep earlier admitted children alive before expecting a later denial; one baseline-accounted FIFO holds overlap and semaphore push proves return.

## Structure

- `crates/norn/Cargo.toml`
- `crates/norn/src/lib.rs`
- `crates/norn/src/loop/runner/tests/d8_parent_prompt_hot_reload.rs`
- `crates/norn/src/process/manager.rs`
- `crates/norn/src/provider/auth/accounts_device_tests.rs`
- `crates/norn/src/provider/openai/provider.rs`
- `crates/norn/src/provider/openai_oauth/account_catalog_tests.rs`
- `crates/norn/src/provider/openai_oauth/auth_root.rs`
- `crates/norn/src/resource/descriptor_governor.rs`
- `crates/norn/src/resource/mod.rs`
- `crates/norn/src/session/migration/hardening_tests.rs`
- `crates/norn/src/test_prerequisite.rs`
- `crates/norn/src/tests/descriptor_retention.rs`
- `crates/norn/src/tools/lsp/workspace_backend/stub_tests.rs`
- `crates/norn/src/tools/search/tool.rs`
- `crates/norn/tests/live_openai_smoke.rs`
- `docs/design/norn-verification/CHECKLIST.md`
- `docs/design/norn-verification/DESIGN.md`
- `docs/design/norn-verification/USER-STORIES.md`
- `docs/design/norn-verification/briefs/NV-003.json`
- `docs/design/norn-verification/briefs/NV-003.md`
- `docs/design/norn-verification/briefs/NV-004.json`
- `docs/design/norn-verification/briefs/NV-004.md`
- `docs/design/norn-verification/checklist.json`
- `docs/design/norn-verification/design.json`
- `docs/design/norn-verification/stories.json`
- `gates.json`
- `scripts/live-openai-smoke.py`
- `scripts/remote-battery.sh`
- `scripts/source-bound-leg.py`
- `scripts/tests/live_openai_smoke_test.py`
- `scripts/tests/remote_battery_test.py`
- `scripts/tests/source_bound_leg_test.py`
- `workflows/live-openai-smoke/README.md`
- `workflows/live-openai-smoke/worker.awl`
- `workflows/live-openai-smoke/workflow.awl`

## Constraints

- **P1** — Tom5September2026: start Norn fixes; exact-commit205Aionbattery rule
- **P2** — Source scripts/remote-battery.sh and isolated failing-leg proof from review
- **P3** — aion/workflows/repo-battery/README.md
- **V1** — The production same-mtime freshness finding remains separately tracked as NWP-00.5. The fixture correction does not repair it. Aion source binding and the exact-commit landing receipt remain open.
- **V2** — NV-001 R7 guards every Norn leg before and after execution, with committed declaration and guard digests. This mitigates persistent dirty-source execution for Norn; it does not repair Aion X.1 globally or detect a change restored inside a leg. Landing review must compare the receipt declaration and guard digests with the exact commit and ensure no unreviewed writer shares the run tree.
- **V3** — Git status is not byte identity: every source witness hashes raw tracked file bytes and symlink targets with the repository object format against HEAD tree object IDs, and verifies executable mode. Unsupported entry types and normalized bytes that differ from committed blobs refuse; no stat-cache or filter assumption silently permits a mismatch.
- **V4** — The release battery measures deterministic/local prerequisites and compiles/lints the optional live-api-smoke target. Live network execution is a separately dispatched Aion lane with its own credential prerequisite and receipt; compile-only coverage is never described as live-provider proof. Missing local prerequisites are errors emitted through the Rust test harness, not passing tracing-only returns.
- **V5** — NV-002 changes only two test notification awaits. Existing production timing and the four-hour venue test-activity bound remain unchanged. The 3964799 red receipt retains its unproven failure cause; a historical candidate6 Elapsed report does not establish the current cause.
- **V6** — Change only a_late_attached_watch_catches_up_over_a_large_region_without_wedging and minimal cfg(test) helpers in process/manager.rs. Establish committed spool bytes via the existing ProcessHandle subscription and incremental SpoolReader, then attach the real cat watch while a test-owned release handshake keeps the child Running. Observe the stored alert via push notification; retain the equally large catch-up region, >64KiB precondition and full byte-for-byte equality. No runtime process/spool/watch changes, new timing promise, polling/sleeps/retries, ignored checks or success caps. Preserve original red evidence. The existing shared manager test module has unrelated inherited policy findings; this narrow cfg(test) repair neither authorizes their expansion nor claims whole-file clearance.
- **V7** — In active_process_permits_release_on_terminal_paths, hold actual admitted children until an explicit fixture-owned release. Retain twenty admission attempts, typed DescriptorAdmission denial, Running before release, successful output/exit, failed-working-directory and killed-child paths. Prove full permit return through the existing cfg(test) semaphore acquire, not a polling deadline. No production or descriptor-governor changes. One baseline-accounted FIFO descriptor holds all children, each consumes one explicit release byte; keep exact x/Exited0 and killed/missing-cwd proofs. Root-approved fixture-only repair preserves the original failed evidence and does not establish a production defect or waive landing gates.
