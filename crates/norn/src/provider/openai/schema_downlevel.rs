//! Down-levels canonical tool parameter schemas to the subset `OpenAI`
//! accepts for function tools.
//!
//! The Responses API requires a function's `parameters` to be a plain
//! object schema: root `type: "object"` and none of `oneOf` / `anyOf` /
//! `allOf` / `enum` / `const` / `not` at the top level. Norn's canonical
//! tool schemas are richer — composite tools derive a root `oneOf` of
//! per-command object variants (see
//! [`CompositeTool`](crate::tool::composite::CompositeTool)). This module
//! rewrites such schemas into the flat shape `OpenAI` accepts while
//! preserving the guidance the model needs:
//!
//! * The command discriminator (every variant carries it as a required
//!   string `const`) becomes a root string property with an `enum` of the
//!   command values and a description listing each command's doc text and
//!   required fields.
//! * All variant fields merge into one optional property set; fields whose
//!   schemas conflict across variants (beyond their descriptions) merge
//!   into a nested `anyOf`, which is allowed below the top level.
//! * `required` keeps only the fields required by every variant — the
//!   discriminator, in practice; per-command requirements move into the
//!   discriminator's description.
//!
//! The flattened schema is deliberately looser than the canonical one.
//! Exact per-command validation still happens at dispatch, where serde
//! rejects malformed arguments with a typed `invalid_arguments` failure
//! the model can correct from.
//!
//! A root `anyOf` flattens by the same rules — the flat schema is already
//! a strictly looser union of the variants, so the exactly-one versus
//! at-least-one distinction is immaterial.
//!
//! Already-compliant schemas pass through untouched (gaining a root
//! `type: "object"` when they omit it — function arguments are always
//! JSON objects, so the annotation is universally sound). A schema that
//! is neither compliant nor a flattenable `oneOf`/`anyOf` — an externally
//! sourced schema (e.g. an MCP server's), or an untagged root enum with
//! non-object variants — is sent verbatim after a `tracing::warn!`, so
//! the provider's rejection is preceded by a local diagnostic naming the
//! tool.

use serde_json::{Map, Value};

/// Keywords `OpenAI` rejects at the top level of function parameters.
const FORBIDDEN_ROOT_KEYWORDS: [&str; 6] = ["oneOf", "anyOf", "allOf", "enum", "const", "not"];

/// Rewrites `schema` into a shape the Responses API accepts for function
/// parameters, per the module-level rules.
pub(crate) fn downlevel_function_parameters(tool_name: &str, schema: &Value) -> Value {
    let Some(root) = schema.as_object() else {
        tracing::warn!(
            tool = tool_name,
            "function parameter schema is not a JSON object; sending verbatim"
        );
        return schema.clone();
    };

    let forbidden: Vec<&str> = FORBIDDEN_ROOT_KEYWORDS
        .iter()
        .copied()
        .filter(|keyword| root.contains_key(*keyword))
        .collect();

    if forbidden.is_empty() {
        return match root.get("type") {
            Some(Value::String(kind)) if kind == "object" => schema.clone(),
            None => {
                let mut fixed = root.clone();
                fixed.insert("type".to_owned(), Value::String("object".to_owned()));
                Value::Object(fixed)
            }
            Some(other) => {
                tracing::warn!(
                    tool = tool_name,
                    root_type = %other,
                    "function parameter schema root is not an object schema; sending verbatim"
                );
                schema.clone()
            }
        };
    }

    // `anyOf` flattens by the same rules: the flat schema is already a
    // strictly looser union of the variants, so the distinction between
    // exactly-one and at-least-one match is immaterial here.
    if let [keyword @ ("oneOf" | "anyOf")] = forbidden.as_slice()
        && let Some(flattened) = flatten_variants(root, keyword)
    {
        return flattened;
    }

    tracing::warn!(
        tool = tool_name,
        keywords = ?forbidden,
        "function parameter schema cannot be down-leveled for OpenAI; sending verbatim"
    );
    schema.clone()
}

/// The pieces of one `oneOf`/`anyOf` variant needed for flattening.
struct VariantParts<'a> {
    properties: &'a Map<String, Value>,
    required: Vec<&'a str>,
    additional_properties_false: bool,
    description: Option<&'a str>,
}

/// Decomposes a variant, or `None` when it is not an object schema with
/// well-formed `properties`/`required` (which makes the whole
/// `oneOf`/`anyOf` unflattenable).
fn variant_parts(variant: &Value) -> Option<VariantParts<'_>> {
    let map = variant.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let properties = map.get("properties")?.as_object()?;
    let required = match map.get("required") {
        None => Vec::new(),
        Some(value) => value
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?,
    };
    Some(VariantParts {
        properties,
        required,
        additional_properties_false: map.get("additionalProperties").and_then(Value::as_bool)
            == Some(false),
        description: map.get("description").and_then(Value::as_str),
    })
}

/// Finds the discriminator property: present and required in every variant,
/// a string `const` in each, with pairwise-distinct values. Returns the
/// property name and the per-variant values in variant order.
fn find_discriminator<'a>(parts: &[VariantParts<'a>]) -> Option<(String, Vec<&'a str>)> {
    let first = parts.first()?;
    'candidates: for name in first.properties.keys() {
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            if !part.required.contains(&name.as_str()) {
                continue 'candidates;
            }
            let Some(value) = part
                .properties
                .get(name)
                .and_then(Value::as_object)
                .and_then(|schema| schema.get("const"))
                .and_then(Value::as_str)
            else {
                continue 'candidates;
            };
            if values.contains(&value) {
                continue 'candidates;
            }
            values.push(value);
        }
        return Some((name.clone(), values));
    }
    None
}

/// One line of the discriminator description: the command value, its doc
/// text, and its required fields. `None` when the line would carry no
/// information beyond the enum value itself.
fn command_line(value: &str, part: &VariantParts<'_>, discriminator: &str) -> Option<String> {
    let requires: Vec<&str> = part
        .required
        .iter()
        .copied()
        .filter(|name| *name != discriminator)
        .collect();
    let description = part.description.unwrap_or("");
    match (description.is_empty(), requires.is_empty()) {
        (true, true) => None,
        (true, false) => Some(format!("{value} (requires: {})", requires.join(", "))),
        (false, true) => Some(format!("{value}: {description}")),
        (false, false) => Some(format!(
            "{value}: {description} (requires: {})",
            requires.join(", ")
        )),
    }
}

/// A field schema stripped of its `description`, for conflict comparison —
/// fields that differ only in doc text are the same field, not a conflict.
fn without_description(schema: &Value) -> Value {
    match schema.as_object() {
        Some(map) => {
            let mut stripped = map.clone();
            stripped.remove("description");
            Value::Object(stripped)
        }
        None => schema.clone(),
    }
}

/// Flattens a root `oneOf`/`anyOf` of object variants into a plain object
/// schema, or `None` when any variant is not flattenable.
fn flatten_variants(root: &Map<String, Value>, keyword: &str) -> Option<Value> {
    let variants = root.get(keyword)?.as_array()?;
    if variants.is_empty() {
        return None;
    }
    let parts: Vec<VariantParts<'_>> = variants
        .iter()
        .map(variant_parts)
        .collect::<Option<Vec<_>>>()?;
    let discriminator = find_discriminator(&parts);

    let mut properties = Map::new();
    if let Some((name, values)) = &discriminator {
        let lines: Vec<String> = parts
            .iter()
            .zip(values)
            .filter_map(|(part, value)| command_line(value, part, name))
            .collect();
        let mut schema = Map::new();
        schema.insert("type".to_owned(), Value::String("string".to_owned()));
        schema.insert(
            "enum".to_owned(),
            Value::Array(
                values
                    .iter()
                    .map(|value| Value::String((*value).to_owned()))
                    .collect(),
            ),
        );
        if !lines.is_empty() {
            schema.insert("description".to_owned(), Value::String(lines.join("\n")));
        }
        properties.insert(name.clone(), Value::Object(schema));
    }

    // Distinct schemas per field across variants, compared modulo
    // description, keeping the first full occurrence of each.
    let mut merged: Vec<(&String, Vec<&Value>)> = Vec::new();
    for part in &parts {
        for (name, field_schema) in part.properties {
            if discriminator.as_ref().is_some_and(|(disc, _)| disc == name) {
                continue;
            }
            let position = merged.iter().position(|(seen, _)| *seen == name);
            let index = if let Some(index) = position {
                index
            } else {
                merged.push((name, Vec::new()));
                merged.len() - 1
            };
            if let Some((_, schemas)) = merged.get_mut(index)
                && !schemas
                    .iter()
                    .any(|seen| without_description(seen) == without_description(field_schema))
            {
                schemas.push(field_schema);
            }
        }
    }
    for (name, schemas) in merged {
        let value = match schemas.as_slice() {
            [single] => (*single).clone(),
            many => {
                let mut wrapper = Map::new();
                wrapper.insert(
                    "anyOf".to_owned(),
                    Value::Array(many.iter().map(|schema| (*schema).clone()).collect()),
                );
                Value::Object(wrapper)
            }
        };
        properties.insert(name.clone(), value);
    }

    // Required: fields every variant requires, in the first variant's order.
    let required: Vec<Value> = parts
        .first()?
        .required
        .iter()
        .filter(|name| parts.iter().all(|part| part.required.contains(name)))
        .map(|name| Value::String((*name).to_owned()))
        .collect();

    let mut description_parts = Vec::new();
    if let Some(container) = root.get("description").and_then(Value::as_str) {
        description_parts.push(container.to_owned());
    }
    if discriminator.is_none() {
        // Without a discriminator the per-variant doc text has no property
        // to live on; fold it into the root description rather than drop it.
        let variant_docs: Vec<&str> = parts.iter().filter_map(|part| part.description).collect();
        if !variant_docs.is_empty() {
            description_parts.push(format!("One of: {}", variant_docs.join(" | ")));
        }
    }

    let mut out = Map::new();
    out.insert("type".to_owned(), Value::String("object".to_owned()));
    if !description_parts.is_empty() {
        out.insert(
            "description".to_owned(),
            Value::String(description_parts.join(" ")),
        );
    }
    out.insert("properties".to_owned(), Value::Object(properties));
    out.insert("required".to_owned(), Value::Array(required));
    if parts.iter().all(|part| part.additional_properties_false) {
        out.insert("additionalProperties".to_owned(), Value::Bool(false));
    }
    Some(Value::Object(out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tools::task::tool::TaskCommand;

    /// Asserts `schema` satisfies `OpenAI`'s function-parameter rules.
    fn assert_openai_compliant(schema: &Value, context: &str) {
        let root = schema.as_object().expect("schema is an object");
        assert_eq!(
            root.get("type").and_then(Value::as_str),
            Some("object"),
            "{context}: root must be type object, got: {schema}",
        );
        for keyword in FORBIDDEN_ROOT_KEYWORDS {
            assert!(
                !root.contains_key(keyword),
                "{context}: forbidden root keyword '{keyword}' present: {schema}",
            );
        }
    }

    #[test]
    fn compliant_object_schema_passes_through_unchanged() {
        let schema = json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        });
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    #[test]
    fn missing_root_type_gains_object() {
        let schema = json!({ "properties": { "city": { "type": "string" } } });
        let out = downlevel_function_parameters("t", &schema);
        assert_eq!(out["type"], "object");
        assert_eq!(out["properties"], schema["properties"]);
    }

    #[test]
    fn non_object_root_type_passes_through_verbatim() {
        let schema = json!({ "type": "string", "minLength": 1 });
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    #[test]
    fn one_of_with_non_object_variants_passes_through_verbatim() {
        let schema = json!({
            "type": "object",
            "oneOf": [ { "type": "string", "const": "a" } ]
        });
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    #[test]
    fn non_object_schema_value_passes_through_verbatim() {
        let schema = json!("not a schema object");
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
        let schema = json!(true);
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    #[test]
    fn empty_one_of_passes_through_verbatim() {
        let schema = json!({ "type": "object", "oneOf": [] });
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    #[test]
    fn multiple_forbidden_keywords_pass_through_verbatim() {
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": { "op": { "const": "a" } },
                    "required": ["op"],
                    "additionalProperties": false
                }
            ],
            "enum": ["a"]
        });
        assert_eq!(downlevel_function_parameters("t", &schema), schema);
    }

    /// A root `anyOf` flattens identically to `oneOf` — the flat schema is
    /// already a looser union, so the distinction is immaterial.
    #[test]
    fn any_of_flattens_like_one_of() {
        let schema = json!({
            "anyOf": [
                {
                    "type": "object",
                    "description": "Create a thing.",
                    "properties": {
                        "op": { "const": "create" },
                        "name": { "type": "string" }
                    },
                    "required": ["op", "name"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "description": "List all things.",
                    "properties": { "op": { "const": "list" } },
                    "required": ["op"],
                    "additionalProperties": false
                }
            ]
        });
        let out = downlevel_function_parameters("t", &schema);
        assert_openai_compliant(&out, "flattened anyOf");
        assert_eq!(out["properties"]["op"]["enum"], json!(["create", "list"]));
        assert_eq!(out["required"], json!(["op"]));
    }

    /// Variants sharing a discriminator const value disqualify that
    /// property as the discriminator; the schema still flattens — the
    /// shared const becomes a plain merged property and the variant docs
    /// fold into the root description.
    #[test]
    fn duplicate_const_values_disqualify_the_discriminator() {
        let schema = json!({
            "oneOf": [
                {
                    "type": "object",
                    "description": "String form.",
                    "properties": {
                        "op": { "const": "set" },
                        "value": { "type": "string" }
                    },
                    "required": ["op", "value"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "description": "Numeric form.",
                    "properties": {
                        "op": { "const": "set" },
                        "value": { "type": "integer" }
                    },
                    "required": ["op", "value"],
                    "additionalProperties": false
                }
            ]
        });
        let out = downlevel_function_parameters("t", &schema);
        assert_openai_compliant(&out, "duplicate const values");
        // No enum discriminator; the shared const merges to one property.
        assert_eq!(out["properties"]["op"], json!({ "const": "set" }));
        assert_eq!(
            out["properties"]["value"]["anyOf"],
            json!([{ "type": "string" }, { "type": "integer" }])
        );
        assert_eq!(out["description"], "One of: String form. | Numeric form.");
        // Both fields are required by every variant.
        assert_eq!(out["required"], json!(["op", "value"]));
    }

    /// The regression that motivated this module: the real `task` tool
    /// schema must down-level to a shape `OpenAI` accepts, with the command
    /// guidance preserved on the discriminator.
    #[test]
    fn task_command_schema_flattens_to_compliant_shape() {
        let canonical = TaskCommand::json_schema();
        let out = downlevel_function_parameters("task", &canonical);
        assert_openai_compliant(&out, "flattened task schema");

        let action = &out["properties"]["action"];
        assert_eq!(action["type"], "string");
        let commands = action["enum"].as_array().expect("enum array");
        assert_eq!(commands.len(), 11);
        assert!(commands.iter().any(|v| v == "create"));
        assert!(commands.iter().any(|v| v == "list_groups"));

        let guidance = action["description"].as_str().expect("guidance");
        assert!(
            guidance.contains("create: Create a new task. (requires: description)"),
            "per-command requirements must survive: {guidance}",
        );
        assert!(guidance.contains("list_groups: List all known task group slugs."));

        // Only the discriminator is universally required.
        assert_eq!(out["required"], json!(["action"]));
        assert_eq!(out["additionalProperties"], false);

        // Fields identical across variants modulo description merge to one
        // schema rather than an anyOf.
        assert_eq!(out["properties"]["task_id"]["type"], "string");
        assert!(out["properties"]["task_id"].get("anyOf").is_none());
        // Nested enums (TaskStatus) are allowed below the top level.
        assert_eq!(
            out["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed", "blocked", "failed"])
        );
    }

    #[test]
    fn conflicting_field_schemas_merge_into_any_of() {
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "op": { "const": "a" },
                        "value": { "type": "string", "description": "Text." }
                    },
                    "required": ["op", "value"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "op": { "const": "b" },
                        "value": { "type": "integer", "description": "Number." }
                    },
                    "required": ["op"],
                    "additionalProperties": false
                }
            ]
        });
        let out = downlevel_function_parameters("t", &schema);
        assert_openai_compliant(&out, "conflicting fields");
        let any_of = out["properties"]["value"]["anyOf"]
            .as_array()
            .expect("conflicting schemas wrap in anyOf");
        assert_eq!(any_of.len(), 2);
        assert_eq!(any_of[0]["type"], "string");
        assert_eq!(any_of[1]["type"], "integer");
        assert_eq!(out["required"], json!(["op"]));
    }

    #[test]
    fn variants_without_discriminator_fold_docs_into_root_description() {
        let schema = json!({
            "description": "Container.",
            "oneOf": [
                {
                    "type": "object",
                    "description": "First form.",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "description": "Second form.",
                    "properties": { "b": { "type": "integer" } },
                    "required": ["b"],
                    "additionalProperties": false
                }
            ]
        });
        let out = downlevel_function_parameters("t", &schema);
        assert_openai_compliant(&out, "no discriminator");
        assert_eq!(
            out["description"],
            "Container. One of: First form. | Second form."
        );
        // Nothing is required by every variant.
        assert_eq!(out["required"], json!([]));
    }
}
