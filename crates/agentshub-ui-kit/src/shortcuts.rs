//! The list of keyboard shortcuts.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::actions::{
    AddProject, CloseAllChats, CloseIdleChats, NewChat, NextNeedingYou, OpenInEditor, OpenSearch,
    RenameChat, ShowRunning, ShowShortcuts, ToggleHistory, ToggleSettings, ToggleShell,
};

pub fn render_shortcuts(window: &Window, cx: &App) -> impl IntoElement {
    let bound: [(&'static str, Box<dyn Action>); 13] = [
        (
            "Search chats, messages, projects and commands",
            Box::new(OpenSearch),
        ),
        (
            "Show the running chats of every project",
            Box::new(ShowRunning),
        ),
        (
            "Open the next chat that needs you",
            Box::new(NextNeedingYou),
        ),
        ("Start an agent in the selected project", Box::new(NewChat)),
        ("Show or hide the project's shell", Box::new(ToggleShell)),
        (
            "Open the chat's folder in your code editor",
            Box::new(OpenInEditor),
        ),
        ("Show or hide the earlier sessions", Box::new(ToggleHistory)),
        ("Rename the chat", Box::new(RenameChat)),
        ("Add a project", Box::new(AddProject)),
        ("Open or close the settings", Box::new(ToggleSettings)),
        ("Close the idle chats", Box::new(CloseIdleChats)),
        ("Close all the chats", Box::new(CloseAllChats)),
        ("Show the keyboard shortcuts", Box::new(ShowShortcuts)),
    ];
    let unbound: [(&'static str, SharedString); 4] = [
        (
            "Open a chat by its place in the list; hold Alt to see the numbers",
            "Alt+1…9".into(),
        ),
        (
            "Select a project by its place in the rail; hold Ctrl to see the numbers",
            "Ctrl+1…9".into(),
        ),
        (
            "Make the terminals' text bigger or smaller",
            "Ctrl+scroll".into(),
        ),
        ("Copy or paste in a terminal", "Ctrl+Shift+C / V".into()),
    ];
    let rows = bound
        .iter()
        .filter_map(|(label, action)| {
            crate::palette::shortcut(action.as_ref(), window).map(|keys| (*label, keys))
        })
        .chain(unbound);
    let theme = cx.theme();
    v_flex().children(rows.map(|(label, keys)| {
        h_flex()
            .py_2()
            .gap_4()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(div().flex_1().min_w_0().child(label))
            .child(
                div()
                    .flex_none()
                    .px_1p5()
                    .rounded(px(4.))
                    .bg(crate::theme::palette(cx).strong)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(keys),
            )
    }))
}
