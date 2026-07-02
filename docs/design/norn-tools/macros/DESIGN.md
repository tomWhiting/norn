---
type: design
cluster: norn-tools/macros
title: "Norn Macros: Proc-Macro Crate for Tool Schema and Follow-Up Generation"
---

# Norn Macros: Proc-Macro Crate for Tool Schema and Follow-Up Generation

## Intention

Norn-macros exists so that tool authors declare their input schema once, as a Rust struct, and the macro generates the JSON Schema that `input_schema()` returns. When this is done, the schema is always in sync with the deserialization target. A field added to the args struct appears in the schema. A field removed from the struct disappears from the schema. Doc comments on fields become schema descriptions. Serde attributes that affect serialization (`default`, `rename`, `skip`) affect the schema identically. The tool author writes a struct, not a JSON blob.

The same crate provides a declarative way to register follow-up actions on tool results — the lifecycle extension described in the follow-up design (D9). Instead of hand-building `Vec<FollowUpAction>` in every tool's `register_follow_ups` method, the tool author declares follow-ups as attributed methods or static descriptions, and the macro generates the registration code.

## Problem

Every tool in the norn crate implements `input_schema()` by hand-writing a `serde_json::json!({...})` block that mirrors the args struct. As of today, 17+ tools do this. The problems:

1. **Drift.** The args struct and the schema are two independent descriptions of the same thing. Add a field to the struct but forget to add it to the schema: the model never sends it. Add it to the schema but misspell the field name: deserialization fails silently with `serde(default)` or loudly without it. Both have happened.

2. **Boilerplate.** A simple three-field tool (EditTool) requires 15 lines of schema JSON. A complex tool (SearchTool, ForkTool) requires 60-80 lines. Across 17+ tools, that is over 500 lines of hand-written JSON Schema that exists only to describe structs that are already fully described by their Rust definitions.

3. **Description maintenance.** Schema property descriptions are string literals buried inside `json!({})` blocks. They are invisible to documentation tools, hard to review, and easy to leave stale. Doc comments on struct fields are the natural place for descriptions, but today they are disconnected from the schema.

4. **Follow-up registration.** The follow-up design (D9) adds a `register_follow_ups` lifecycle phase. Without macros, every mutation tool must manually construct `FollowUpAction` structs with action names, descriptions, expiry conditions, and argument overrides. This is the same class of problem: structured data described in code that should be described declaratively.

## Solution

### D1: Separate proc-macro crate

`norn-macros` is a proc-macro crate (`lib.rs` with `proc-macro = true` in Cargo.toml). It compiles before `norn` and cannot depend on `norn`. The generated code references types from `norn` by full path (e.g., `::serde_json::Value`, `::serde_json::json!`). The `norn` crate re-exports the derive macros so tool authors write `use norn::tool::ToolArgs` (or a prelude), not `use norn_macros::ToolArgs`.

Dependencies: `proc-macro2`, `quote`, `syn` (with `full` and `extra-traits` features). No other dependencies. Specifically, no `serde`, no `serde_json`, no runtime crates. The generated code uses `serde_json` types, but the macro crate itself only manipulates token streams.

### D2: `#[derive(ToolArgs)]` for JSON Schema generation

Applied to a tool's args struct:

```rust
/// Arguments for the edit tool.
#[derive(Debug, Deserialize, ToolArgs)]
struct EditArgs {
    /// Absolute path to the file to edit.
    path: String,
    /// Exact text to find and replace. Must match exactly once.
    old_string: String,
    /// Replacement text.
    new_string: String,
}
```

Generates an inherent impl:

```rust
impl EditArgs {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact text to find and replace. Must match exactly once."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                }
            },
            "additionalProperties": false
        })
    }
}
```

The tool's `input_schema()` method becomes:

```rust
fn input_schema(&self) -> serde_json::Value {
    EditArgs::json_schema()
}
```

### D3: Type mapping rules

The macro maps Rust types to JSON Schema types at compile time. The mapping is closed — only supported types are accepted. Unsupported types produce a compile error with a clear message.

| Rust type | JSON Schema | Notes |
|-----------|-------------|-------|
| `String`, `&str` | `{"type": "string"}` | |
| `bool` | `{"type": "boolean"}` | |
| `i8`..`i64`, `isize` | `{"type": "integer"}` | |
| `u8`..`u64`, `usize` | `{"type": "integer", "minimum": 0}` | |
| `f32`, `f64` | `{"type": "number"}` | |
| `Option<T>` | Schema of `T` with field omitted from `required` | Not nullable — absent means None |
| `Vec<T>` | `{"type": "array", "items": <schema of T>}` | |
| `HashMap<String, T>` | `{"type": "object", "additionalProperties": <schema of T>}` | Key must be String |
| `serde_json::Value` | `{}` (any) | Escape hatch for untyped fields |
| Struct with `#[derive(ToolArgs)]` | Inline object schema | Recursive |
| Enum with `#[derive(ToolArgs)]` | See D4 | |

### D4: Enum handling

Enums come in two forms relevant to tool schemas:

**String enums** (unit variants only):

```rust
#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "snake_case")]
enum SearchMode {
    /// Search file contents using regex.
    Content,
    /// Search file names using glob patterns.
    Files,
    /// Search using AST structural queries.
    Ast,
}
```

Generates `{"type": "string", "enum": ["content", "files", "ast"]}` with variant descriptions collected into a top-level `description` field listing each option.

**Tagged enums** (variants with data, `#[serde(tag = "...")]`):

```rust
#[derive(Deserialize, ToolArgs)]
#[serde(tag = "type")]
enum TaskAction {
    Create { title: String, description: String },
    Update { task_id: String, status: String },
}
```

Generates a `oneOf` schema with the tag discriminator. Each variant becomes an object schema with the tag field as a const.

Untagged enums (`#[serde(untagged)]`) are supported but generate `oneOf` without a discriminator. Adjacently tagged (`#[serde(tag = "t", content = "c")]`) is also supported.

### D5: Serde attribute integration

The macro inspects serde attributes on the struct and its fields to ensure the schema matches deserialization behavior:

| Serde attribute | Schema effect |
|----------------|---------------|
| `#[serde(rename = "x")]` | Property name in schema becomes `"x"` |
| `#[serde(rename_all = "...")]` | All property names transformed |
| `#[serde(default)]` on field | Field omitted from `required` |
| `#[serde(default = "fn")]` on field | Field omitted from `required` |
| `#[serde(skip)]` | Field omitted from schema entirely |
| `#[serde(skip_deserializing)]` | Field omitted from schema entirely |
| `#[serde(skip_serializing)]` | Field included (it still deserializes) |
| `#[serde(flatten)]` | Nested struct's properties merged into parent |
| `#[serde(alias = "x")]` | No schema effect (aliases are runtime-only) |
| `#[serde(deny_unknown_fields)]` | `"additionalProperties": false` (already the default) |

The macro does NOT attempt to parse or interpret custom `deserialize_with` functions. If a field has `#[serde(deserialize_with = "...")]`, the macro uses the field's declared Rust type for schema generation. If the custom deserializer accepts a different wire format, the author must use `#[tool_args(schema = ...)]` to override (see D6).

### D6: Schema overrides via `#[tool_args(...)]` attributes

For cases where the automatic mapping is insufficient:

```rust
#[derive(Deserialize, ToolArgs)]
struct PatchArgs {
    /// Unified-diff patch text.
    patch: String,
    /// Directory to resolve relative paths against.
    #[serde(default)]
    working_dir: Option<String>,
    /// Matching mode for hunk application.
    #[serde(default = "default_mode")]
    #[tool_args(schema = {"type": "string", "enum": ["strict", "structural", "auto"]})]
    mode: PatchMode,
}
```

Available overrides:

- `#[tool_args(schema = {...})]` — replace the generated schema for this field with a literal JSON value.
- `#[tool_args(description = "...")]` — override the doc-comment description.
- `#[tool_args(skip)]` — exclude from schema (same as `#[serde(skip)]` but schema-only).
- `#[tool_args(required)]` — force inclusion in `required` even with `#[serde(default)]`.
- `#[tool_args(additional_properties)]` — set `additionalProperties: true` on this struct/field instead of false.

### D7: `additionalProperties: false` by default

All generated object schemas include `"additionalProperties": false` unless overridden. This matches the current hand-written convention across all 17+ tools and ensures models do not send unexpected fields.

### D8: Follow-up action registration macro

A declarative macro (not derive) for registering follow-up actions on tool results. This implements the `register_follow_ups` lifecycle phase from the follow-up design (D9).

```rust
tool_follow_ups! {
    tool: EditTool,
    actions: [
        {
            name: "undo",
            description: "Revert {path} to pre-edit content",
            when: |output| output.content["committed"] == true,
            expires: FileModified(output.content["path"].as_str()),
            overrides: {},
        },
        {
            name: "apply_with_allow_broken_ast",
            description: "Re-apply edit, allowing broken AST",
            when: |output| output.content["kind"] == "edit_blocked_by_ast",
            expires: FileModified(output.content["path"].as_str()),
            overrides: { "flags": ["allow_broken_ast"] },
        },
    ]
}
```

The macro generates an implementation of the `register_follow_ups` method (or a helper called by it) that:

1. Evaluates each `when` condition against the tool output.
2. For matching actions, constructs `FollowUpAction` with the name, description (with interpolation from output fields), expiry condition, and argument overrides.
3. Returns `Vec<FollowUpAction>`.

The macro is a function-like proc macro (`#[proc_macro]`), not `macro_rules!` and not a derive macro. Follow-up declarations are per-tool-struct, not per-args-struct, and they reference the tool's output shape, not its input shape.

**Silent-false semantics for `when` closures:** `serde_json::Value` indexing returns `Value::Null` for missing keys, and comparisons against `Null` return false. This means follow-ups whose `when` conditions reference absent output fields are silently not registered. This is intentional — a follow-up that cannot evaluate its condition does not apply to that result shape. For example, `output.content["committed"] == true` evaluates to false when the output has no "committed" field, which correctly suppresses the undo follow-up for non-mutation results.

### D9: No runtime dependency from norn-macros

The proc-macro crate generates code that references:
- `::serde_json::json!` and `::serde_json::Value` (for schema construction)
- Follow-up types from `::norn::tool::follow_up` (for follow-up registration)

It does NOT link against these crates at compile time. It emits token streams that the consuming crate resolves. This is standard proc-macro hygiene.

### D10: Compile-time validation

The macro performs compile-time checks:

1. **All fields must map to a known schema type.** Unknown types produce `compile_error!("ToolArgs: unsupported type `Foo`. Use #[tool_args(schema = {...})] to provide a manual schema.")`.
2. **Doc comments are required on all fields.** Missing doc comments produce `compile_error!("ToolArgs: field `path` has no doc comment. Add a /// comment to provide the schema description.")`. Fields with `#[tool_args(skip)]` or `#[serde(skip)]` are exempt.
3. **`#[serde(flatten)]` on non-struct types is rejected** with a clear error.
4. **Enum variants without doc comments produce a warning** (not an error, since variant descriptions are concatenated into the enum description and individual descriptions are optional).

### D11: Re-export path

The `norn` crate re-exports the derive macro:

```rust
// In norn/src/tool/mod.rs or norn/src/lib.rs
pub use norn_macros::ToolArgs;
```

Tool authors write:

```rust
use norn::tool::ToolArgs;

#[derive(Deserialize, ToolArgs)]
struct MyToolArgs { ... }
```

The follow-up macro is also re-exported:

```rust
pub use norn_macros::tool_follow_ups;
```

## Prerequisites

**PR0: Lifecycle transparency fix.** `ToolError::PostValidationFailed` must carry an optional `committed_output`. The follow-up macros generate code that references committed state in tool outputs. Without PR0, follow-up actions on failed-but-committed mutations would reference data that the lifecycle discarded.

**PR2: FollowUpAction types.** `FollowUpAction`, `ExpiryCondition`, and `BeforeContentSource` must be defined in `norn::tool::follow_up` before the `tool_follow_ups!` macro can generate code that constructs them. These types are runtime types owned by the norn crate.

## Goals

G1. A tool's `input_schema()` can be implemented as a one-line delegation to `Args::json_schema()`, where the schema is derived from the args struct definition.

G2. Doc comments on args struct fields become JSON Schema `description` values, keeping documentation and schema in sync.

G3. Serde attributes (`rename`, `default`, `skip`, `flatten`, `rename_all`, `tag`) are respected in schema generation, so the schema matches deserialization behavior without manual synchronization.

G4. Complex types (Option, Vec, HashMap, nested structs, enums) produce correct JSON Schema without manual overrides.

G5. Follow-up actions can be declared per-tool using a declarative macro, eliminating manual `FollowUpAction` construction in `register_follow_ups`.

G6. The proc-macro crate compiles with only `proc-macro2`, `quote`, and `syn` as dependencies — no runtime crates.

G7. Existing tools can be migrated incrementally — `#[derive(ToolArgs)]` can coexist with hand-written `input_schema()` methods during transition.

## Non-Goals

NG1. Generating the entire `Tool` trait implementation. `ToolArgs` generates only `json_schema()`. The tool author still implements `name()`, `description()`, `effect()`, `execute()`, and lifecycle methods. A full `#[derive(Tool)]` is a separate, larger design that requires solving tool registration, effect declaration, and async execution patterns.

NG2. Runtime schema generation. The macro generates a function that builds the schema at call time using `serde_json::json!`. It does not generate a const or lazy_static. The schema is cheap to build (small JSON objects) and tools are registered once at startup.

NG3. Replacing `schemars`. The macro is purpose-built for tool input schemas. It does not implement the full JSON Schema specification (no `$ref`, no `$defs`, no `if/then/else`, no `patternProperties`). It generates the subset that the OpenAI and Anthropic tool-calling APIs accept.

NG4. Output schema generation. Tool output shapes are ad-hoc `serde_json::Value` objects, not deserialization targets. Deriving output schemas is a different problem with different constraints (dynamic shapes, error variants, diagnostic payloads).

NG5. Validation code generation. The macro generates schema descriptions, not validators. Input validation is handled by `jsonschema` (already in the workspace) at the framework level, not per-tool.

## Structure

```
crates/norn-macros/
  Cargo.toml                     -- proc-macro crate, deps: proc-macro2, quote, syn
  src/
    lib.rs                       -- #[proc_macro_derive(ToolArgs)] + tool_follow_ups! entry points
    tool_args/
      mod.rs                     -- ToolArgs derive orchestration
      parse.rs                   -- syn parsing: struct fields, serde attrs, tool_args attrs, doc comments
      schema.rs                  -- type-to-JSON-Schema mapping, schema construction via quote
      enum_schema.rs             -- enum variant handling: string enums, tagged, untagged, adjacent
      validate.rs                -- compile-time checks: required docs, supported types, serde compat
    follow_up/
      mod.rs                     -- tool_follow_ups! macro orchestration
      parse.rs                   -- action declaration parsing: name, description, when, expires, overrides
      codegen.rs                 -- FollowUpAction construction code generation
```

In `crates/norn/`:

```
src/tool/
  mod.rs                         -- adds `pub use norn_macros::ToolArgs;` re-export
  follow_up.rs                   -- FollowUpAction, ExpiryCondition, FollowUpRegistry types (NEW)
```

## Constraints

CO1. The proc-macro crate must not depend on any norn runtime crate. It depends only on `proc-macro2`, `quote`, and `syn`. Generated code references external types by absolute path (`::serde_json::Value`, `::norn::tool::follow_up::FollowUpAction`).

CO2. Generated schemas must be byte-identical to the hand-written schemas they replace for the same field set. This means: `"additionalProperties": false` on all objects, `required` array contains only non-default non-optional fields, property order matches field declaration order (using `serde_json`'s `preserve_order` feature which is already enabled in the workspace).

CO3. The derive macro must not interfere with `#[derive(Deserialize)]`. Both derives can be applied to the same struct independently. The macro reads serde attributes but does not modify the struct or add serde attributes.

CO4. Compile errors from the macro must point to the offending field or attribute, not to the macro invocation site. Use `syn::Error::new_spanned` for all diagnostics.

CO5. The macro must handle all type patterns currently used across the 17+ existing tool args structs without requiring schema overrides. Migration of existing tools should require only adding `#[derive(ToolArgs)]` and replacing the `json!({...})` block with `Args::json_schema()`.

CO6. Follow-up action types (`FollowUpAction`, `ExpiryCondition`) must be defined in the `norn` crate (not in `norn-macros`), since they are runtime types used by the tool lifecycle. The macro generates code that constructs these types.

CO7. The `tool_follow_ups!` macro must not require the tool author to manually construct expiry condition enum variants. The macro provides shorthand syntax (`FileModified(path)`, `AnyFileModified`, `TurnScoped`, `Never`) that expands to the appropriate enum construction.
