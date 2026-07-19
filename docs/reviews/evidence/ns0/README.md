# NS0 candidate evidence

**Status:** Review evidence for the NS0 source inventory and logical contract
fixtures. This directory does not establish canonical contract bytes or NS0
acceptance.

## Inventory

`inventory-manifest.json` pins the current Norn source base and ten adjacent
committed snapshots. It records the selected committed Rust/TypeScript
source boundary, four discovery queries, 34 source-bound semantic records, and
seven explicit absence assertions.

`verify_inventory.py` reads only the pinned Git objects. It fails when a pinned
object is unavailable, a semantic needle changes, a semantic record falls
outside the selected source boundary, or a negative assertion finds a
forbidden authority type. Checkout HEAD is retained as informational freshness
metadata only. A later commit, rebase, or sibling-repository advance does not
invalidate this historical evidence; re-pinning requires an explicit freshness
sweep and regenerated dispositions.

Run from the Norn worktree root:

```sh
python3 -B docs/reviews/evidence/ns0/verify_inventory.py \
  --manifest docs/reviews/evidence/ns0/inventory-manifest.json \
  --output docs/reviews/evidence/ns0/inventory-report.json
```

The verifier derives the adjacent repository root from Git's common directory.
Use `--ablative-root` only when reproducing the evidence in a differently
arranged checkout.

The discovery hashes are a reproducible lexical baseline. The manifest does
not disposition every lexical match individually, so the plan's exhaustive
semantic-sweep item remains open.

## Logical fixtures

`contract-fixture-manifest.json` identifies six logical candidate cases and
binds each fixture file with a SHA-256 integrity hash. Those hashes identify
the reviewed files only; they are not record, schema, event, or artifact
digests.

Run:

```sh
python3 -B docs/reviews/evidence/ns0/verify_contract_fixtures.py \
  --manifest docs/reviews/evidence/ns0/contract-fixture-manifest.json \
  --output docs/reviews/evidence/ns0/contract-fixture-report.json
```

The fixture verifier rejects duplicate JSON keys, malformed domain references,
duplicate roles, missing Norn event/session scope, malformed event/relation
shapes, canonical-byte or authorization overclaims, executable unknowns, and a
multi-parent example that omits the current Norn format-2 limitation.

Rust and TypeScript decoders are intentionally absent. They remain blocked on
the owner repository, canonical encoding, digest, and versioning decisions
listed in `docs/design/ablative-stack-contract-freeze.md`.
