//! The editors chats open in: the installed ones, their icons and the menu
//! that picks the default.

use std::rc::Rc;

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::*;

use agentshub_app::editors::{ICON_SIZE, Installed};
use agentshub_app::settings::Settings;
use agentshub_core::editors::{self, Editor, Found};

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
