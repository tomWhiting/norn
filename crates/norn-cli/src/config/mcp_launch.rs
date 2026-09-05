//! Strict, redacted MCP launch documents feeding the existing settings overlay.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use norn::config::{McpServerSettings, NornSettings, validate_settings};
use norn::error::ConfigError;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::cli::BuildError;

use super::extensions::collect_extension_servers;

/// An unparsed `--mcp-config` value whose JSON or path is withheld from Debug.
///
/// Parsing is deferred until the CLI has selected its effective working
/// directory. JSON objects, arrays and scalar literals are inline; other values
/// are file paths.
#[derive(Clone)]
pub struct McpConfigArg(String);

impl fmt::Debug for McpConfigArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpConfigArg([REDACTED])")
    }
}

impl FromStr for McpConfigArg {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_owned()))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDocument {
    #[serde(rename = "mcpServers")]
    servers: BTreeMap<String, JsonObject<McpServerSettings>>,
}

/// Restrict a reused Serde struct to JSON objects rather than positional arrays.
struct JsonObject<T>(T);

struct ObjectVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for JsonObject<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
    type Value = JsonObject<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        T::deserialize(de::value::MapAccessDeserializer::new(map)).map(JsonObject)
    }
}

/// Collect complete CLI MCP definitions without writing settings or connecting.
///
/// Files resolve against the effective process working directory. Definitions
/// remain ordinary typed process/HTTP data; commands and arguments are not
/// interpreted as shell text. Duplicate names are refused across all inputs.
///
/// # Errors
///
/// Returns an argument error for unreadable files, malformed documents,
/// duplicate JSON keys or server names, or settings rejected by the existing
/// MCP validator. Diagnostics omit document contents and scalar values.
pub fn collect_mcp_launch_servers(
    configs: &[McpConfigArg],
    extensions: &[String],
) -> Result<BTreeMap<String, McpServerSettings>, BuildError> {
    let mut servers = collect_extension_servers(extensions).map_err(redact_extension_error)?;
    validate_servers(&servers, "--extension")?;
    for (index, config) in configs.iter().enumerate() {
        let source = format!("--mcp-config document {}", index + 1);
        let contents = read_document(config, &source)?;
        let definitions = parse_document(&contents, &source)?;
        validate_servers(&definitions, &source)?;
        for (name, definition) in definitions {
            if servers.contains_key(&name) {
                return Err(BuildError::Argument(format!(
                    "{source} repeats server '{name}' from an earlier launch input",
                )));
            }
            servers.insert(name, definition);
        }
    }
    Ok(servers)
}

fn redact_extension_error(error: BuildError) -> BuildError {
    match error {
        BuildError::Argument(reason) => {
            // The URI parser uses only validated ASCII/generated names in its
            // diagnostics. Its unsupported-scheme error alone includes URI data.
            let safe = if let Some((referent, _)) = reason.split_once(" uses unsupported scheme '")
            {
                format!(
                    "{referent} uses an unsupported URI scheme; expected stdio://, http:// or https:// (value withheld)",
                )
            } else {
                reason
            };
            BuildError::Argument(safe)
        }
        BuildError::Auth(_) => BuildError::Auth(
            "--extension configuration unexpectedly required authentication".to_owned(),
        ),
    }
}

fn read_document(config: &McpConfigArg, source: &str) -> Result<String, BuildError> {
    let trimmed = config.0.trim_start();
    if trimmed.starts_with(['{', '[', '"'])
        || matches!(trimmed.trim_end(), "null" | "true" | "false")
        || trimmed.trim_end().parse::<serde_json::Number>().is_ok()
    {
        return Ok(trimmed.to_owned());
    }
    let permit = norn::resource::acquire_filesystem_operation()
        .map_err(|error| BuildError::Argument(format!("{source}: {error}")))?;
    let contents = std::fs::read_to_string(&config.0).map_err(|error| {
        BuildError::Argument(format!(
            "{source} file {:?} could not be read ({:?})",
            config.0,
            error.kind(),
        ))
    });
    drop(permit);
    contents
}

fn parse_document(
    contents: &str,
    source: &str,
) -> Result<BTreeMap<String, McpServerSettings>, BuildError> {
    // Walk every object before typed deserialization. BTreeMap deserialization
    // alone would silently replace duplicate server, environment or header keys.
    serde_json::from_str::<UniqueJson>(contents)
        .map_err(|error| json_error(source, "invalid JSON or duplicate object key", &error))?;
    let document = serde_json::from_str::<JsonObject<McpDocument>>(contents).map_err(|error| {
        let message = error.to_string();
        let category = if message.starts_with("duplicate field `transport`") {
            "duplicate transport/type declaration"
        } else if message.starts_with("missing field `mcpServers`") {
            "missing required mcpServers object"
        } else if message.starts_with("unknown field ") {
            "unknown MCP document field (field name withheld)"
        } else {
            "invalid mcpServers envelope or incorrect field type"
        };
        json_error(source, category, &error)
    })?;
    Ok(document
        .0
        .servers
        .into_iter()
        .map(|(name, JsonObject(definition))| (name, definition))
        .collect())
}

fn json_error(source: &str, category: &str, error: &serde_json::Error) -> BuildError {
    BuildError::Argument(format!(
        "{source}: {category} at line {}, column {}; contents withheld",
        error.line(),
        error.column(),
    ))
}

fn validate_servers(
    servers: &BTreeMap<String, McpServerSettings>,
    source: &str,
) -> Result<(), BuildError> {
    for (index, (name, definition)) in servers.iter().enumerate() {
        let settings = NornSettings {
            mcp_servers: Some(BTreeMap::from([(name.clone(), definition.clone())])),
            ..NornSettings::default()
        };
        validate_settings(&settings).map_err(|error| {
            BuildError::Argument(format!(
                "{source}, server entry {}: {}",
                index + 1,
                redacted_settings_error(error, name, definition),
            ))
        })?;
    }
    Ok(())
}

fn redacted_settings_error(
    error: ConfigError,
    name: &str,
    definition: &McpServerSettings,
) -> String {
    // Check names only for safe diagnostic attribution; validate_settings owns
    // admission. Its remaining MCP messages contain field names, except for
    // the unsupported transport value, which is replaced below.
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return "invalid server name: use non-empty ASCII letters, digits, '-' or '_' (name withheld)".to_owned();
    }
    match error {
        ConfigError::InvalidConfig { reason } => {
            if definition.transport.as_ref().is_some_and(|transport| {
                reason == format!(
                    "mcp server '{name}' has incompatible or unsupported transport '{transport}'",
                )
            }) {
                format!("mcp server '{name}' has incompatible or unsupported transport (value withheld)")
            } else {
                reason
            }
        }
        ConfigError::MissingField { .. } => {
            format!("mcp server '{name}' is missing a required field")
        }
    }
}

/// A validation-only JSON walk: scalar data is never retained or rendered.
struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
}

#[cfg(test)]
#[path = "mcp_launch_tests.rs"]
mod tests;
