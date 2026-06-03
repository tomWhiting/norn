//! Input editor, keybindings, autocomplete, and visual wrap layout.

pub mod editor;
pub mod history;
pub mod keybindings;
mod navigation;
pub mod wrap;

pub use autocomplete::{
    Acceptance, AutocompletePopup, AutocompleteTrigger, CandidateRow, FileCandidate,
    SlashCandidate, SourceTag, TriggerKind, detect_trigger, filter_slash_candidates,
    generate_file_candidates,
};
pub use editor::InputEditor;
pub use history::InputHistory;
pub use keybindings::{InputAction, map_key_event};

pub mod autocomplete;
