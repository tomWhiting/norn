# Norn-Tools/Macros — User Stories

## AI Agent — Tool Author Implementing a New Tool

**S1.** As a tool author, I want to derive the JSON Schema from my args struct so that I do not have to write and maintain a separate json!({...}) block that mirrors the struct.

**S2.** As a tool author, I want doc comments on struct fields to become schema descriptions so that documentation and the model-visible schema stay in sync automatically.

**S3.** As a tool author, I want a compile error when I add a field without a doc comment so that I cannot accidentally ship a schema property with no description.

**S4.** As a tool author, I want to override the generated schema for a single field so that I can handle cases where the Rust type does not directly map to the desired JSON Schema.

**S5.** As a tool author, I want the macro to respect my serde attributes so that the schema matches how the struct actually deserializes, without manual synchronization.

## AI Agent — Tool Author Registering Follow-Up Actions

**S6.** As a tool author, I want to declare follow-up actions on my tool result using a declarative macro so that I do not have to manually construct FollowUpAction structs.

**S7.** As a tool author, I want to specify when a follow-up action is available using a condition on the output so that only relevant actions appear in the tool result.

**S8.** As a tool author, I want to specify expiry conditions in shorthand so that I do not have to construct ExpiryCondition enum variants by hand.

**S9.** As a tool author, I want follow-up descriptions to interpolate values from the tool output so that the model sees concrete, actionable descriptions.

## AI Agent — Using a Tool with Follow-Up Actions

**S10.** As an AI agent, I want to see follow-up actions in a tool result so that I can act on deferred options without re-generating the original tool call.

**S11.** As an AI agent, I want follow-up action descriptions to name the specific file or operation so that I can make informed decisions about whether to invoke the follow-up.

## Human Developer — Maintaining the Tool Codebase

**S12.** As a developer maintaining tools, I want schema generation to be derived from the args struct so that adding or removing a field automatically updates the schema.

**S13.** As a developer reviewing a tool PR, I want to see the schema defined by struct fields and doc comments so that I can review the schema and the deserialization target in one place.

**S14.** As a developer migrating an existing tool, I want to add #[derive(ToolArgs)] without changing anything else about the tool so that I can migrate tools one at a time.

## Human Developer — Debugging Tool Schema Issues

**S15.** As a developer debugging a schema mismatch, I want the macro to produce compile errors that point to the specific field with the problem so that I do not have to guess which field caused the issue.

**S16.** As a developer using an unsupported type, I want a compile error that names the type and suggests the override syntax so that I know exactly how to fix it.
