use serde_json::Value;

use super::super::authority::RedactionRegistry;
use super::super::model::{ArtifactRegistration, SyntheticPurpose};
use super::super::protocol_json_schema::{validate_json_schema, validate_schema_literal};
use super::super::protocol_literals::type_accepts;
use super::super::protocol_schema::{
    FieldRule, ProtocolDialect, ProtocolObjectRole, RuleContext, StringRule, enum_accepts,
    field_rule, is_approved_source_url, is_category, is_concern, is_finding_id, is_fixed_url,
    is_fixture_id, is_fixture_path, is_hex_pin, is_json_pointer, is_pinned_source_path,
    sensitive_code, string_rule_code,
};
use super::super::scan::{decoded_string_violation, decoded_structural_violation};
use super::super::validate::{ArtifactIssue, RedactionCode};

pub(super) fn scan_value(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    context: RuleContext,
    expected_leaf: Option<SyntheticPurpose>,
    issues: &mut Vec<ArtifactIssue>,
) {
    match value {
        Value::Object(object) => {
            let object_context = RuleContext {
                assistant_message: object.get("role").and_then(Value::as_str) == Some("assistant")
                    && object.get("type").and_then(Value::as_str) == Some("message"),
                ..context
            };
            for (key, child) in object {
                let Some(rule) = field_rule(key, object_context) else {
                    issues.push(issue(RedactionCode::SchemaMismatch));
                    continue;
                };
                let child_context = RuleContext {
                    role: child_role(object_context.role, key),
                    ..object_context
                };
                apply_rule(registry, registration, child, rule, child_context, issues);
            }
        }
        Value::Array(values) => {
            let element_context = RuleContext {
                role: ProtocolObjectRole::Other,
                ..context
            };
            for child in values {
                scan_value(
                    registry,
                    registration,
                    child,
                    element_context,
                    expected_leaf,
                    issues,
                );
            }
        }
        Value::String(string) => {
            let purpose = expected_leaf.unwrap_or(SyntheticPurpose::Generic);
            validate_string_rule(
                registry,
                registration,
                string,
                StringRule::Synthetic(purpose),
                issues,
            );
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            if expected_leaf.is_some_and(|purpose| purpose != SyntheticPurpose::Generic) {
                issues.push(issue(sensitive_code(expected_leaf)));
            }
        }
    }
}

fn child_role(parent: ProtocolObjectRole, key: &str) -> ProtocolObjectRole {
    match (parent, key) {
        (ProtocolObjectRole::RequestPayload, "reasoning") => ProtocolObjectRole::RequestReasoning,
        (ProtocolObjectRole::Event, "response") => ProtocolObjectRole::Response,
        (ProtocolObjectRole::Response, "incomplete_details") => {
            ProtocolObjectRole::IncompleteDetails
        }
        _ => ProtocolObjectRole::Other,
    }
}

pub(super) fn validate_protocol_type(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &str,
    dialect: ProtocolDialect,
    issues: &mut Vec<ArtifactIssue>,
) {
    if type_accepts(value) || (dialect == ProtocolDialect::Codex && value == "response.metadata") {
        return;
    }
    if matches!(
        value,
        "norn-synthetic-generic-evt-05-event" | "norn-synthetic-generic-evt-05-item-type"
    ) {
        validate_synthetic(
            registry,
            registration,
            value,
            SyntheticPurpose::Generic,
            issues,
        );
    } else {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

pub(in crate::redaction) fn validate_string_rule(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &str,
    rule: StringRule,
    issues: &mut Vec<ArtifactIssue>,
) {
    let violation = match rule {
        StringRule::Synthetic(_) => decoded_string_violation(value),
        StringRule::ApprovedSourceUrl
        | StringRule::Category
        | StringRule::Concern
        | StringRule::Enum(_)
        | StringRule::FindingId
        | StringRule::FixtureId
        | StringRule::FixturePath
        | StringRule::FixedUrl
        | StringRule::HexPin
        | StringRule::JsonPointer
        | StringRule::PinnedSourcePath => decoded_structural_violation(value),
    };
    if let Some(code) = violation {
        issues.push(issue(code.into()));
    }
    let accepted = match rule {
        StringRule::ApprovedSourceUrl => is_approved_source_url(value),
        StringRule::Category => is_category(value),
        StringRule::Concern => is_concern(value),
        StringRule::Enum(set) => enum_accepts(set, value),
        StringRule::FindingId => is_finding_id(value),
        StringRule::FixtureId => is_fixture_id(value),
        StringRule::Synthetic(purpose) => {
            return validate_synthetic(registry, registration, value, purpose, issues);
        }
        StringRule::FixturePath => is_fixture_path(value),
        StringRule::FixedUrl => is_fixed_url(value),
        StringRule::HexPin => is_hex_pin(value),
        StringRule::JsonPointer => is_json_pointer(value),
        StringRule::PinnedSourcePath => is_pinned_source_path(value),
    };
    if !accepted {
        issues.push(issue(string_rule_code(rule)));
    }
}

fn apply_rule(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    rule: FieldRule,
    context: RuleContext,
    issues: &mut Vec<ArtifactIssue>,
) {
    match rule {
        FieldRule::String { rule, nullable } => match value {
            Value::String(string) => {
                validate_string_rule(registry, registration, string, rule, issues);
            }
            Value::Null if nullable => {}
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => issues.push(issue(string_rule_code(rule))),
        },
        FieldRule::Number { nullable } => validate_number(value, nullable, issues),
        FieldRule::NumberOrString(rule) => match value {
            Value::Number(_) => {}
            Value::String(value) => {
                validate_string_rule(registry, registration, value, rule, issues);
            }
            Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        },
        FieldRule::Boolean { nullable } => validate_boolean(value, nullable, issues),
        FieldRule::Container {
            leaf,
            nullable,
            scalar,
        } => validate_container(
            registry,
            registration,
            value,
            context,
            ContainerRule {
                leaf,
                nullable,
                scalar,
            },
            issues,
        ),
        FieldRule::MachineMap { leaf } => {
            validate_machine_map(registry, registration, value, context, leaf, issues);
        }
        FieldRule::StringList { rule, nullable } => {
            validate_string_list(registry, registration, value, rule, nullable, issues);
        }
        FieldRule::StringOrContainer {
            rule,
            leaf,
            nullable,
        } => match value {
            Value::String(string) => {
                validate_string_rule(registry, registration, string, rule, issues);
            }
            Value::Array(_) | Value::Object(_) => {
                scan_value(registry, registration, value, context, Some(leaf), issues);
            }
            Value::Null if nullable => {}
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        },
        FieldRule::BooleanOrContainer => match value {
            Value::Bool(_) => {}
            Value::Array(_) | Value::Object(_) => scan_value(
                registry,
                registration,
                value,
                context,
                Some(SyntheticPurpose::Generic),
                issues,
            ),
            Value::Null | Value::Number(_) | Value::String(_) => {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        },
        FieldRule::SchemaLiteral => {
            validate_schema_literal(registry, registration, value, issues);
        }
        FieldRule::JsonSchema => validate_json_schema(registry, registration, value, issues),
        FieldRule::ProtocolType => match value {
            Value::String(value) => {
                validate_protocol_type(registry, registration, value, context.dialect, issues);
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => {
                issues.push(issue(RedactionCode::SchemaMismatch));
            }
        },
    }
}

#[derive(Clone, Copy)]
struct ContainerRule {
    leaf: SyntheticPurpose,
    nullable: bool,
    scalar: bool,
}

fn validate_container(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    context: RuleContext,
    rule: ContainerRule,
    issues: &mut Vec<ArtifactIssue>,
) {
    match value {
        Value::Null if rule.nullable => {}
        Value::String(string) if rule.scalar => validate_string_rule(
            registry,
            registration,
            string,
            StringRule::Synthetic(rule.leaf),
            issues,
        ),
        Value::Array(_) | Value::Object(_) => {
            scan_value(
                registry,
                registration,
                value,
                context,
                Some(rule.leaf),
                issues,
            );
        }
        Value::Bool(_) | Value::Number(_) if rule.leaf == SyntheticPurpose::Generic => {}
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
    }
}

fn validate_machine_map(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    context: RuleContext,
    leaf: SyntheticPurpose,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(object) = value.as_object() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for (key, child) in object {
        validate_string_rule(
            registry,
            registration,
            key,
            StringRule::Synthetic(SyntheticPurpose::Generic),
            issues,
        );
        scan_value(registry, registration, child, context, Some(leaf), issues);
    }
}

fn validate_string_list(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &Value,
    rule: StringRule,
    nullable: bool,
    issues: &mut Vec<ArtifactIssue>,
) {
    if value.is_null() && nullable {
        return;
    }
    let Some(values) = value.as_array() else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    for value in values {
        let Some(string) = value.as_str() else {
            issues.push(issue(RedactionCode::SchemaMismatch));
            continue;
        };
        validate_string_rule(registry, registration, string, rule, issues);
    }
}

fn validate_number(value: &Value, nullable: bool, issues: &mut Vec<ArtifactIssue>) {
    match value {
        Value::Number(_) => {}
        Value::Null if nullable => {}
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
    }
}

fn validate_boolean(value: &Value, nullable: bool, issues: &mut Vec<ArtifactIssue>) {
    match value {
        Value::Bool(_) => {}
        Value::Null if nullable => {}
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            issues.push(issue(RedactionCode::SchemaMismatch));
        }
    }
}

fn validate_synthetic(
    registry: &RedactionRegistry,
    registration: &ArtifactRegistration,
    value: &str,
    purpose: SyntheticPurpose,
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(synthetic) = registry.synthetic_for_value(value) else {
        issues.push(issue(sensitive_code(Some(purpose))));
        return;
    };
    if synthetic.purpose() != purpose
        || !registration
            .synthetic_ids()
            .iter()
            .any(|id| id == synthetic.id())
    {
        issues.push(issue(RedactionCode::SyntheticMetadataMismatch));
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
