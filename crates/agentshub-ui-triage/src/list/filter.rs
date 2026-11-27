//! The project filter at the top of the list, and its menu.

use gpui_kit::assets::IconName as ExtraIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::store::Project;
use agentshub_ui_kit::actions::{NUMBERED, NewChat};
use agentshub_ui_kit::layout::{Held, NavEvent};
use agentshub_ui_kit::marks::marks;
use agentshub_ui_kit::status::badge;
use agentshub_ui_kit::theme as appearance;

use super::card::swatch;
use super::{TriageList, emit, project_mark};

/// The widest a project's name gets in the filter and its menu, beside its
/// mark and the buttons there.
const MAX_FILTER_NAME_WIDTH: Pixels = px(200.);
const FILTER_MENU_WIDTH: Pixels = px(300.);

/// A project as the filter's menu lists it.
struct MenuProject {
    id: i64,
    name: SharedString,
    mark: SharedString,
    /// How many of its chats need the user and how many are open, and
    /// whether one of them works.
    counts: (usize, usize, bool),
}

/// The project filter's menu: every project, with how many of its chats need
/// the user or are open, and its own actions, and adding a project.
fn filter_menu(
    menu: PopupMenu,
    list: &Entity<TriageList>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let this = list.read(cx);
    let filter = this.filter;
    let listed: Vec<&Project> = this.listed().collect();
    let marks = marks(listed.iter().map(|project| project.name.as_str()));
    let stack: Vec<i64> = listed.iter().take(3).map(|project| project.id).collect();
    let projects: Vec<MenuProject> = listed
        .iter()
        .zip(marks)
        .map(|(project, mark)| MenuProject {
            id: project.id,
            name: project.name.clone().into(),
            mark: mark.into(),
            counts: this.counts(project),
        })
        .collect();
    let all = {
        let list = list.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            list.update(cx, |list, cx| list.set_filter(None, cx));
        }
    };
    let mut menu = menu
        .min_w(FILTER_MENU_WIDTH)
        .max_w(FILTER_MENU_WIDTH)
        .item(
            PopupMenuItem::element(move |_, cx| {
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(h_flex().w(px(18.)).flex_none().justify_center().children(
                        stack.iter().enumerate().map(|(index, id)| {
                            swatch(*id, cx)
                                .size(px(12.))
                                .border_2()
                                .border_color(cx.theme().popover)
                                .when(index > 0, |this| this.ml(px(-5.)))
                        }),
                    ))
                    .child(div().flex_1().child("All projects"))
                    .when(filter.is_none(), |this| {
                        this.child(Icon::new(IconName::Check).xsmall())
                    })
            })
            .on_click(all),
        )
        .separator();
    for (index, project) in projects.into_iter().enumerate() {
        let current = filter == Some(project.id);
        menu = menu.item(project_item(
            index,
            project,
            current,
            list,
            cx.weak_entity(),
        ));
    }
    menu.separator().item(
        PopupMenuItem::new("Add project…")
            .icon(IconName::FolderOpen)
            .on_click(emit(list, NavEvent::AddProject)),
    )
}

/// The `index`th project of the filter's menu, which shows its chats, with
/// its own actions on hover. `current` when the list shows its chats.
fn project_item(
    index: usize,
    project: MenuProject,
    current: bool,
    list: &Entity<TriageList>,
    menu: WeakEntity<PopupMenu>,
) -> PopupMenuItem {
    let MenuProject {
        id,
        name,
        mark,
        counts: (needing, open, working),
    } = project;
    let filter = {
        let list = list.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            list.update(cx, |list, cx| list.set_filter(Some(id), cx));
        }
    };
    let list = list.clone();
    PopupMenuItem::element(move |_, cx| {
        let held = list.read(cx).held == Some(Held::Projects) && index < NUMBERED;
        let group = SharedString::from(format!("filter-project-{id}"));
        // While Ctrl is held, the mark gives way to the number that filters
        // to the project, so nothing moves.
        let mark = if held {
            SharedString::from((index + 1).to_string())
        } else {
            mark.clone()
        };
        let action = |icon: Icon, tip: &'static str, event: NavEvent| {
            let (list, menu) = (list.clone(), menu.clone());
            Button::new(SharedString::from(format!("{tip}-{id}")))
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(tip)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    menu.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
                    list.update(cx, |_, cx| cx.emit(event.clone()));
                })
        };
        h_flex()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(project_mark(
                mark,
                appearance::project_tint(id, cx),
                px(18.),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(MAX_FILTER_NAME_WIDTH)
                    .truncate()
                    .child(name.clone()),
            )
            .map(|this| match (needing, open) {
                (0, 0) => this,
                (0, open) => this.child(badge(open, false, working, cx)),
                (needing, _) => this.child(badge(needing, true, false, cx)),
            })
            .when(current, |this| {
                this.child(Icon::new(IconName::Check).xsmall())
            })
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .pl_2()
                    .gap_0p5()
                    .items_center()
                    .bg(cx.theme().accent)
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(action(
                        Icon::new(IconName::Plus),
                        "New chat…",
                        NavEvent::NewChat(id),
                    ))
                    .child(action(
                        Icon::new(ExtraIcon::Archive),
                        "Archive…",
                        NavEvent::ArchiveProject(id),
                    ))
                    .child(action(
                        Icon::new(IconName::Delete),
                        "Remove…",
                        NavEvent::RemoveProject(id),
                    )),
            )
    })
    .on_click(filter)
}

impl TriageList {
    /// The project filter and the button that starts a new chat.
    pub(super) fn render_top(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let listed: Vec<&Project> = self.listed().collect();
        let marks = marks(listed.iter().map(|project| project.name.as_str()));
        let shown = self
            .filter
            .and_then(|id| listed.iter().position(|project| project.id == id));
        let lead = match shown {
            Some(at) => project_mark(
                marks[at].clone().into(),
                appearance::project_tint(listed[at].id, cx),
                px(20.),
                cx,
            )
            .into_any_element(),
            None => h_flex()
                .flex_none()
                .px(px(2.))
                .children(listed.iter().take(3).enumerate().map(|(index, project)| {
                    swatch(project.id, cx)
                        .size(px(12.))
                        .border_2()
                        .border_color(theme.sidebar)
                        .when(index > 0, |this| this.ml(px(-3.)))
                }))
                .into_any_element(),
        };
        let name = match shown {
            Some(at) => SharedString::from(listed[at].name.clone()),
            None => "All projects".into(),
        };
        let new_chat = self.new_chat_project();
        h_flex()
            .flex_none()
            .px(px(10.))
            .pt(px(10.))
            .pb_1()
            .gap_1()
            .justify_between()
            .child(
                Button::new("project-filter")
                    .ghost()
                    .h(px(34.))
                    .px_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(lead)
                            .child(
                                div()
                                    .max_w(MAX_FILTER_NAME_WIDTH)
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(name),
                            )
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .xsmall()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .dropdown_menu({
                        let list = cx.entity();
                        move |menu, _, cx| filter_menu(menu, &list, cx)
                    }),
            )
            .child(
                Button::new("new-chat")
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip_with_action("New chat", &NewChat, None)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(match new_chat {
                            Some(project) => NavEvent::NewChat(project),
                            None => NavEvent::AddProject,
                        })
                    })),
            )
    }
}
