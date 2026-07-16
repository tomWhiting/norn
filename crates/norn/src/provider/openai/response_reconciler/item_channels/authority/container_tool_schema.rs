//! Validation for automatic container environments nested in shell tools.

use serde_json::{Map, Value};

use super::schema::{
    JsonShape, ValidationResult, invalid, optional_enum, optional_value, require_enum,
    require_object, require_string, require_value, required_str, validate_string_array,
    value_object,
};

#[derive(Clone, Copy)]
pub(super) enum NetworkPolicyPath {
    Container,
    Environment,
}

impl NetworkPolicyPath {
    const fn policy_type(self) -> &'static str {
        match self {
            Self::Container => "tools[].container.network_policy.type",
            Self::Environment => "tools[].environment.network_policy.type",
        }
    }

    const fn allowed_domains(self) -> &'static str {
        match self {
            Self::Container => "tools[].container.network_policy.allowed_domains",
            Self::Environment => "tools[].environment.network_policy.allowed_domains",
        }
    }

    const fn allowed_domain(self) -> &'static str {
        match self {
            Self::Container => "tools[].container.network_policy.allowed_domains[]",
            Self::Environment => "tools[].environment.network_policy.allowed_domains[]",
        }
    }

    const fn domain_secrets(self) -> &'static str {
        match self {
            Self::Container => "tools[].container.network_policy.domain_secrets",
            Self::Environment => "tools[].environment.network_policy.domain_secrets",
        }
    }

    const fn domain_secret(self) -> &'static str {
        match self {
            Self::Container => "tools[].container.network_policy.domain_secrets[]",
            Self::Environment => "tools[].environment.network_policy.domain_secrets[]",
        }
    }

    fn secret_field(self, key: &str) -> &'static str {
        match (self, key) {
            (Self::Container, "domain") => {
                "tools[].container.network_policy.domain_secrets[].domain"
            }
            (Self::Container, "name") => "tools[].container.network_policy.domain_secrets[].name",
            (Self::Container, _) => "tools[].container.network_policy.domain_secrets[].value",
            (Self::Environment, "domain") => {
                "tools[].environment.network_policy.domain_secrets[].domain"
            }
            (Self::Environment, "name") => {
                "tools[].environment.network_policy.domain_secrets[].name"
            }
            (Self::Environment, _) => "tools[].environment.network_policy.domain_secrets[].value",
        }
    }
}

pub(super) fn validate_container_auto(
    environment: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    if let Some(files) = optional_value(
        environment,
        "file_ids",
        item_type,
        "tools[].environment.file_ids",
        JsonShape::Array,
        false,
    )? {
        validate_string_array(files, item_type, "tools[].environment.file_ids[]")?;
    }
    optional_enum(
        environment,
        "memory_limit",
        item_type,
        "tools[].environment.memory_limit",
        &["1g", "4g", "16g", "64g"],
        true,
    )?;
    if let Some(policy) = optional_value(
        environment,
        "network_policy",
        item_type,
        "tools[].environment.network_policy",
        JsonShape::Object,
        false,
    )? {
        validate_network_policy(
            value_object(policy, item_type, "tools[].environment.network_policy")?,
            item_type,
            NetworkPolicyPath::Environment,
        )?;
    }
    if let Some(skills) = optional_value(
        environment,
        "skills",
        item_type,
        "tools[].environment.skills",
        JsonShape::Array,
        false,
    )? {
        for skill in skills
            .as_array()
            .ok_or_else(|| invalid(item_type, "tools[].environment.skills"))?
        {
            validate_container_skill(
                value_object(skill, item_type, "tools[].environment.skills[]")?,
                item_type,
            )?;
        }
    }
    Ok(())
}

pub(super) fn validate_network_policy(
    policy: &Map<String, Value>,
    item_type: &'static str,
    path: NetworkPolicyPath,
) -> ValidationResult {
    match required_str(policy, "type", item_type, path.policy_type())? {
        "disabled" => Ok(()),
        "allowlist" => {
            let domains = require_value(
                policy,
                "allowed_domains",
                item_type,
                path.allowed_domains(),
                JsonShape::Array,
                false,
            )?;
            validate_string_array(domains, item_type, path.allowed_domain())?;
            if let Some(secrets) = optional_value(
                policy,
                "domain_secrets",
                item_type,
                path.domain_secrets(),
                JsonShape::Array,
                false,
            )? {
                validate_domain_secrets(secrets, item_type, path)?;
            }
            Ok(())
        }
        _ => Err(invalid(item_type, path.policy_type())),
    }
}

fn validate_domain_secrets(
    value: &Value,
    item_type: &'static str,
    path: NetworkPolicyPath,
) -> ValidationResult {
    for secret in value
        .as_array()
        .ok_or_else(|| invalid(item_type, path.domain_secrets()))?
    {
        let secret = value_object(secret, item_type, path.domain_secret())?;
        for key in ["domain", "name", "value"] {
            require_string(secret, key, item_type, path.secret_field(key))?;
        }
    }
    Ok(())
}

fn validate_container_skill(
    skill: &Map<String, Value>,
    item_type: &'static str,
) -> ValidationResult {
    match required_str(
        skill,
        "type",
        item_type,
        "tools[].environment.skills[].type",
    )? {
        "skill_reference" => {
            require_string(
                skill,
                "skill_id",
                item_type,
                "tools[].environment.skills[].skill_id",
            )?;
            optional_value(
                skill,
                "version",
                item_type,
                "tools[].environment.skills[].version",
                JsonShape::String,
                false,
            )?;
            Ok(())
        }
        "inline" => validate_inline_skill(skill, item_type),
        _ => Err(invalid(item_type, "tools[].environment.skills[].type")),
    }
}

fn validate_inline_skill(skill: &Map<String, Value>, item_type: &'static str) -> ValidationResult {
    require_string(
        skill,
        "description",
        item_type,
        "tools[].environment.skills[].description",
    )?;
    require_string(
        skill,
        "name",
        item_type,
        "tools[].environment.skills[].name",
    )?;
    let source = require_object(
        skill,
        "source",
        item_type,
        "tools[].environment.skills[].source",
    )?;
    require_enum(
        source,
        "type",
        item_type,
        "tools[].environment.skills[].source.type",
        &["base64"],
    )?;
    require_enum(
        source,
        "media_type",
        item_type,
        "tools[].environment.skills[].source.media_type",
        &["application/zip"],
    )?;
    require_string(
        source,
        "data",
        item_type,
        "tools[].environment.skills[].source.data",
    )?;
    Ok(())
}
