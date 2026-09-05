---
type: design
cluster: norn-verification
title: Truthful diagnostics and declared venue verification
---

# Truthful diagnostics and declared venue verification

> **Cluster:** norn-verification

## Intention

Make diagnostic failures truthful and declare the venue legs without treating diagnostics as landing authority.

## Problem

The legacy diagnostic script could hide failing legs or reset source, the pinned baseline has four Clippy findings, and a prompt-refresh test assumes a distinct filesystem timestamp, and an account-catalog recovery fixture applies its deliberate lock-failure deadline to successful setup. The live Aion battery checks HEAD without checking source cleanliness around each leg.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `scripts/remote-battery.sh` | NV-001 R1 exact file wall |  |
| `scripts/tests/remote_battery_test.py` | NV-001 R2 exact file wall |  |
| `gates.json` | NV-001 R3 and R7 exact file walls |  |
| `crates/norn/src/resource/mod.rs` | NV-001 R4 exact file wall |  |
| `crates/norn/src/resource/descriptor_governor.rs` | NV-001 R4 exact file wall |  |
| `crates/norn/src/provider/openai_oauth/auth_root.rs` | NV-001 R4 exact file wall |  |
| `crates/norn/src/session/migration/hardening_tests.rs` | NV-001 R4 exact file wall |  |
| `crates/norn/src/loop/runner/tests/d8_parent_prompt_hot_reload.rs` | NV-001 R5 timestamp fixture repair before edit |  |
| `crates/norn/src/provider/openai_oauth/account_catalog_tests.rs` | NV-001 R6 account-catalog fault-phase fixture repair before edit |  |
| `scripts/source-bound-leg.py` | NV-001 R7 exact file wall |  |
| `scripts/tests/source_bound_leg_test.py` | NV-001 R7 exact file wall |  |

## Constraints

- **P1** — Tom5September2026: start Norn fixes; exact-commit205Aionbattery rule
- **P2** — Source scripts/remote-battery.sh and isolated failing-leg proof from review
- **P3** — aion/workflows/repo-battery/README.md
- **V1** — The production same-mtime freshness finding remains separately tracked as NWP-00.5. The fixture correction does not repair it. Aion source binding and the exact-commit landing receipt remain open.
- **V2** — NV-001 R7 guards every Norn leg before and after execution, with committed declaration and guard digests. This mitigates persistent dirty-source execution for Norn; it does not repair Aion X.1 globally or detect a change restored inside a leg. Landing review must compare the receipt declaration and guard digests with the exact commit and ensure no unreviewed writer shares the run tree.
- **V3** — Git status is not byte identity: every source witness hashes raw tracked file bytes and symlink targets with the repository object format against HEAD tree object IDs, and verifies executable mode. Unsupported entry types and normalized bytes that differ from committed blobs refuse; no stat-cache or filter assumption silently permits a mismatch.
