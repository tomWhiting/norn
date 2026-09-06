//! Exact view shortcuts, validated on settings admission and indexed before input dispatch.

use std::collections::{BTreeMap, HashMap};

use iridium_editor::commands::{KeymapError, ModifierPattern, ModifierState, StrokePattern};
use iridium_editor::{KeyCode as EditorCode, Modifiers as EditorModifiers};
use serde_json::{Map, Value};
use termina::event::{KeyCode, KeyEvent, Modifiers};

/// Frontend-only actions; no action admits provider input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ViewAction {
    PaneToggle,
    PaneDiff,
    PaneAgents,
    SendKeyCycle,
    UpperSwitch,
    Search,
    Copy,
    Export,
    FocusNext,
    FocusPrevious,
}

const ACTIONS: [ViewAction; 10] = [
    ViewAction::PaneToggle,
    ViewAction::PaneDiff,
    ViewAction::PaneAgents,
    ViewAction::SendKeyCycle,
    ViewAction::UpperSwitch,
    ViewAction::Search,
    ViewAction::Copy,
    ViewAction::Export,
    ViewAction::FocusNext,
    ViewAction::FocusPrevious,
];
const FUNCTIONS: [EditorCode; 12] = [
    EditorCode::F1,
    EditorCode::F2,
    EditorCode::F3,
    EditorCode::F4,
    EditorCode::F5,
    EditorCode::F6,
    EditorCode::F7,
    EditorCode::F8,
    EditorCode::F9,
    EditorCode::F10,
    EditorCode::F11,
    EditorCode::F12,
];

impl ViewAction {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::PaneToggle => "pane_toggle",
            Self::PaneDiff => "pane_diff",
            Self::PaneAgents => "pane_agents",
            Self::SendKeyCycle => "send_key_cycle",
            Self::UpperSwitch => "upper_switch",
            Self::Search => "search",
            Self::Copy => "copy",
            Self::Export => "export",
            Self::FocusNext => "focus_next",
            Self::FocusPrevious => "focus_previous",
        }
    }

    fn parse(name: &str) -> Result<Self, ShortcutError> {
        ACTIONS
            .into_iter()
            .find(|action| action.name() == name)
            .ok_or_else(|| ShortcutError::Unknown {
                path: format!("tui.input.bindings.{name}"),
            })
    }

    fn declared(self) -> &'static [(EditorCode, Modifiers)] {
        match self {
            Self::PaneToggle => &[
                (EditorCode::Char('p'), Modifiers::ALT),
                (EditorCode::F7, Modifiers::NONE),
            ],
            Self::PaneDiff => &[
                (EditorCode::Char('d'), Modifiers::ALT),
                (EditorCode::F8, Modifiers::NONE),
            ],
            Self::PaneAgents => &[
                (EditorCode::Char('a'), Modifiers::ALT),
                (EditorCode::F9, Modifiers::NONE),
            ],
            Self::SendKeyCycle => &[
                (EditorCode::Char('s'), Modifiers::ALT),
                (EditorCode::F10, Modifiers::NONE),
            ],
            Self::UpperSwitch => &[(EditorCode::F2, Modifiers::NONE)],
            Self::Search => &[(EditorCode::F3, Modifiers::NONE)],
            Self::Copy => &[(EditorCode::F4, Modifiers::NONE)],
            Self::Export => &[(EditorCode::F5, Modifiers::NONE)],
            Self::FocusNext => &[(EditorCode::F6, Modifiers::NONE)],
            Self::FocusPrevious => &[(EditorCode::F6, Modifiers::SHIFT)],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Shortcut {
    code: EditorCode,
    bits: u8,
    pattern: StrokePattern,
    canonical: String,
    label: String,
}

impl Shortcut {
    fn new(code: EditorCode, modifiers: Modifiers) -> Self {
        let pattern = StrokePattern::new(
            code,
            ModifierPattern::exact(EditorModifiers {
                ctrl: modifiers.contains(Modifiers::CONTROL),
                alt: modifiers.contains(Modifiers::ALT),
                shift: modifiers.contains(Modifiers::SHIFT),
                meta: false,
                alt_graph: false,
            }),
        );
        let canonical = pattern.to_string();
        let label = canonical
            .replace("ctrl+", "Ctrl+")
            .replace("alt+", "Option+")
            .replace("shift+", "Shift+");
        let label = if FUNCTIONS.contains(&code) {
            label.to_ascii_uppercase()
        } else {
            label
        };
        Self {
            code,
            bits: modifiers.bits(),
            pattern,
            canonical,
            label,
        }
    }

    fn parse(text: &str, path: &str) -> Result<Self, ShortcutError> {
        let pattern: StrokePattern = text.parse().map_err(|source| ShortcutError::Syntax {
            path: path.to_owned(),
            source,
        })?;
        let modifiers = pattern.modifiers;
        if pattern.captures()
            || [modifiers.ctrl, modifiers.alt, modifiers.shift].contains(&ModifierState::Any)
            || modifiers.meta != ModifierState::Forbidden
            || modifiers.alt_graph != ModifierState::Forbidden
        {
            return Err(ShortcutError::Unsupported {
                path: path.to_owned(),
                reason: "one exact Ctrl/Alt/Shift stroke; no wildcard or Meta/Super/AltGraph",
            });
        }
        if !matches!(pattern.key, EditorCode::Char(_)) && !FUNCTIONS.contains(&pattern.key) {
            return Err(ShortcutError::Unsupported {
                path: path.to_owned(),
                reason: "a character or F1..F12; editing and navigation keys remain reserved",
            });
        }
        let mut bits = Modifiers::NONE;
        for (state, flag) in [
            (modifiers.ctrl, Modifiers::CONTROL),
            (modifiers.alt, Modifiers::ALT),
            (modifiers.shift, Modifiers::SHIFT),
        ] {
            if state == ModifierState::Required {
                bits |= flag;
            }
        }
        if let EditorCode::Char(character) = pattern.key
            && (character.is_control() || !bits.intersects(Modifiers::CONTROL | Modifiers::ALT))
        {
            return Err(ShortcutError::Unsupported {
                path: path.to_owned(),
                reason: "printable character with Control or Alt; ordinary typing remains reserved",
            });
        }
        Ok(Self::new(pattern.key, bits))
    }
}

/// Complete effective bindings and a cached exact-key dispatch index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewShortcuts {
    bindings: BTreeMap<ViewAction, Vec<Shortcut>>,
    index: HashMap<(EditorCode, u8), ViewAction>,
    hints: BTreeMap<ViewAction, String>,
}

impl Default for ViewShortcuts {
    fn default() -> Self {
        let bindings = ACTIONS
            .into_iter()
            .map(|action| {
                (
                    action,
                    action
                        .declared()
                        .iter()
                        .map(|(code, modifiers)| Shortcut::new(*code, *modifiers))
                        .collect(),
                )
            })
            .collect();
        Self::indexed(bindings)
    }
}

impl ViewShortcuts {
    fn indexed(bindings: BTreeMap<ViewAction, Vec<Shortcut>>) -> Self {
        let index = bindings
            .iter()
            .flat_map(|(action, strokes)| {
                strokes
                    .iter()
                    .map(move |stroke| ((stroke.code, stroke.bits), *action))
            })
            .collect();
        let hints = bindings
            .iter()
            .map(|(action, strokes)| {
                (
                    *action,
                    if strokes.is_empty() {
                        "unbound".to_owned()
                    } else {
                        strokes
                            .iter()
                            .map(|stroke| stroke.label.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    },
                )
            })
            .collect();
        Self {
            bindings,
            index,
            hints,
        }
    }

    pub(crate) fn decode(value: Option<&Value>) -> Result<Self, ShortcutError> {
        let mut bindings = Self::default().bindings;
        if let Some(value) = value {
            let object = value.as_object().ok_or_else(|| ShortcutError::Shape {
                path: "tui.input.bindings".to_owned(),
                expected: "action-to-array object",
            })?;
            for (name, value) in object {
                let action = ViewAction::parse(name)?;
                let path = format!("tui.input.bindings.{name}");
                let array = value.as_array().ok_or_else(|| ShortcutError::Shape {
                    path: path.clone(),
                    expected: "array of exact stroke strings",
                })?;
                let strokes = array
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let path = format!("{path}[{index}]");
                        let text = value.as_str().ok_or_else(|| ShortcutError::Shape {
                            path: path.clone(),
                            expected: "exact stroke string",
                        })?;
                        Shortcut::parse(text, &path)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                bindings.insert(action, strokes);
            }
        }
        Self::validated(bindings)
    }

    pub(crate) fn replacement(&self, name: &str, keys: &[&str]) -> Result<Self, ShortcutError> {
        let action = ViewAction::parse(name)?;
        let mut bindings = self.bindings.clone();
        let strokes = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                Shortcut::parse(key, &format!("tui.input.bindings.{name}[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        bindings.insert(action, strokes);
        Self::validated(bindings)
    }

    fn validated(bindings: BTreeMap<ViewAction, Vec<Shortcut>>) -> Result<Self, ShortcutError> {
        let editor = iridium_editor::commands::default_non_modal_keymap();
        let mut claimed = HashMap::new();
        for (action, strokes) in &bindings {
            for (index, stroke) in strokes.iter().enumerate() {
                let path = format!("tui.input.bindings.{}[{index}]", action.name());
                if let Some(first) = claimed.insert((stroke.code, stroke.bits), path.clone()) {
                    return Err(ShortcutError::Duplicate {
                        first,
                        second: path,
                        stroke: stroke.canonical.clone(),
                    });
                }
                // Function keys are already Norn-owned view controls. Character
                // bindings may never shadow editor commands or fixed host input.
                if let EditorCode::Char(character) = stroke.code {
                    if stroke.bits & Modifiers::CONTROL.bits() != 0
                        && "acefktou".contains(character)
                    {
                        return Err(ShortcutError::Reserved {
                            path,
                            owner: "Norn composer/control".to_owned(),
                        });
                    }
                    if let Some(binding) = editor.bindings().iter().find(|binding| {
                        binding
                            .sequence()
                            .first()
                            .is_some_and(|first| first.overlaps(&stroke.pattern))
                    }) {
                        return Err(ShortcutError::Reserved {
                            path,
                            owner: binding.command().map_or_else(
                                || "Iridium keymap prefix".to_owned(),
                                ToString::to_string,
                            ),
                        });
                    }
                }
            }
        }
        Ok(Self::indexed(bindings))
    }

    /// Identity lookup only: the input owner decides whether a press may act.
    pub(crate) fn action(&self, event: KeyEvent) -> Option<ViewAction> {
        let code = match event.code {
            KeyCode::Char(character) => EditorCode::Char(character.to_ascii_lowercase()),
            KeyCode::Function(number) => *FUNCTIONS.get(usize::from(number).checked_sub(1)?)?,
            _ => return None,
        };
        self.index.get(&(code, event.modifiers.bits())).copied()
    }

    pub(crate) fn hint(&self, action: ViewAction) -> &str {
        self.hints.get(&action).map_or("unbound", String::as_str)
    }

    pub(crate) fn projection(&self) -> Value {
        Value::Object(
            self.bindings
                .iter()
                .map(|(action, strokes)| {
                    (
                        action.name().to_owned(),
                        Value::Array(
                            strokes
                                .iter()
                                .map(|stroke| Value::String(stroke.canonical.clone()))
                                .collect(),
                        ),
                    )
                })
                .collect::<Map<_, _>>(),
        )
    }

    pub(crate) fn summary(&self) -> String {
        self.bindings
            .iter()
            .map(|(action, strokes)| {
                format!(
                    "{}: {}",
                    action.name(),
                    if strokes.is_empty() {
                        "unbound".to_owned()
                    } else {
                        strokes
                            .iter()
                            .map(|stroke| stroke.canonical.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ShortcutError {
    #[error("unknown view action at {path}")]
    Unknown { path: String },
    #[error("invalid view binding {path}: expected {expected}")]
    Shape {
        path: String,
        expected: &'static str,
    },
    #[error("invalid view binding {path}: expected one exact stroke")]
    Syntax {
        path: String,
        #[source]
        source: KeymapError,
    },
    #[error("unsupported view binding {path}: expected {reason}")]
    Unsupported { path: String, reason: &'static str },
    #[error("duplicate view binding {stroke} at {first} and {second}")]
    Duplicate {
        first: String,
        second: String,
        stroke: String,
    },
    #[error("view binding {path} conflicts with {owner}")]
    Reserved { path: String, owner: String },
}

#[cfg(test)]
#[path = "view_shortcuts_tests.rs"]
mod tests;
