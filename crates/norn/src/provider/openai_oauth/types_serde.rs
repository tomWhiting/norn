use std::collections::BTreeMap;

use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, de::Error as _};

use super::super::credential_validation::{CredentialField, validate_credential_value};
use super::{AuthDotJson, ChatGptTokens, IdTokenInfo};

#[derive(Deserialize)]
struct ChatGptTokensWire {
    #[serde(deserialize_with = "super::id_token_serde::deserialize")]
    id_token: IdTokenInfo,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    account_id: Option<String>,
    #[serde(default, flatten)]
    additional_fields: BTreeMap<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for ChatGptTokens {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ChatGptTokensWire::deserialize(deserializer)?;
        let account_id = wire
            .id_token
            .reconcile_account_id(wire.account_id)
            .map_err(D::Error::custom)?;
        if wire.access_token.is_empty() && wire.refresh_token.is_empty() {
            return Err(D::Error::custom(
                "stored OAuth credential is missing a usable token",
            ));
        }
        for (field, value) in [
            (CredentialField::AccessToken, wire.access_token.as_str()),
            (CredentialField::RefreshToken, wire.refresh_token.as_str()),
        ] {
            if !value.is_empty() {
                validate_credential_value(field, value).map_err(D::Error::custom)?;
            }
        }

        Ok(Self {
            id_token: wire.id_token,
            access_token: wire.access_token,
            refresh_token: wire.refresh_token,
            account_id: Some(account_id),
            additional_fields: wire.additional_fields,
        })
    }
}

impl Serialize for AuthDotJson {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        const RESERVED_FIELDS: &[&str] = &[
            "auth_mode",
            "OPENAI_API_KEY",
            "tokens",
            "last_refresh",
            "agent_identity",
        ];

        let mut map = serializer.serialize_map(Some(serialized_field_count(
            &self.additional_fields,
            RESERVED_FIELDS,
        )))?;
        map.serialize_entry("auth_mode", &self.auth_mode)?;
        map.serialize_entry("OPENAI_API_KEY", &self.openai_api_key)?;
        map.serialize_entry("tokens", &self.tokens)?;
        map.serialize_entry("last_refresh", &self.last_refresh)?;
        map.serialize_entry("agent_identity", &self.agent_identity)?;
        serialize_additional_fields(&mut map, &self.additional_fields, RESERVED_FIELDS)?;
        map.end()
    }
}

impl Serialize for ChatGptTokens {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        const RESERVED_FIELDS: &[&str] =
            &["id_token", "access_token", "refresh_token", "account_id"];

        let mut map = serializer.serialize_map(Some(serialized_field_count(
            &self.additional_fields,
            RESERVED_FIELDS,
        )))?;
        map.serialize_entry("id_token", &self.id_token.raw_jwt)?;
        map.serialize_entry("access_token", &self.access_token)?;
        map.serialize_entry("refresh_token", &self.refresh_token)?;
        map.serialize_entry("account_id", &self.account_id)?;
        serialize_additional_fields(&mut map, &self.additional_fields, RESERVED_FIELDS)?;
        map.end()
    }
}

fn serialized_field_count(
    fields: &BTreeMap<String, serde_json::Value>,
    reserved_fields: &[&str],
) -> usize {
    reserved_fields.len().saturating_add(
        fields
            .keys()
            .filter(|key| !reserved_fields.contains(&key.as_str()))
            .count(),
    )
}

fn serialize_additional_fields<M>(
    map: &mut M,
    fields: &BTreeMap<String, serde_json::Value>,
    reserved_fields: &[&str],
) -> Result<(), M::Error>
where
    M: serde::ser::SerializeMap,
{
    for (key, value) in fields {
        if !reserved_fields.contains(&key.as_str()) {
            map.serialize_entry(key, value)?;
        }
    }
    Ok(())
}
