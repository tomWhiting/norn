//! Typed partial channel settings, strict decoding and explicit layer overlays.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroUsize;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// A configured delivery choice, independent of runtime source negotiation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelPolicySetting {
    /// Do not admit external messages from the selected source.
    Off,
    /// Wake the running session when external messages arrive.
    Wake,
    /// Admit external messages on the next interactive turn.
    NextTurn,
}

impl<'de> Deserialize<'de> for ChannelPolicySetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const EXPECTED: &str = "channel policy must be off, wake or next-turn (value withheld)";
        let value =
            String::deserialize(deserializer).map_err(|error| redacted_error(error, EXPECTED))?;
        match value.as_str() {
            "off" => Ok(Self::Off),
            "wake" => Ok(Self::Wake),
            "next-turn" => Ok(Self::NextTurn),
            _ => Err(de::Error::custom(EXPECTED)),
        }
    }
}

/// Explicit behavior when a retained channel inbox reaches its chosen limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelOverflowSetting {
    /// Refuse newly arriving messages while preserving retained messages.
    RejectNew,
}

impl<'de> Deserialize<'de> for ChannelOverflowSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const EXPECTED: &str = "channel overflow must be reject-new (value withheld)";
        let value =
            String::deserialize(deserializer).map_err(|error| redacted_error(error, EXPECTED))?;
        match value.as_str() {
            "reject-new" => Ok(Self::RejectNew),
            _ => Err(de::Error::custom(EXPECTED)),
        }
    }
}

/// Partial settings for this launch's channel policy and retained inbox limits.
///
/// Missing and null fields inherit lower layers. No retention quota or overflow
/// policy is supplied by this type; the runtime validates completed settings.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelSettings {
    /// Policy for eligible sources without a named override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_policy: Option<ChannelPolicySetting>,
    /// Named source policies; explicit off overrides a lower delivery policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<BTreeMap<String, ChannelPolicySetting>>,
    /// Explicit positive bound on the number of retained messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retained_messages: Option<NonZeroUsize>,
    /// Explicit positive bound on the number of retained content bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retained_bytes: Option<NonZeroUsize>,
    /// Explicit overflow choice; absence remains unresolved until launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overflow: Option<ChannelOverflowSetting>,
}

impl fmt::Debug for ChannelSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChannelSettings")
            .field("default_policy", &self.default_policy)
            .field("source_entries", &self.sources.as_ref().map(BTreeMap::len))
            .field("max_retained_messages", &self.max_retained_messages)
            .field("max_retained_bytes", &self.max_retained_bytes)
            .field("overflow", &self.overflow)
            .finish()
    }
}

impl ChannelSettings {
    /// Overlay a higher-precedence layer without clearing inherited entries.
    ///
    /// Present scalar fields replace lower values. Source maps merge by name;
    /// an empty map adds nothing, and explicit off remains a real override.
    pub fn overlay(&mut self, higher: Self) {
        self.default_policy = higher.default_policy.or(self.default_policy);
        self.max_retained_messages = higher.max_retained_messages.or(self.max_retained_messages);
        self.max_retained_bytes = higher.max_retained_bytes.or(self.max_retained_bytes);
        self.overflow = higher.overflow.or(self.overflow);
        if let Some(sources) = higher.sources {
            self.sources
                .get_or_insert_with(BTreeMap::new)
                .extend(sources);
        }
    }
}

impl<'de> Deserialize<'de> for ChannelSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ChannelSettingsVisitor)
    }
}

struct ChannelSettingsVisitor;

impl<'de> Visitor<'de> for ChannelSettingsVisitor {
    type Value = ChannelSettings;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a channels settings object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut settings = ChannelSettings::default();
        let mut seen = BTreeSet::new();
        while let Some(field) = map.next_key::<String>()? {
            if !matches!(
                field.as_str(),
                "default_policy"
                    | "sources"
                    | "max_retained_messages"
                    | "max_retained_bytes"
                    | "overflow"
            ) {
                return Err(de::Error::custom("unknown channels field (name withheld)"));
            }
            if !seen.insert(field.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate channels.{field} field"
                )));
            }
            match field.as_str() {
                "default_policy" => settings.default_policy = map.next_value()?,
                "sources" => {
                    settings.sources = map
                        .next_value::<Option<ChannelSources>>()?
                        .map(|sources| sources.0);
                }
                "max_retained_messages" => {
                    settings.max_retained_messages = map.next_value().map_err(|error| {
                        redacted_error(error, "channels.max_retained_messages must be a positive integer or null (value withheld)")
                    })?;
                }
                "max_retained_bytes" => {
                    settings.max_retained_bytes = map.next_value().map_err(|error| {
                        redacted_error(error, "channels.max_retained_bytes must be a positive integer or null (value withheld)")
                    })?;
                }
                "overflow" => settings.overflow = map.next_value()?,
                _ => return Err(de::Error::custom("unknown channels field (name withheld)")),
            }
        }
        Ok(settings)
    }
}

struct ChannelSources(BTreeMap<String, ChannelPolicySetting>);

impl<'de> Deserialize<'de> for ChannelSources {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ChannelSourcesVisitor)
    }
}

struct ChannelSourcesVisitor;

impl<'de> Visitor<'de> for ChannelSourcesVisitor {
    type Value = ChannelSources;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a channels.sources object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut sources = BTreeMap::new();
        while let Some(name) = map.next_key::<String>()? {
            if sources.contains_key(&name) {
                return Err(de::Error::custom(
                    "duplicate channels.sources name (name withheld)",
                ));
            }
            sources.insert(name, map.next_value()?);
        }
        Ok(ChannelSources(sources))
    }
}

/// Preserve a typed parse failure while preventing serde from echoing a scalar.
fn redacted_error<E: de::Error>(error: E, message: &str) -> E {
    drop(error);
    E::custom(message)
}

#[cfg(test)]
#[path = "channels_tests.rs"]
mod tests;
