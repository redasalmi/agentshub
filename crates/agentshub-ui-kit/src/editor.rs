//! The editors chats open in: the installed ones, their icons and the menu
//! that picks the default.

use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::*;

use agentshub_app::editors::{ICON_SIZE, Installed};
use agentshub_app::settings::Settings;
use agentshub_core::editors::{self, Editor, Found};

use crate::actions::OpenInEditor;
use crate::chat_body::{ChatIntent, On};

/// An installed editor, with its icon.
#[derive(Clone)]
pub struct EditorChoice {
    pub editor: Editor,
    pub icon: Option<ImageSource>,
}

/// The editor chats open in, and every installed one, once they are known.
pub fn editor_choices(cx: &App) -> Option<(EditorChoice, Vec<EditorChoice>)> {
    let installed = &cx.try_global::<Installed>()?.0;
    let found: Vec<Found> = installed.iter().map(|each| each.found.clone()).collect();
    let current = editors::pick(&found, cx.global::<Settings>().editor)?.editor;
    let all: Vec<EditorChoice> = installed
        .iter()
        .map(|each| EditorChoice {
            editor: each.found.editor,
            icon: each.icon.clone(),
        })
        .collect();
    let current = all.iter().find(|choice| choice.editor == current)?.clone();
    Some((current, all))
}

/// An editor's icon, or a folder of code when none was found.
pub fn editor_icon(icon: Option<&ImageSource>) -> AnyElement {
    let size = px(f32::from(ICON_SIZE));
    match icon {
        Some(icon) => img(icon.clone()).flex_none().size(size).into_any_element(),
        None => Icon::new(ExtraIcon::FolderCode)
            .with_size(size)
            .into_any_element(),
    }
}

/// What picking an editor in `editor_menu` does.
pub type SetEditor = Rc<dyn Fn(Editor, &mut Window, &mut App)>;

/// The installed editors with their icons, `current` checked. Picking one
/// only makes it the default.
pub fn editor_menu(
    menu: PopupMenu,
    current: Editor,
    all: &[EditorChoice],
    set: &SetEditor,
) -> PopupMenu {
    all.iter()
        .fold(menu.label("Default editor"), |menu, choice| {
            let (editor, icon) = (choice.editor, choice.icon.clone());
            let set = Rc::clone(set);
            menu.item(
                PopupMenuItem::element(move |_, _| {
                    h_flex()
                        .gap_2p5()
                        .child(editor_icon(icon.as_ref()))
                        .child(editor.name())
                })
                .checked(editor == current)
                .on_click(move |_, window, cx| set(editor, window, cx)),
            )
        })
}

/// Opens the chat's folder in the default editor; its menu changes which one
/// that is. A filled surface, the one ghost buttons take on hover, tells it
/// apart from the header's plain buttons as a control with a menu.
pub fn editor_button(current: &EditorChoice, all: Vec<EditorChoice>, on: On) -> impl IntoElement {
    let editor = current.editor;
    let set: SetEditor = {
        let on = Rc::clone(&on);
        Rc::new(move |editor, window, cx| on(ChatIntent::SetEditor(editor), window, cx))
    };
    let open = format!("Open in {}", editor.name());
    DropdownButton::new("editor")
        .secondary()
        .small()
        .button(
            Button::new("open-editor")
                .small()
                .child(editor_icon(current.icon.as_ref()))
                .accessibility_label(open.clone())
                .tooltip_with_action(open, &OpenInEditor, None)
                .on_click(move |_, window, cx| on(ChatIntent::OpenEditor(editor), window, cx)),
        )
        .menu_accessibility_label("Choose the default editor")
        .dropdown_menu(move |menu, _, _| editor_menu(menu, editor, &all, &set))
}
