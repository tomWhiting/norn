# Review: `codex/conventions-wip` (d176907)

**Reviewer:** Sable Nightwick
**Date:** 2026-08-08
**Requested by:** Tom ("the conventions fix, let's give that a review")
**Subject:** one commit, `wip(conventions): retain hard-feedback prototype` —
`CONVENTIONS.toml` (76 lines), `diagnostics_check/tests.rs` (+181),
`.claude/skills/norn/SKILL.md` (18).
**Method:** cherry-picked onto current main (`9897785`) in a clean worktree —
applies with zero conflicts. Every claim below verified at bytes, on this
tree, today; enforcement-engine claims verified at the pinned chiron
checkout (`25161bc`).

---

## Verdict in one line

**The diagnosis is right, the mechanism is sound, three of the nine block
rules are safe to take today — and the other six would seize the estate,
because whole-file scanning collides with the ruled test-code exception.
The WIP label is honest. Split it: land the truth-telling half now, hold
the hard-feedback half until the engine can see `#[cfg(test)]`.**

---

## 1. What the branch fixes is real (independently confirmed)

Main's `CONVENTIONS.toml` declares `[rust.diagnostics] clippy` and
`[rust.remediation] rustfmt`, and `load_non_executing_conventions`
(`crates/norn/src/tools/diagnostics_infra.rs`) strips exactly those tables
before use. The file advertises enforcement that structurally cannot run —
absence presenting as presence, aimed at the discipline the whole estate
depends on. I verified this defect in the loader code on 2026-08-08, not
from this branch's claims. The branch's header rewrite states the trust
boundary plainly and removes the dead declarations. **This half is pure
truth-telling and is wanted regardless of everything below.**

## 2. The mechanism holds (verified, not assumed)

- **`on = "tool|stop"` parses.** The pinned diagnostics crate splits the
  trigger string on `|` (`conventions/rule.rs:174`) and `TestTrigger` has a
  `Stop` variant. Stop-time repetition of unresolved hard findings is a real
  capability, not aspiration.
- **The rewritten patterns are real tree-sitter queries** (attribute_item +
  inner_attribute_item forms, `#eq?`/`#match?` predicates) replacing the old
  shorthand strings, each with a `@bypass` capture.
- **The added tests are serious.** One compiles the checked-in
  `CONVENTIONS.toml` and proves every hard pattern fires on synthetic
  fixtures; one drives every mutator output shape plus stop. The fixtures
  self-dodge the very rules they test (string-concatenated `"pan"+"ic!"`,
  `"#[ig"+"nore]"`) — the author understood self-triggering.
- **The `SKILL.md` leg is unrelated but good:** prefer the repo-local
  `.claude/skills/norn` over `~/.claude/skills/norn` when present. Note: an
  earlier survey note of mine said this branch also dropped `--account` from
  the skill wrapper — **that was wrong; the diff contains no such change.**

## 3. The blocker, measured

Pattern checks run against the **whole file content** after a mutation
(`run_pattern_tool` receives `file_content`), and the rule scopes are path
matchers — they cannot see `#[cfg(test)]` inside a file. Norn's tests live
overwhelmingly *inside* source files as `mod tests`. Measured on today's
tree (942 `.rs` files under `crates/norn/src`):

| Rule at `block` | Files that fire today | Where the hits live |
|---|---|---|
| `fallible-shortcuts` (unwrap/expect) | **175** | test modules — production is clean by construction (workspace lints deny it) |
| `allow-attr` | **179** | the ruled test-code exception itself (`#[allow]` on `#[cfg(test)]` items) |
| `silent-var-rename` | ~110 | mixed; includes trait-impl `_param` idioms |
| `panic-macros` | tests' assertion arms | same collision |
| `todo-markers` (regex, raw text) | 3 — including `diagnostics_infra.rs` and `diagnostics_check/tests.rs` themselves | **the conventions machinery's own files become un-editable** |
| `expect-attr` | few | same class |
| `deny-attr` | **0** | safe to block today |
| `ignore-attr` | **0** | safe to block today |
| `cfg-any` | **0** (the one grep hit is a string literal in feedback text — the AST matcher cannot match it) | safe to block today |

**Consequence if merged as-is:** any edit to any test-bearing file — 178 of
the 351 test-carrying files match on unwrap/allow alone — fails validation
at mutation time and again at stop. Gate-mode tools reject the call
outright. The estate's own sanctioned idiom (the CLAUDE.md test-code
exception, owner-ruled) becomes a hard stop. **This is why the branch is
WIP, now measured rather than guessed.**

## 4. Test run on the rebased tree — green, both new tests included

`cargo test -p norn diagnostics_check`, measured at my hands 2026-08-08:

- **main (`9897785`):** `test result: ok. 55 passed; 0 failed`
- **main + cherry-pick (`5e17899`, worktree):** `test result: ok. 57 passed;
  0 failed; 0 ignored; 0 measured; 4451 filtered out; finished in 5.12s`

The delta is exactly the branch's two new tests
(`checked_in_conventions_compile_and_cover_hard_rust_patterns`,
`checked_in_hard_feedback_handles_every_mutator_output_shape_and_stop`),
and both pass against the current chiron pin — the rewritten tree-sitter
queries compile and fire. The cherry-pick applies to today's main with zero
conflicts, so the 131-commits-behind gap is no obstacle to the split in §5.

## 5. Recommendation

Split the commit's content in two:

1. **Land now (truth-telling, zero enforcement change):** the header
   rewrite, removal of the dead `[rust.diagnostics]`/`[rust.remediation]`
   declarations and their rule activations, the tree-sitter query upgrades
   **kept at `advise`**, the three provably-clean rules at `block`
   (`deny-attr`, `ignore-attr`, `cfg-any`), `on = "tool|stop"` everywhere,
   the new tests (with block-expectations adjusted to match), and the
   `SKILL.md` leg.
2. **Hold behind an engine feature:** `block` for `fallible-shortcuts`,
   `allow-attr`, `expect-attr`, `silent-var-rename`, `panic-macros`,
   `todo-markers` until the diagnostics engine can subtract matches whose
   span lies inside a `#[cfg(test)]`-gated item (a chiron feature: second
   query for test-gated ancestors, range subtraction). Path scoping cannot
   express this and the tree-sitter query language cannot negate ancestors —
   the carve-out is engine work, not configuration work.

The alternative — blocking only on files outside `tests/` paths — is a
false comfort: inline `mod tests` dominates, so it would either still seize
the estate or silently exempt almost nothing. Do not take it.

## 6. Outcome (2026-08-09) — candidate built, verified, and hardened

Owner approved the split and authorized a workflow (four Opus agents: one
implementer, three parallel verifiers). Candidate: **`land/conventions-truth`
@ `ff30f70`**, three commits — the WIP cherry-pick (original authorship
kept), the split, and a fix pass.

**The adversarial verifier earned its seat.** It caught the split commit
reintroducing the defect class this whole branch exists to remove: the
activation comment claimed advisory feedback "repeats at stop", but the stop
hook acts on validation failures alone — `on_stop` reads `result.outcome`
only, advisories are computed and discarded (`stop_hook.rs`, confirmed at
bytes by this reviewer). `on = "tool|stop"` on the six advisory rules was
dead configuration advertised as enforcement. Fix: advisory activations are
`on = "tool"`; the header states the asymmetry; the gate command is quoted
exactly as `CLAUDE.md` defines it. Tests now pin per-rule trigger sets
(block = `[tool, stop]`, advise = `[tool]`) and pin both activation tables
exhaustively to the nine split patterns. Red-proofed both ways: flipping one
advisory rule back to `tool|stop` fails the suite, and the pre-fix config
fails it too.

**Independent re-measurement (second verifier):** all zero-claims re-derived
from scratch and hold — deny 0, ignore 0, cfg-any 0 real attribute sites
across 1,155 files; every unwrap/expect/allow hit classified into test code;
the one non-test grep residual is a doc comment the AST matcher cannot
match. No advisory rule can block anywhere in the candidate file.

**Gates:** fmt clean; `clippy --workspace --all-targets -D warnings` clean;
full workspace battery green at the implementer's commit (30 suites, 5,980
passed, 0 failed); diagnostics suite 57/0 and fd-limit differential clean at
the fix-pass commit (one `descriptor_retention` failure under post-suite fd
residue, matching its documented register row — passes on both trees when
quiet, ran the differential in both directions).

**Follow-ups surfaced by the verifiers, not folded in (scope discipline):**

1. **`norn init conventions` still generates the dead
   `[rust.diagnostics]`/`[rust.remediation]` tables** — every newly
   initialised repo receives the exact defect this candidate removes, and
   the generator's own tests pin that output
   (`norn-cli/src/commands/init/conventions.rs:265`, tests at `:632`,
   `:649`, `:758`). Needs its own small brief.
2. **The chiron engine ask** (§5.2) — cfg(test)-span subtraction so the six
   advisory rules can graduate to block. To be written up for chiron's
   owner once this lands.

## 7. What I did not verify

- Whether chiron upstream already has a test-span carve-out in a newer rev
  than the pin (`25161bc`) — worth one look before scoping the engine work.
- The `silent-var-rename` count includes legitimate trait-impl `_param`
  idioms; the exact split between bypass and idiom was not measured.
