//! Applying a setting, and what saving it writes, which lasts until the app
//! quits if the save fails.

use super::*;

impl Settings {
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_notification_popups(value: bool, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            notification_popups: value,
            ..*cx.global::<Self>()
        });
        (NOTIFICATION_POPUPS, Value::Flag(value))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_notification_sound(value: bool, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            notification_sound: value,
            ..*cx.global::<Self>()
        });
        (NOTIFICATION_SOUND, Value::Flag(value))
    }

    // The setters apply the setting, and give the key and value to save, which
    // only lasts until the app quits when it can't be saved.

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_close_to_tray(value: bool, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            close_to_tray: value,
            ..*cx.global::<Self>()
        });
        (CLOSE_TO_TRAY, Value::Flag(value))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_worktree_by_default(value: bool, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            worktree_by_default: value,
            ..*cx.global::<Self>()
        });
        (WORKTREE_BY_DEFAULT, Value::Flag(value))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_project_names(value: bool, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            project_names: value,
            ..*cx.global::<Self>()
        });
        (PROJECT_NAMES, Value::Flag(value))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_rail_width(value: f32, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            rail_width: value,
            ..*cx.global::<Self>()
        });
        (RAIL_WIDTH, Value::Number(value.into()))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_shell_share(value: f32, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            shell_share: value,
            ..*cx.global::<Self>()
        });
        (SHELL_SHARE, Value::Number(value.into()))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_sidebar_width(value: f32, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            sidebar_width: value,
            ..*cx.global::<Self>()
        });
        (SIDEBAR_WIDTH, Value::Number(value.into()))
    }

    /// Callers put the theme in the new mode with `appearance::apply`.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_appearance(value: Appearance, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            appearance: value,
            ..*cx.global::<Self>()
        });
        (APPEARANCE, Value::Text(value.key()))
    }

    /// Callers make it take effect with `appearance::apply_motion`.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_motion(value: Motion, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            motion: value,
            ..*cx.global::<Self>()
        });
        (MOTION, Value::Text(value.key()))
    }

    /// Kept within the sizes the terminals zoom to.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_terminal_font_size(value: f32, cx: &mut App) -> (&'static str, Value) {
        let value = value.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE);
        cx.set_global(Self {
            terminal_font_size: value,
            ..*cx.global::<Self>()
        });
        (TERMINAL_FONT_SIZE, Value::Number(value.into()))
    }

    /// Callers put the theme in the new family with `appearance::apply`.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_theme(value: ThemeFamily, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            theme: value,
            ..*cx.global::<Self>()
        });
        (THEME, Value::Text(value.key()))
    }

    /// The window keeps the layout it opened in, so this only changes the one
    /// the app opens in next.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_layout(value: Layout, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            next_layout: value,
            ..*cx.global::<Self>()
        });
        (LAYOUT, Value::Text(value.key()))
    }

    /// Saves where the window is for the next start to open it there, once
    /// (see `clear_restart_bounds`).
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn save_restart_bounds(bounds: WindowBounds) -> (&'static str, Value) {
        (RESTART_BOUNDS, Value::Bounds(bounds))
    }

    /// Forgets the bounds `save_restart_bounds` saved, once a start has read
    /// them, so a later one opens centred.
    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn clear_restart_bounds() -> (&'static str, Value) {
        (RESTART_BOUNDS, Value::Text(""))
    }

    #[must_use = "the setting is saved only once the runtime is asked to save it"]
    pub fn set_editor(value: Editor, cx: &mut App) -> (&'static str, Value) {
        cx.set_global(Self {
            editor: Some(value),
            ..*cx.global::<Self>()
        });
        (EDITOR, Value::Text(value.key()))
    }
}
