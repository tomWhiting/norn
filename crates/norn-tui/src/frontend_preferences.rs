//! Typed frontend settings and launch authority captured without additional filesystem reads.

use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;

use norn::config::{
    TuiPreferenceLayer, TuiPreferenceScope, TuiPreferencesLayers, TuiPreferencesSnapshot,
};
use serde_json::{Map, Value};

use crate::app::active_input::InFlightSubmitMode;
use crate::app::view_config::ViewConfig;
use crate::events::DisplayToggles;
use crate::render::layout::{SplitPreference, UpperPane};
use crate::terminal::clipboard::ClipboardCapability;

/// Physical send key, independent of steer/queue delivery during agent work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerSendKey {
    /// Enter sends; Alt+Enter inserts a newline.
    #[default]
    Enter,
    /// Shift+Enter sends; Enter inserts a newline.
    ShiftEnter,
    /// Alt+Enter sends; Enter inserts a newline.
    AltEnter,
}

impl ComposerSendKey {
    /// Stable settings and local-command spelling.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::ShiftEnter => "shift-enter",
            Self::AltEnter => "alt-enter",
        }
    }

    /// Cycle the three explicit send-key policies without changing the draft.
    #[must_use]
    pub const fn next_policy(self) -> Self {
        match self {
            Self::Enter => Self::ShiftEnter,
            Self::ShiftEnter => Self::AltEnter,
            Self::AltEnter => Self::Enter,
        }
    }
}

/// Existing frontend choices; no transcript identity, draft or runtime authority is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendPreferences {
    pub(crate) changes_open: bool,
    pub(crate) split: SplitPreference,
    pub(crate) upper: UpperPane,
    pub(crate) view: ViewConfig,
    pub(crate) display: DisplayToggles,
    pub(crate) submit_mode: InFlightSubmitMode,
    pub(crate) composer_send_key: ComposerSendKey,
}

impl Default for FrontendPreferences {
    fn default() -> Self {
        Self {
            changes_open: false,
            split: SplitPreference::default(),
            upper: UpperPane::Conversation,
            view: ViewConfig::default(),
            display: DisplayToggles::default(),
            submit_mode: InFlightSubmitMode::Steer,
            composer_send_key: ComposerSendKey::default(),
        }
    }
}

/// Where subsequent operator preference edits are saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceScope {
    /// Temporary changes for this process only.
    Run,
    /// Personal settings, the declared automatic-save default.
    User,
    /// Explicitly selected workspace-local settings.
    Local,
}

/// Validated initial choices and opaque writable layer snapshots.
#[derive(Clone, Debug)]
pub struct FrontendPreferencesLaunch {
    pub(crate) initial: FrontendPreferences,
    pub(crate) user: Option<TuiPreferencesSnapshot>,
    pub(crate) local: Option<TuiPreferencesSnapshot>,
    pub(crate) winner: Option<TuiPreferenceLayer>,
    pub(crate) scope: PreferenceScope,
}

impl FrontendPreferencesLaunch {
    /// Isolated embedders receive current defaults with no filesystem save authority.
    #[must_use]
    pub fn run_only() -> Self {
        Self {
            initial: FrontendPreferences::default(),
            user: None,
            local: None,
            winner: None,
            scope: PreferenceScope::Run,
        }
    }

    /// Validate already-loaded layers and capture immutable targets without rereading files.
    pub fn from_layers(
        layers: &TuiPreferencesLayers,
        launch_root: &Path,
    ) -> Result<Self, FrontendPreferenceError> {
        let user = TuiPreferencesSnapshot::from_layer(
            TuiPreferenceScope::User,
            launch_root,
            layers.value(TuiPreferenceLayer::User).cloned(),
        )?;
        let local = TuiPreferencesSnapshot::from_layer(
            TuiPreferenceScope::WorkspaceLocal,
            launch_root,
            layers.value(TuiPreferenceLayer::WorkspaceLocal).cloned(),
        )?;
        let winner = layers.winning_layer();
        let mut initial = FrontendPreferences::default();
        for layer in [
            TuiPreferenceLayer::User,
            TuiPreferenceLayer::SharedProject,
            TuiPreferenceLayer::WorkspaceLocal,
        ] {
            let path = match layer {
                TuiPreferenceLayer::User => user.path().to_path_buf(),
                TuiPreferenceLayer::SharedProject => launch_root.join(".norn/settings.json"),
                TuiPreferenceLayer::WorkspaceLocal => local.path().to_path_buf(),
            };
            let preferences =
                FrontendPreferences::decode(layers.value(layer)).map_err(|source| {
                    FrontendPreferenceError::Document {
                        path,
                        source: Box::new(source),
                    }
                })?;
            if winner == Some(layer) {
                initial = preferences;
            }
        }
        Ok(Self {
            initial,
            user: Some(user),
            local: Some(local),
            winner,
            scope: PreferenceScope::User,
        })
    }
}

/// A typed refusal of a malformed owned setting; unowned sibling data is untouched.
#[derive(Debug, thiserror::Error)]
pub enum FrontendPreferenceError {
    /// A named field did not meet its exact schema.
    #[error("invalid frontend setting {path}: expected {expected}")]
    Invalid {
        /// Dotted owned field path.
        path: String,
        /// Required type or allowed values.
        expected: &'static str,
    },
    /// An unknown field was supplied inside an owned object.
    #[error("unknown frontend setting {path}")]
    Unknown {
        /// Dotted owned field path.
        path: String,
    },
    /// A supplied integer is outside the named machine field's range.
    #[error("frontend setting {path} is outside its integer range: {source}")]
    Integer {
        /// Dotted owned field path.
        path: String,
        /// Actual conversion failure.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// Exact loaded document containing a malformed owned setting.
    #[error("frontend preferences in {path}: {source}")]
    Document {
        /// Loaded document path.
        path: std::path::PathBuf,
        /// Typed dotted-field refusal.
        #[source]
        source: Box<FrontendPreferenceError>,
    },
    /// The save target could not be captured from the existing launch context.
    #[error(transparent)]
    Target(#[from] norn::config::TuiPreferencesError),
}

impl FrontendPreferences {
    /// Decode only the four owned sections, using existing defaults for absent fields.
    pub fn decode(value: Option<&Value>) -> Result<Self, FrontendPreferenceError> {
        let mut result = Self::default();
        let Some(value) = value else {
            return Ok(result);
        };
        let root = object(value, "tui")?;
        if let Some(value) = root.get("view") {
            let view = object(value, "tui.view")?;
            known(
                view,
                "tui.view",
                &[
                    "changes_open",
                    "split",
                    "upper_pane",
                    "expanded_tools",
                    "history_events",
                    "body_bytes",
                    "clipboard",
                ],
            )?;
            boolean(view, "changes_open", "tui.view", &mut result.changes_open)?;
            boolean(
                view,
                "expanded_tools",
                "tui.view",
                &mut result.view.expanded_tools,
            )?;
            if let Some(value) = view.get("split") {
                let split = object(value, "tui.view.split")?;
                known(split, "tui.view.split", &["conversation", "changes"])?;
                let weight = |key: &str| -> Result<NonZeroU16, FrontendPreferenceError> {
                    let Some(value) = split.get(key) else {
                        return Ok(NonZeroU16::MIN);
                    };
                    let path = format!("tui.view.split.{key}");
                    let number = value
                        .as_u64()
                        .ok_or_else(|| invalid(&path, "positive u16 integer"))?;
                    let number = u16::try_from(number).map_err(|source| {
                        FrontendPreferenceError::Integer {
                            path: path.clone(),
                            source,
                        }
                    })?;
                    NonZeroU16::new(number).ok_or_else(|| invalid(&path, "positive u16 integer"))
                };
                result.split = SplitPreference::new(weight("conversation")?, weight("changes")?);
            }
            if let Some(value) = view.get("upper_pane") {
                result.upper = match value.as_str() {
                    Some("conversation") => UpperPane::Conversation,
                    Some("changes") => UpperPane::Changes,
                    _ => return Err(invalid("tui.view.upper_pane", "conversation or changes")),
                };
            }
            if let Some(value) = view.get("history_events") {
                result
                    .view
                    .set_history_demand(positive(value, "tui.view.history_events")?);
            }
            if let Some(value) = view.get("body_bytes") {
                result
                    .view
                    .set_body_demand(positive(value, "tui.view.body_bytes")?);
            }
            if let Some(value) = view.get("clipboard") {
                result.view.clipboard = match value.as_str() {
                    Some("unspecified") => ClipboardCapability::Unspecified,
                    Some("disabled") => ClipboardCapability::Disabled,
                    Some("osc52") => ClipboardCapability::Osc52,
                    _ => {
                        return Err(invalid(
                            "tui.view.clipboard",
                            "unspecified, disabled or osc52",
                        ));
                    }
                };
            }
        }
        if let Some(value) = root.get("display") {
            let display = object(value, "tui.display")?;
            known(
                display,
                "tui.display",
                &["thinking_visible", "secondary_fields_visible"],
            )?;
            boolean(
                display,
                "thinking_visible",
                "tui.display",
                &mut result.display.thinking_visible,
            )?;
            boolean(
                display,
                "secondary_fields_visible",
                "tui.display",
                &mut result.display.secondary_fields_visible,
            )?;
        }
        if let Some(value) = root.get("input") {
            let input = object(value, "tui.input")?;
            known(input, "tui.input", &["submit_mode"])?;
            if let Some(value) = input.get("submit_mode") {
                result.submit_mode = match value.as_str() {
                    Some("steer") => InFlightSubmitMode::Steer,
                    Some("queue") => InFlightSubmitMode::Queue,
                    _ => return Err(invalid("tui.input.submit_mode", "steer or queue")),
                };
            }
        }
        if let Some(value) = root.get("composer") {
            let composer = object(value, "tui.composer")?;
            known(composer, "tui.composer", &["send_key"])?;
            if let Some(value) = composer.get("send_key") {
                result.composer_send_key = match value.as_str() {
                    Some("enter") => ComposerSendKey::Enter,
                    Some("shift-enter") => ComposerSendKey::ShiftEnter,
                    Some("alt-enter") => ComposerSendKey::AltEnter,
                    _ => {
                        return Err(invalid(
                            "tui.composer.send_key",
                            "enter, shift-enter or alt-enter",
                        ));
                    }
                };
            }
        }
        Ok(result)
    }

    /// Produce only the owned sections, with no unowned settings or session data.
    pub fn projection(&self) -> Result<Map<String, Value>, crate::TuiError> {
        let (conversation, changes) = self.split.weights();
        let mut result = Map::new();
        result.insert("view".to_owned(), serde_json::json!({
            "changes_open":self.changes_open,"split":{"conversation":conversation,"changes":changes},
            "upper_pane":match self.upper { UpperPane::Conversation => "conversation", UpperPane::Changes => "changes" },
            "expanded_tools":self.view.expanded_tools,"history_events":self.view.history_demand()?.get(),
            "body_bytes":self.view.body_demand()?.get(),"clipboard":match self.view.clipboard {
                ClipboardCapability::Unspecified => "unspecified", ClipboardCapability::Disabled => "disabled", ClipboardCapability::Osc52 => "osc52" }
        }));
        result.insert("display".to_owned(), serde_json::json!({"thinking_visible":self.display.thinking_visible,"secondary_fields_visible":self.display.secondary_fields_visible}));
        result.insert(
            "input".to_owned(),
            serde_json::json!({"submit_mode":self.submit_mode.label()}),
        );
        result.insert(
            "composer".to_owned(),
            serde_json::json!({"send_key":self.composer_send_key.label()}),
        );
        Ok(result)
    }
}

fn invalid(path: &str, expected: &'static str) -> FrontendPreferenceError {
    FrontendPreferenceError::Invalid {
        path: path.to_owned(),
        expected,
    }
}
fn object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a Map<String, Value>, FrontendPreferenceError> {
    value.as_object().ok_or_else(|| invalid(path, "object"))
}
fn known(
    object: &Map<String, Value>,
    prefix: &str,
    keys: &[&str],
) -> Result<(), FrontendPreferenceError> {
    for key in object.keys() {
        if !keys.contains(&key.as_str()) {
            return Err(FrontendPreferenceError::Unknown {
                path: format!("{prefix}.{key}"),
            });
        }
    }
    Ok(())
}
fn boolean(
    object: &Map<String, Value>,
    key: &str,
    prefix: &str,
    result: &mut bool,
) -> Result<(), FrontendPreferenceError> {
    if let Some(value) = object.get(key) {
        *result = value
            .as_bool()
            .ok_or_else(|| invalid(&format!("{prefix}.{key}"), "boolean"))?;
    }
    Ok(())
}
fn positive(value: &Value, path: &str) -> Result<NonZeroUsize, FrontendPreferenceError> {
    let number = value
        .as_u64()
        .ok_or_else(|| invalid(path, "positive machine-sized integer"))?;
    let number = usize::try_from(number).map_err(|source| FrontendPreferenceError::Integer {
        path: path.to_owned(),
        source,
    })?;
    NonZeroUsize::new(number).ok_or_else(|| invalid(path, "positive machine-sized integer"))
}

#[cfg(test)]
#[path = "frontend_preferences_tests.rs"]
mod tests;
