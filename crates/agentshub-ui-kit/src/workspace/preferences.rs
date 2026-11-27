//! The settings screen and the preferences that change the app's look.

use super::*;
use crate::dialogs;

impl Workspace {
    /// Shows the settings in place of the chat, or the chat again. The focus
    /// leaves the covered terminals, so keys can't reach a CLI out of sight.
    pub fn show_settings(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_shown = shown;
        // A chat still opening mustn't take the view from the settings.
        self.apply(Intent::ShowSettings(shown), cx);
        if shown {
            self.focus_handle.focus(window, cx);
        } else {
            self.focus_chat(window, cx);
        }
        self.cover_terminals(cx);
        self.set_focused(window.is_window_active(), cx);
        cx.notify();
    }

    /// Saves the theme's mode and puts the theme in it.
    pub(super) fn set_appearance(
        &mut self,
        value: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = Settings::set_appearance(value, cx);
        self.save_setting(saved, cx);
        appearance::apply(window, cx);
        cx.notify();
    }

    /// Shows the layout's chat numbers while Alt alone is held, and its
    /// project numbers while Ctrl alone is.
    pub(super) fn modifiers_changed(&mut self, modifiers: Modifiers, cx: &mut Context<Self>) {
        let held = if modifiers == Modifiers::alt() {
            Some(Held::Chats)
        } else if modifiers == Modifiers::control() {
            Some(Held::Projects)
        } else {
            None
        };
        self.release_modifiers(cx);
        if let Some(held) = held {
            self.reveal_numbers = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(REVEAL_NUMBERS_AFTER).await;
                this.update(cx, |this, cx| this.layout.hold_numbers(Some(held), cx))
                    .ok();
            }));
        }
    }

    /// Hides the layout's numbers, as when the window loses the keyboard
    /// while a modifier is down and never hears it let go.
    pub fn release_modifiers(&mut self, cx: &mut Context<Self>) {
        self.reveal_numbers = None;
        self.layout.hold_numbers(None, cx);
    }

    /// Makes the terminals' text `by` points bigger, or smaller when negative.
    pub(super) fn zoom_terminals(&mut self, by: f32, _: &mut Window, cx: &mut Context<Self>) {
        let size = cx.global::<Settings>().terminal_font_size + by;
        let saved = Settings::set_terminal_font_size(size, cx);
        self.save_setting(saved, cx);
        cx.notify();
    }

    pub(super) fn set_motion(&mut self, value: Motion, _: &mut Window, cx: &mut Context<Self>) {
        let saved = Settings::set_motion(value, cx);
        self.save_setting(saved, cx);
        appearance::apply_motion(cx);
        cx.notify();
    }

    /// Saves the family of themes and puts the theme in it.
    pub(super) fn set_theme(
        &mut self,
        value: ThemeFamily,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = Settings::set_theme(value, cx);
        self.save_setting(saved, cx);
        appearance::apply(window, cx);
        cx.notify();
    }

    /// Saves the layout the app opens in next and, unless the window is in
    /// it, asks whether to restart into it now.
    fn set_layout(&mut self, value: Layout, window: &mut Window, cx: &mut Context<Self>) {
        let saved = Settings::set_layout(value, cx);
        self.save_setting(saved, cx);
        if value == cx.global::<Settings>().layout {
            return;
        }
        let running = self
            .runtime
            .read(cx)
            .open_chats()
            .filter(|chat| chat.running)
            .count();
        let this = cx.entity().downgrade();
        dialogs::restart_for_layout(
            window,
            cx,
            value,
            running,
            move |window, cx| {
                this.update(cx, |this, cx| this.restart_into_layout(window, cx))
                    .ok();
            },
            |_, _| {},
        );
    }

    /// Restarts into the layout the app opens in next, with the window where
    /// it is. Shutdown waits for the writes asked for, so the bounds are
    /// saved before the next start reads them.
    fn restart_into_layout(&self, window: &Window, cx: &mut App) {
        let saved = Settings::save_restart_bounds(window.inner_window_bounds());
        self.save_setting(saved, cx);
        cx.restart();
    }

    /// Applies what the settings view asked for.
    pub(super) fn settings_intent(
        &mut self,
        intent: SettingsIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            SettingsIntent::Close => self.show_settings(false, window, cx),
            SettingsIntent::SetTheme(theme) => self.set_theme(theme, window, cx),
            SettingsIntent::SetLayout(value) => self.set_layout(value, window, cx),
            SettingsIntent::RestartForLayout => self.restart_into_layout(window, cx),
            SettingsIntent::SetAppearance(value) => self.set_appearance(value, window, cx),
            SettingsIntent::SetMotion(value) => self.set_motion(value, window, cx),
            SettingsIntent::Zoom(by) => self.zoom_terminals(by, window, cx),
            SettingsIntent::SetCloseToTray(value) => {
                let saved = Settings::set_close_to_tray(value, cx);
                self.save_setting(saved, cx);
            }
            SettingsIntent::SetWorktreeByDefault(value) => {
                let saved = Settings::set_worktree_by_default(value, cx);
                self.save_setting(saved, cx);
            }
            SettingsIntent::SetEditor(value) => {
                let saved = Settings::set_editor(value, cx);
                self.save_setting(saved, cx);
            }
            SettingsIntent::SetNotificationPopups(value) => {
                let saved = Settings::set_notification_popups(value, cx);
                self.save_setting(saved, cx);
            }
            SettingsIntent::SetNotificationSound(value) => {
                let saved = Settings::set_notification_sound(value, cx);
                self.save_setting(saved, cx);
            }
            SettingsIntent::TestNotifications => alerts::test(cx),
            SettingsIntent::ShowShortcuts => {
                self.show_shortcuts(&ShowShortcuts, window, cx);
            }
            SettingsIntent::Export => self.export_database(window, cx),
            SettingsIntent::Import => self.import_database(window, cx),
        }
        cx.notify();
    }
}
