# Ablative stack logical contract candidate

**Status:** NS0 review candidate. Logical shape and fixture cases are frozen for
review; repository ownership, canonical encoding, digest construction, and
authority allocators are not accepted.

**Date:** 2026-07-19

**Current-state evidence:**
[`ablative-stack-identity-inventory.md`](ablative-stack-identity-inventory.md)

**Candidate fixtures:**
[`../reviews/evidence/ns0/contract-fixture-manifest.json`](../reviews/evidence/ns0/contract-fixture-manifest.json)

## 1. Boundary

This candidate defines the smallest logical vocabulary needed to correlate
records across Norn, Aion, Frame, and later stack services without replacing
their native authorities. It is not a production schema, wire protocol,
authorization model, storage migration, or compatibility layer.

The terms in this document have three explicit states:

- **Observed** means the pinned source inventory demonstrates an existing
  native authority or absence.
- **Proposed** means the logical shape is a review candidate only.
- **Owner decision** means implementation is blocked until the named domain or
  stack owner selects the authority and lifecycle.

No JSON file in the fixture set is an accepted canonical byte sequence. The
JSON is a readable carrier for the logical cases. Consequently, the fixture
SHA-256 values prove file integrity only; they are not record, schema, event,
or artifact digests.

## 2. Observed constraints

The pinned inventory establishes these constraints:

1. Native identifiers are scoped by their owning domain. Equal strings,
   integers, UUIDs, or 32-byte values are not interchangeable.
2. Native stores remain authoritative. A projection cannot rewrite Norn
   transcript history, Aion workflow history, Frame component state, or any
   other domain record.
3. Semantic identity, delivery position, and authorization are separate.
4. There is no stack-wide total order. Native order, direct causality, delivery
   sequence, and display timestamps answer different questions.
5. Norn format 2 records at most one parent event. A multi-parent logical shape
   is representable here but unsupported by current Norn persistence.
6. Unknown native payloads must remain opaque and non-executable.
7. Norn action attribution is not an authenticated mutation principal.
8. The proposed component, supervisor, session-run, producer, and delivery
   epochs do not yet have accepted cross-stack allocators.

## 3. Proposed logical types

Field names below describe the logical candidate. They do not freeze JSON key
order, integer representation, optional-field encoding, or canonical bytes.

### 3.1 `DomainRefV1`

```text
DomainRefV1
  domain
  kind
  native_value
  scope[]
```

- `domain` names the authority that owns the reference.
- `kind` names an owner-defined reference class within that domain.
- `native_value` preserves the owner's structured value; it is not coerced to
  a universal string.
- `scope` carries role-labelled `DomainRefV1` values required to disambiguate
  the native value. It is empty only when the owner defines the value as
  self-scoping.

`DomainRefV1` provides naming and correlation only. Possessing or presenting
one grants no authority.

### 3.2 `EventRecordV1`

```text
EventRecordV1
  event_id: DomainRefV1
  event_kind
  schema_ref: DomainRefV1
  producer_ref: DomainRefV1
  producer_epoch: DomainRefV1
  subjects[]: { role, ref }
  actor_ref?: DomainRefV1
  direct_causes[]: { role, event_ref }
  correlation_ref?: DomainRefV1
  links[]: { role, ref }
  occurred_at?
  payload
```

The record is an immutable projection. Its payload retains a typed native value
or an owner-defined artifact reference. `occurred_at` is display metadata, not
ordering authority. `actor_ref` is attribution unless a separate verified
authorization record proves otherwise.

Projection identity and cardinality are owner decisions. A native event may
map one-to-one or to a stable set of declared projection kinds, but a projector
must not mint a different identity on each read.

### 3.3 `RelationRecordV1`

```text
RelationRecordV1
  relation_id: DomainRefV1
  relation_kind
  schema_ref: DomainRefV1
  asserting_producer_ref: DomainRefV1
  asserting_producer_epoch: DomainRefV1
  endpoints[]: { role, ref }
  supporting_native_refs[]: DomainRefV1
  direct_causes[]: { role, event_ref }
  supersedes_relation_id?: DomainRefV1
  retracts_relation_id?: DomainRefV1
  payload?
```

A relation is asserted by one named native authority and cites its supporting
native record or evidence. Later discovery does not mutate an earlier event's
links. Supersession and retraction are new immutable records.

Relation identity, endpoint ordering, and which domain may assert each
relation kind remain owner decisions.

### 3.4 Delivery and cursor

```text
DeliveryV1
  stream_ref: DomainRefV1
  stream_epoch: DomainRefV1
  sequence
  record_ref: DomainRefV1
  record_bytes?

DeliveryCursorV1
  stream_ref: DomainRefV1
  stream_epoch: DomainRefV1
  sequence
```

Delivery retries may repeat a record but cannot change its semantic identity.
Sequence is meaningful only inside the exact `(stream_ref, stream_epoch)`.
Neither sequence nor stream epoch is a producer epoch or causal parent.

This shape is included so semantic records do not absorb transport state. No
delivery fixture is canonical in NS0 because the delivery owner and epoch
allocator remain unresolved.

## 4. Authority rules

The candidate is governed by these fail-closed rules:

1. A wire request carries request data and presented credentials, never a
   caller-asserted authorization decision.
2. Mutation authority must be constructed locally from an authenticated
   principal, invocation surface, delegation chain, granted capabilities,
   target epoch, and authorization evidence.
3. `producer_ref` identifies who emitted a semantic record; it is not an
   authenticated actor, delivery participant, or signing key.
4. `actor_ref` records attributed agency only. Authorization evidence must use
   a separate owner-approved contract.
5. A signature or attestation proves bytes and key possession. The consuming
   domain still decides principal meaning, grants, freshness, and revocation.
6. Unknown payload kinds remain opaque and non-executable until their owning
   domain accepts a decoder and policy.

## 5. Fixture dispositions

| Fixture | What it binds | Current status |
|---|---|---|
| Norn event | One domain-qualified Norn `UserMessage` projection with its owning session incarnation and one observed parent | Native concepts observed; projection identity and producer epoch proposed |
| Aion event | One workflow-history event scoped by workflow and run, preserving workflow-local sequence separately from event identity | Native concepts observed; projection identity and producer epoch proposed |
| Frame contribution event | One proposed contribution-snapshot publication correlated with observed `ComponentId`; it does not promote lifecycle sequence or storage incarnation | Contribution event and loaded-generation allocator require Frame owner decision |
| Durable relation | One Aion-owned workflow-run to Norn-session relation with role-labelled endpoints and supporting native references | Logical relation proposed; asserting authority and identity require owner decision |
| Opaque unknown | An unknown native variant retains every fixture field without interpretation and is marked non-executable | Fail-closed behavior required; canonical bytes unresolved |
| Multi-parent example | Two role-labelled direct parents without a total-order claim | Logical example only; explicitly unsupported by Norn format 2 |

The manifest records a content hash for each fixture file so reviewers can
verify which candidate they inspected. Those hashes must never be copied into
`schema_ref`, `event_id`, `relation_id`, or `record_ref`.

## 6. M0 stop decisions

The following decisions are unresolved and block acceptance or decoders:

1. Which repository owns the shared logical types and generated bindings.
2. Which source owners define `WorkspaceRef`, `SnapshotRef`, `ChangeRef`,
   `DiagnosticRef`, `SessionRef`, and `AgentRef`.
3. Who allocates and fences `ComponentGenerationId`, `BrowserActivationId`,
   `SupervisorInstanceId`, `SessionRunEpoch`, `ProducerEpoch`, and
   `DeliveryStreamEpoch`.
4. The stable native-to-projection identity construction and cardinality for
   each event kind.
5. The asserting authority, identity construction, and endpoint ordering for
   each durable relation kind.
6. The authenticated Norn mutation principal and authorization-evidence
   contract.
7. Canonical encoding, number/text normalization, optional-field rules,
   domain separation, digest algorithm, and version evolution.
8. Whether unordered multi-parent edges are sorted, and by which accepted
   canonical reference representation.

Until these are accepted, do not add production shared types, Rust or
TypeScript fixture decoders, runtime integration, compatibility shims, or NS2
projection code.
