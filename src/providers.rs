//! The provider table.
//!
//! Everything that has to know *which* providers exist, in what order they are
//! drawn, and how they are addressed reads that from here: the Models menu, the
//! tray icons, the settings file and the poll loop.
//!
//! The drawing code is generic: `paint_content` and `draw_row` take the columns
//! built by `render_columns` and never mention a provider by name. Adding a
//! provider still means touching a few per-provider arms that remain — the
//! settings fields, the per-provider state, the row formatting and the tray
//! badge colours — plus a poller arm. What it no longer means is editing the
//! positional parameter lists that used to thread three providers through every
//! paint call.

use crate::localization::Strings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    ClaudeCode,
    Codex,
    Antigravity,
}

/// Display order, left to right. Also the order the settings file writes them,
/// and the order their columns are drawn in.
pub const PROVIDERS: [ProviderId; 3] = [
    ProviderId::ClaudeCode,
    ProviderId::Codex,
    ProviderId::Antigravity,
];

impl ProviderId {
    /// Menu command id for the Models submenu entry that toggles this provider.
    pub const fn menu_id(self) -> u16 {
        match self {
            Self::ClaudeCode => 60,
            Self::Codex => 61,
            Self::Antigravity => 62,
        }
    }

    /// Two-letter code the widget prints in its row, the way an instrument
    /// panel labels its gauges. Not translated: it is a mark, not a word.
    pub const fn code(self) -> &'static str {
        match self {
            Self::ClaudeCode => "CL",
            Self::Codex => "CX",
            Self::Antigravity => "AG",
        }
    }

    /// Name shown in the menu and in tray tooltips, in the user's language.
    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::ClaudeCode => strings.claude_code_model,
            Self::Codex => strings.codex_model,
            Self::Antigravity => strings.antigravity_model,
        }
    }
}

/// Resolve a menu command id back to its provider, if it is one. Keeps the
/// command handler a single generic branch instead of one per provider.
pub fn from_menu_id(menu_id: u16) -> Option<ProviderId> {
    PROVIDERS.into_iter().find(|id| id.menu_id() == menu_id)
}
