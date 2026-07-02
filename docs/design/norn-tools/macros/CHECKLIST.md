# Norn-Tools/Macros — Checklist

## Crate Setup

- [ ] **C1** — norn-macros crate exists as a workspace member with proc-macro = true in Cargo.toml
- [ ] **C2** — norn-macros depends only on proc-macro2, quote, and syn (full + extra-traits features)
- [ ] **C3** — norn-macros Cargo.toml declares unsafe_code = deny and pedantic clippy lints matching workspace standard
- [ ] **C4** — norn crate declares norn-macros as a dependency
- [ ] **C5** — norn crate re-exports ToolArgs derive macro via pub use norn_macros::ToolArgs in tool module
- [ ] **C6** — norn crate re-exports tool_follow_ups macro via pub use norn_macros::tool_follow_ups

## ToolArgs Derive — Core Schema Generation

- [ ] **C7** — ToolArgs derive generates a json_schema() -> serde_json::Value inherent method on the annotated struct
- [ ] **C8** — Generated schema has type: object with properties matching struct field names
- [ ] **C9** — Generated schema has additionalProperties: false by default
- [ ] **C10** — Fields without #[serde(default)] and not Option<T> appear in the required array
- [ ] **C11** — Fields with #[serde(default)] are omitted from the required array
- [ ] **C12** — Fields with #[serde(default = "fn")] are omitted from the required array
- [ ] **C13** — Doc comments on struct fields become description values on the corresponding schema properties
- [ ] **C14** — Multi-line doc comments are joined with spaces into a single description string
- [ ] **C15** — Property order in generated schema matches field declaration order

## ToolArgs Derive — Type Mapping

- [ ] **C16** — String and &str fields generate {"type": "string"}
- [ ] **C17** — bool fields generate {"type": "boolean"}
- [ ] **C18** — Signed integer fields (i8 through i64, isize) generate {"type": "integer"}
- [ ] **C19** — Unsigned integer fields (u8 through u64, usize) generate {"type": "integer", "minimum": 0}
- [ ] **C20** — Float fields (f32, f64) generate {"type": "number"}
- [ ] **C21** — Option<T> fields generate the inner type's schema and omit the field from required
- [ ] **C22** — Vec<T> fields generate {"type": "array", "items": <schema of T>}
- [ ] **C23** — HashMap<String, T> fields generate {"type": "object", "additionalProperties": <schema of T>}
- [ ] **C24** — serde_json::Value fields generate {} (any type accepted)
- [ ] **C25** — Nested structs with #[derive(ToolArgs)] generate inline object schemas with their own properties
- [ ] **C26** — Unsupported types produce compile_error! with a message naming the type and suggesting #[tool_args(schema = {...})]

## ToolArgs Derive — Enum Support

- [ ] **C27** — Unit-variant-only enums generate {"type": "string", "enum": [...]} with variant names
- [ ] **C28** — Enum variant doc comments are collected into the enum-level description
- [ ] **C29** — #[serde(rename_all)] on enums transforms variant names in the enum array
- [ ] **C30** — #[serde(rename)] on individual variants overrides the variant name in the enum array
- [ ] **C31** — Tagged enums (#[serde(tag = "...")]) generate oneOf with discriminator const on each variant
- [ ] **C32** — Untagged enums (#[serde(untagged)]) generate oneOf without discriminator
- [ ] **C33** — Adjacently tagged enums (#[serde(tag = "t", content = "c")]) generate the correct two-field schema per variant

## ToolArgs Derive — Serde Attribute Integration

- [ ] **C34** — #[serde(rename = "x")] on a field changes the property name in the schema to x
- [ ] **C35** — #[serde(rename_all = "...")] on the struct transforms all property names
- [ ] **C36** — #[serde(skip)] omits the field from the schema entirely
- [ ] **C37** — #[serde(skip_deserializing)] omits the field from the schema entirely
- [ ] **C38** — #[serde(skip_serializing)] keeps the field in the schema (it still deserializes)
- [ ] **C39** — #[serde(flatten)] merges the nested struct's properties into the parent schema
- [ ] **C40** — #[serde(flatten)] on a non-struct type produces compile_error!

## ToolArgs Derive — Override Attributes

- [ ] **C41** — #[tool_args(schema = {...})] replaces the generated schema for that field with the literal JSON
- [ ] **C42** — #[tool_args(description = "...")] overrides the doc-comment description
- [ ] **C43** — #[tool_args(skip)] excludes the field from the schema without affecting deserialization
- [ ] **C44** — #[tool_args(required)] forces inclusion in required even with #[serde(default)]
- [ ] **C45** — #[tool_args(additional_properties)] sets additionalProperties: true on the field's object schema

## ToolArgs Derive — Compile-Time Validation

- [ ] **C46** — Fields without doc comments and without #[tool_args(skip)] or #[serde(skip)] produce compile_error!
- [ ] **C47** — Compile errors from the macro point to the offending field via syn::Error::new_spanned
- [ ] **C48** — ToolArgs derive does not interfere with #[derive(Deserialize)] applied to the same struct

## Follow-Up Action Macro

- [ ] **C49** — tool_follow_ups! macro generates code that constructs Vec<FollowUpAction> from declarative action definitions
- [ ] **C50** — Each action declaration supports name, description, when (condition closure), expires, and overrides fields
- [ ] **C51** — Description field supports interpolation from output fields via format-string syntax
- [ ] **C52** — Expiry shorthand (FileModified(path), AnyFileModified, TurnScoped, Never) expands to ExpiryCondition construction
- [ ] **C53** — when condition is evaluated against ToolOutput at runtime to filter applicable follow-ups
- [ ] **C54** — Overrides are expressed as JSON literal that gets stored on the FollowUpAction

## Migration Compatibility

- [ ] **C55** — Existing tools can use #[derive(ToolArgs)] without changing their Tool trait implementation beyond input_schema()
- [ ] **C56** — Hand-written input_schema() methods coexist with ToolArgs-derived json_schema() during incremental migration
- [ ] **C57** — Generated schemas for EditArgs, BashArgs, and PatchArgs are byte-identical to current hand-written schemas
