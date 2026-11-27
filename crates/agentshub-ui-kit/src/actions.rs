//! The app's actions and the shortcuts that reach them.

use gpui_kit::*;

use agentshub_app::settings::ThemeFamily;

actions!(
    workspace,
    [
        AddProject,
        ArchiveProject,
        ShowArchived,
        CloseAllChats,
        CloseIdleChats,
        ToggleTheme,
        ExportDatabase,
        ImportDatabase,
        ToggleProjectNames,
    ]
);

actions!(
    chat,
    [
        OpenSearch,
        NextNeedingYou,
        ShowRunning,
        ToggleHistory,
        ToggleShell,
        OpenInEditor,
        RenameChat,
        TogglePin,
        DeleteChat,
        ArchiveChat,
    ]
);

actions!(settings, [ToggleSettings, ShowShortcuts]);

actions!(sidebar, [NewChat]);

/// Puts the app in a family of themes.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct UseTheme(pub ThemeFamily);

/// Opens the `n`th chat of the sidebar's list, counting from zero.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenChatAt(pub usize);

/// Selects the `n`th project of the sidebar's rail, counting from zero.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = workspace, no_json)]
pub struct SelectProjectAt(pub usize);

/// How many chats and projects have a shortcut of their own.
pub const NUMBERED: usize = 9;

// The shortcuts are bound without a context, so they reach the workspace
// before a focused terminal sends them to its CLI. They hold Ctrl and Shift
// together, or Alt or Ctrl with a digit, which terminal programs leave alone,
// so the CLIs keep the plain Ctrl keys.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-k", OpenSearch, None),
        KeyBinding::new("ctrl-shift-n", NewChat, None),
        KeyBinding::new("ctrl-`", ToggleShell, None),
        KeyBinding::new("ctrl-shift-h", ToggleHistory, None),
        KeyBinding::new("ctrl-shift-e", OpenInEditor, None),
        KeyBinding::new("f2", RenameChat, None),
        KeyBinding::new("ctrl-shift-o", AddProject, None),
        KeyBinding::new("ctrl-,", ToggleSettings, None),
        KeyBinding::new("ctrl-shift-w", CloseAllChats, None),
        KeyBinding::new("ctrl-shift-i", CloseIdleChats, None),
        KeyBinding::new("ctrl-shift-a", ArchiveChat, None),
        KeyBinding::new("ctrl-0", ShowRunning, None),
        KeyBinding::new("ctrl-shift-j", NextNeedingYou, None),
        // Ctrl+Shift+/. Ctrl+/ is the CLIs' own: terminals send it as Ctrl+_,
        // which they undo with.
        KeyBinding::new("ctrl-?", ShowShortcuts, None),
    ]);
    let open_chat: [&str; NUMBERED] = [
        "alt-1", "alt-2", "alt-3", "alt-4", "alt-5", "alt-6", "alt-7", "alt-8", "alt-9",
    ];
    let select_project: [&str; NUMBERED] = [
        "ctrl-1", "ctrl-2", "ctrl-3", "ctrl-4", "ctrl-5", "ctrl-6", "ctrl-7", "ctrl-8", "ctrl-9",
    ];
    for (n, (open_chat, select_project)) in open_chat.into_iter().zip(select_project).enumerate() {
        cx.bind_keys([
            KeyBinding::new(open_chat, OpenChatAt(n), None),
            KeyBinding::new(select_project, SelectProjectAt(n), None),
        ]);
    }
}
