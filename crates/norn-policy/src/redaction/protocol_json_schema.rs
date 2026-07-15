use serde_json::Value;

use super::authority::RedactionRegistry;
use super::model::{ArtifactRegistration, SyntheticPurpose};
use super::protocol::validate_string_rule;
use super::protocol_schema::{StringRule, is_json_pointer};
use super::validate::{ArtifactIssue, RedactionCode};

pub(crate) fn validate_json_schema(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(object) = value.as_object() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for (key, child) in object {
        match key.as_str() {
            "$ref" => validate_reference(child, issues),
            "type" => validate_schema_type(child, issues),
            "properties" | "$defs" => {
                validate_schema_map(registry, registration, child, issues);
            }
            "required" => validate_synthetic_strings(registry, registration, child, issues),
            "description" => validate_synthetic_string(registry, registration, child, issues),
            "enum" => validate_literals(registry, registration, child, issues),
            "const" | "default" => {
                validate_schema_literal(registry, registration, child, issues);
            }
            "items" | "additionalProperties" => match child {
                Value::Bool(_) if key == "additionalProperties" => {}
                Value::Object(_) => validate_json_schema(registry, registration, child, issues),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_) => {
                    issues.push(issue(RedactionCode::SchemaMismatch));
                }
            },
            "allOf" | "anyOf" | "oneOf" => {
                let Some(values) = child.as_array() else {
                    issues.push(issue(RedactionCode::SchemaMismatch));
                    continue;
                };
                for schema in values {
                    validate_json_schema(registry, registration, schema, issues);
                }
            }
            "maximum" | "minimum" | "maxItems" | "minItems" | "maxLength" | "minLength" => {
                if !child.is_number() {
                    issues.push(issue(RedactionCode::SchemaMismatch));
                }
            }
            _ => issues.push(issue(RedactionCode::SchemaMismatch)),
        }
    }
}

fn validate_schema_map(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(object) = value.as_object() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for (key, schema) in object {
        validate_string_rule(
            registry,
            registration,
            key,
            StringRule::Synthetic(SyntheticPurpose::Generic),
            issues,
        );
        validate_json_schema(registry, registration, schema, issues);
    }
}

fn validate_schema_type(value: &Value, issues: &mut Vec<ArtifactIssue>) {
    let accepted = |value: &str| {
        matches!(
            value,
            "array" | "boolean" | "integer" | "null" | "number" | "object" | "string"
        )
    };
    match value {
        Value::String(value) if accepted(value) => {}
        Value::Array(values)
            if !values.is_empty()
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(&accepted)) => {}
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => issues.push(issue(RedactionCode::SchemaMismatch)),
    }
}

fn validate_reference(value: &Value, issues: &mut Vec<ArtifactIssue>) {
    if !value.as_str().is_some_and(is_json_pointer) {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

fn validate_synthetic_strings(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(values) = value.as_array() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for value in values {
        validate_synthetic_string(registry, registration, value, issues);
    }
}

fn validate_synthetic_string(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(value) = value.as_str() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    validate_string_rule(
        registry,
        registration,
        value,
        StringRule::Synthetic(SyntheticPurpose::Generic),
        issues,
    );
}

fn validate_literals(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(values) = value.as_array() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for value in values {
        validate_schema_literal(registry, registration, value, issues);
    }
}

pub(crate) fn validate_schema_literal(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    issues: &mut Vec<ArtifactIssue>,
) {
    match value {
        Value::String(_) => validate_synthetic_string(registry, registration, value, issues),
        Value::Array(values) => {
            for child in values {
                validate_schema_literal(registry, registration, child, issues);
            }
        }
        Value::Object(object) => {
            for (key, child) in object {
                validate_string_rule(
                    registry,
                    registration,
                    key,
                    StringRule::Synthetic(SyntheticPurpose::Generic),
                    issues,
                );
                validate_schema_literal(registry, registration, child, issues);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
