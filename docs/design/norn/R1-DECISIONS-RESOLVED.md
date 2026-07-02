# R1 Open Decisions — resolved defaults (2026-07-02)

Resolutions for the seven open decisions in `BRIEF-R1-ASSEMBLY-UNIFICATION.md` §7.
Owner was away at the decision point; these are the recommended defaults, applied
autonomously per the campaign's standing "keep going" directive. All are reversible on
the `hardening/final-state` branch before merge. **Owner may override any of these; flag
on review.**

- **D1 — Session-hook ownership: Agent::run fires them.** `Agent::run` fires
  `on_session_start`/`on_session_end` with `info.session_id`; `into_parts` drivers (TUI,
  print step-loop) get explicit `fire_session_start`/`fire_session_end` helpers on
  `AgentParts`. Rationale: fixes the confirmed bug at the source (embedded agents,
  including all Meridian paths, currently fire no session hooks); Meridian can drop its
  hand-firing in `finalize_norn_session`.

- **D2 — Root registry registration: opt-in.** `build()` reserves the `AgentRegistry`
  "/root" entry only when BOTH `.agent_registry()` and `.register_root(path, role)` are
  set. Never mandatory. Rationale: embedders like Meridian wire no coordination and must
  not be forced to register a root; the TUI/print paths opt in.

- **D3 — Terminal-reclamation control: `.terminal_reclamation(bool)`, default `true`.**
  `true` preserves today's unconditional `install_terminal_reclamation` behavior (the
  existing documented default, not an invented value); the TUI passes `false` (its status
  panel owns reclamation).

- **D4 — CLI session front door: `.open_session`.** `builder_from_cli` uses
  `.open_session(SessionManager, SessionSpec, DurabilityPolicy::Flush)` at build time;
  `--no-session` maps to `.session(EventStore::new())`. Print's post-build ordering for
  `debug_dump_file` is preserved by reading `parts.info.session_id`.

- **D5 — Skill-tool registration gate: load_runtime_base path only.** Register `SkillTool`
  where the catalog + `SkillToolConfig` exist (the `load_runtime_base` extension path),
  matching the CLI's `!catalog.is_empty()` gate. Library agents built without
  `load_runtime_base` have no catalog, so no skill tool — correct.

- **D6 — Meridian migration scope: OUT of scope (norn only).** R1 exposes the library
  surfaces so Meridian *can* delete its copies (`NornSessionStore` ~527 L, workflow
  session-index copy) via `.open_session`, but the actual Meridian edits are a separate
  PR. This campaign stays norn-only and mergeable independently. Capability-discovery
  helper (§C) and shared provider defaults (§D) are NOT added to the library now — left as
  intentional Meridian copies until a future ask.

- **D7 — `event_schemas` / `variables` on the builder: yes.** Add `.event_schemas()` and
  `.variables()` to `AgentBuilder` (additive; the CLI needs them and the builder is the
  assembler). Minor public-surface expansion, acceptable.
