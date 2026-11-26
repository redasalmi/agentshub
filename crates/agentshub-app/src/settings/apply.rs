//! Applying a setting and saving it, which lasts until the app quits if the
//! save fails.

use super::*;

impl Settings {
    pub fn set_notification_popups(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            notification_popups: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(NOTIFICATION_POPUPS, value)
    }

    pub fn set_notification_sound(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            notification_sound: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(NOTIFICATION_SOUND, value)
    }

    // The setters apply the setting, which only lasts until the app quits
    // when it can't be saved.

    pub fn set_close_to_tray(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            close_to_tray: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(CLOSE_TO_TRAY, value)
    }

    pub fn set_worktree_by_default(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            worktree_by_default: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(WORKTREE_BY_DEFAULT, value)
    }

    pub fn set_project_names(
        store: &Store,
        value: bool,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            project_names: value,
            ..*cx.global::<Self>()
        });
        store.set_flag(PROJECT_NAMES, value)
    }

    pub fn set_rail_width(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            rail_width: value,
            ..*cx.global::<Self>()
        });
        store.set_number(RAIL_WIDTH, value.into())
    }

    pub fn set_shell_share(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            shell_share: value,
            ..*cx.global::<Self>()
        });
        store.set_number(SHELL_SHARE, value.into())
    }

    pub fn set_sidebar_width(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            sidebar_width: value,
            ..*cx.global::<Self>()
        });
        store.set_number(SIDEBAR_WIDTH, value.into())
    }

    /// Callers put the theme in the new mode with `appearance::apply`.
    pub fn set_appearance(
        store: &Store,
        value: Appearance,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            appearance: value,
            ..*cx.global::<Self>()
        });
        store.set_text(APPEARANCE, value.key())
    }

    /// Callers make it take effect with `appearance::apply_motion`.
    pub fn set_motion(
        store: &Store,
        value: Motion,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            motion: value,
            ..*cx.global::<Self>()
        });
        store.set_text(MOTION, value.key())
    }

    /// Kept within the sizes the terminals zoom to.
    pub fn set_terminal_font_size(
        store: &Store,
        value: f32,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        let value = value.clamp(MIN_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE);
        cx.set_global(Self {
            terminal_font_size: value,
            ..*cx.global::<Self>()
        });
        store.set_number(TERMINAL_FONT_SIZE, value.into())
    }

    /// Callers put the theme in the new family with `appearance::apply`.
    pub fn set_theme(
        store: &Store,
        value: ThemeFamily,
        cx: &mut App,
    ) -> impl Future<Output = Result<()>> + use<> {
        cx.set_global(Self {
            theme: value,
            ..*cx.global::<Self>()
        });
        store.set_text(THEME, value.key())
    }
}
