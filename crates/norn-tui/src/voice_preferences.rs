//! Native read-aloud preferences; decoding never connects to an audio service.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::frontend_preferences::FrontendPreferenceError;

/// Operator choices for the native Locutus control connection.
///
/// NV-001 declares manual, disabled read-aloud as the initial policy. There is
/// no guessed socket or voice: the socket must be supplied before enabling,
/// and an absent voice leaves selection with the registered Locutus seat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoicePreferences {
    /// Admit native speech requests from the terminal.
    pub enabled: bool,
    /// Read newly completed root answers without an explicit play command.
    pub automatic: bool,
    /// Explicit path to the Locutus control door.
    pub control_socket: Option<PathBuf>,
    /// Explicit voice override, or the registered seat's voice.
    pub voice: Option<String>,
}

impl VoicePreferences {
    pub(crate) fn decode(value: Option<&Value>) -> Result<Self, FrontendPreferenceError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let object = value.as_object().ok_or_else(|| invalid("", "object"))?;
        for key in object.keys() {
            if !["enabled", "automatic", "control_socket", "voice"].contains(&key.as_str()) {
                return Err(FrontendPreferenceError::Unknown {
                    path: format!("tui.voice.{key}"),
                });
            }
        }
        let result = Self {
            enabled: boolean(object, "enabled")?,
            automatic: boolean(object, "automatic")?,
            control_socket: text(object, "control_socket")?.map(PathBuf::from),
            voice: text(object, "voice")?,
        };
        result.validate()?;
        Ok(result)
    }

    /// Check that enabling read-aloud has an explicit service destination.
    ///
    /// # Errors
    /// Returns a named configuration error if no control socket was supplied.
    pub fn validate(&self) -> Result<(), FrontendPreferenceError> {
        if self
            .control_socket
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(invalid("control_socket", "absolute socket path"));
        }
        if self.enabled && self.control_socket.is_none() {
            return Err(invalid(
                "control_socket",
                "explicit socket path when enabled",
            ));
        }
        Ok(())
    }

    pub(crate) fn projection(&self) -> Value {
        serde_json::json!({
            "enabled": self.enabled,
            "automatic": self.automatic,
            "control_socket": self.control_socket,
            "voice": self.voice,
        })
    }
}

fn invalid(key: &str, expected: &'static str) -> FrontendPreferenceError {
    FrontendPreferenceError::Invalid {
        path: if key.is_empty() {
            "tui.voice".to_owned()
        } else {
            format!("tui.voice.{key}")
        },
        expected,
    }
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, FrontendPreferenceError> {
    match object.get(key) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| invalid(key, "boolean")),
    }
}

fn text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, FrontendPreferenceError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(invalid(key, "nonempty string or null")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_preferences_are_disabled_and_manual() -> Result<(), FrontendPreferenceError> {
        assert_eq!(VoicePreferences::decode(None)?, VoicePreferences::default());
        Ok(())
    }

    #[test]
    fn enabling_requires_a_named_service() {
        let result = VoicePreferences::decode(Some(&json!({"enabled":true})));
        assert!(
            matches!(result, Err(FrontendPreferenceError::Invalid { path, .. }) if path == "tui.voice.control_socket")
        );
    }

    #[test]
    fn malformed_values_and_unknown_keys_are_rejected() {
        for value in [
            json!({"enabled":"true"}),
            json!({"automatic":null}),
            json!({"voice":" "}),
            json!({"control_socket":4}),
            json!({"autmatic":true}),
        ] {
            assert!(
                VoicePreferences::decode(Some(&value)).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn unknown_key_reports_its_exact_path() {
        assert!(matches!(
            VoicePreferences::decode(Some(&json!({"autmatic":true}))),
            Err(FrontendPreferenceError::Unknown { path }) if path == "tui.voice.autmatic"
        ));
    }

    #[test]
    fn preferences_round_trip_without_losing_the_socket() -> Result<(), FrontendPreferenceError> {
        let original = json!({"enabled":true,"automatic":false,"control_socket":"/tmp/a voice.sock","voice":"bm_fable"});
        let preferences = VoicePreferences::decode(Some(&original))?;
        assert_eq!(preferences.projection(), original);
        assert_eq!(
            VoicePreferences::decode(Some(&preferences.projection()))?,
            preferences
        );
        Ok(())
    }
}
