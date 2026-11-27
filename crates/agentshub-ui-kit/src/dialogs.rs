//! The dialogs the layouts share: starting a new chat, renaming one, and the
//! questions asked before closing, deleting, archiving, removing or importing.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Theme, ThemeStyled as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use agentshub_core::agent::Agent;
use agentshub_core::format::{home_relative, plural};
use agentshub_core::store;

use crate::widgets::path_line;

/// What the new chat dialog shows and starts with.
pub struct NewChatArgs {
    /// The project's folder.
    pub path: PathBuf,
    /// The agents installed, in the order they are offered.
    pub agents: Vec<Agent>,
    /// Whether the project is in git, so its chat can get a worktree.
    pub in_git: bool,
    /// Whether the worktree box starts ticked in a git project.
    pub worktree_by_default: bool,
}

/// Asks which agent to start, under which name and whether in a worktree.
/// `on_start` gets the agent picked, the name typed or `None`, and the
/// worktree box, once the dialog has closed.
#[expect(
    clippy::too_many_lines,
    reason = "the dialog's element tree and the state its controls share"
)]
pub fn new_chat(
    window: &mut Window,
    cx: &mut App,
    args: NewChatArgs,
    on_start: impl Fn(Agent, Option<String>, bool, &mut Window, &mut App) + 'static,
) {
    let NewChatArgs {
        path,
        agents,
        in_git,
        worktree_by_default,
    } = args;
    let agents = Rc::new(agents);
    let none_installed = agents.is_empty();
    // Whether the chat gets a worktree starts as the settings say.
    let use_worktree = Rc::new(Cell::new(in_git && worktree_by_default));
    let chosen = Rc::new(Cell::new(agents.first().copied()));
    // Left empty, the chat is titled after its agent's sessions.
    let title = cx.new(|cx| InputState::new(window, cx).placeholder("Chat name (optional)"));
    let picker = cx.focus_handle().tab_stop(true);
    let start = Rc::new({
        let (title, use_worktree) = (title.clone(), Rc::clone(&use_worktree));
        move |agent: Agent, window: &mut Window, cx: &mut App| {
            let title = title.read(cx).value().to_string();
            window.close_dialog(cx);
            let worktree = use_worktree.get();
            on_start(
                agent,
                Some(title).filter(|title| !title.is_empty()),
                worktree,
                window,
                cx,
            );
        }
    });

    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) && !none_installed {
            picker.focus(window, cx);
        }
        let theme = cx.theme();
        let palette = crate::appearance::palette(cx);
        dialog
            .title("Start a coding agent")
            // Enter, in the name too, starts the chosen agent. It closes
            // the dialog itself, before the chat opens.
            .on_ok({
                let (start, chosen) = (Rc::clone(&start), Rc::clone(&chosen));
                move |_, window, cx| {
                    if let Some(agent) = chosen.get() {
                        start(agent, window, cx);
                    }
                    false
                }
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(path_line(&path, cx))
                    .map(|content| {
                        if none_installed {
                            return content.child(div().text_sm().child(
                                "No coding agent is installed. Install Claude Code, \
                                     Codex or Pi, with its command on your PATH.",
                            ));
                        }
                        let picked = chosen.get();
                        content
                            .child(
                                h_flex()
                                    .id("agents")
                                    .track_focus(&picker)
                                    .relative()
                                    .rounded(theme.radius)
                                    .when(
                                        picker.is_focused(window)
                                            && window.last_input_was_keyboard(),
                                        |this| this.focus_ring_style(window, cx),
                                    )
                                    .gap_2()
                                    .on_key_down({
                                        let (agents, chosen, start) = (
                                            Rc::clone(&agents),
                                            Rc::clone(&chosen),
                                            Rc::clone(&start),
                                        );
                                        move |event: &KeyDownEvent, window, cx| {
                                            let keystroke = &event.keystroke;
                                            if keystroke.modifiers.number_of_modifiers() > 0 {
                                                return;
                                            }
                                            let at = chosen
                                                .get()
                                                .and_then(|agent| {
                                                    agents.iter().position(|a| *a == agent)
                                                })
                                                .unwrap_or_default();
                                            let to = match keystroke.key.as_str() {
                                                "left" => at.checked_sub(1),
                                                "right" => Some(at + 1),
                                                key => {
                                                    if let Some(agent) = key
                                                        .parse::<usize>()
                                                        .ok()
                                                        .and_then(|n| n.checked_sub(1))
                                                        .and_then(|n| agents.get(n))
                                                    {
                                                        cx.stop_propagation();
                                                        start(*agent, window, cx);
                                                    }
                                                    return;
                                                }
                                            };
                                            if let Some(agent) = to.and_then(|to| agents.get(to)) {
                                                chosen.set(Some(*agent));
                                                window.refresh();
                                            }
                                            cx.stop_propagation();
                                        }
                                    })
                                    .children(agents.iter().enumerate().map(|(index, agent)| {
                                        let agent = *agent;
                                        let start = Rc::clone(&start);
                                        let picked = picked == Some(agent);
                                        v_flex()
                                            .id(("agent", index as u64))
                                            .flex_1()
                                            .min_w_0()
                                            .px_3()
                                            .py_2p5()
                                            .gap_1()
                                            .rounded(theme.radius)
                                            .border_1()
                                            .cursor_pointer()
                                            .map(|card| {
                                                if picked {
                                                    card.border_color(palette.accent)
                                                        .bg(palette.accent_soft)
                                                } else {
                                                    card.border_color(theme.border).hover(|style| {
                                                        style.border_color(palette.border_strong)
                                                    })
                                                }
                                            })
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_medium()
                                                    .truncate()
                                                    .child(agent.display_name()),
                                            )
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .text_xs()
                                                    .font_family(theme.mono_font_family.clone())
                                                    .text_color(theme.muted_foreground)
                                                    .child(agent.program())
                                                    .child(
                                                        div()
                                                            .px_1p5()
                                                            .rounded(px(4.))
                                                            .bg(palette.strong)
                                                            .child((index + 1).to_string()),
                                                    ),
                                            )
                                            .on_click(move |_, window, cx| start(agent, window, cx))
                                    })),
                            )
                            .child(Input::new(&title))
                            .children(picked.map(|agent| {
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "Enter starts {}. {} or ← → picks another.",
                                        agent.display_name(),
                                        match agents.len() {
                                            1 => "1".to_string(),
                                            n => format!("1–{n}"),
                                        }
                                    ))
                            }))
                    })
                    .when(in_git, |content| {
                        let use_worktree = Rc::clone(&use_worktree);
                        content.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    Checkbox::new("new-chat-worktree")
                                        .label("Start in a new git worktree")
                                        .checked(use_worktree.get())
                                        .on_click(move |checked, window, _| {
                                            use_worktree.set(*checked);
                                            window.refresh();
                                        }),
                                )
                                .child(
                                    div()
                                        .pl_6()
                                        .text_sm()
                                        .line_height(rems(1.4))
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            "The agent works on a branch of its own, in a \
                                             copy of the project as it is now, with its \
                                             uncommitted changes and ignored files like .env. \
                                             Other chats don't touch it.",
                                        ),
                                ),
                        )
                    }),
            )
    });
}

/// Asks for a chat's new title, starting from `title`. `on_rename` gets
/// the one typed, unless it is `title` untouched.
pub fn rename(
    window: &mut Window,
    cx: &mut App,
    title: SharedString,
    on_rename: impl Fn(String, &mut App) + 'static,
) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(title.clone())
            .placeholder("Its agent's title")
    });
    let on_rename = Rc::new(on_rename);
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            input.focus_handle(cx).focus(window, cx);
            input.update(cx, |input, cx| input.select_all(window, cx));
        }
        let (on_rename, input, title) = (Rc::clone(&on_rename), input.clone(), title.clone());
        dialog
            .title("Rename this chat")
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Leave it empty to title it after its agent's sessions."),
                    )
                    .child(Input::new(&input)),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                    .child(
                        DialogAction::new().child(Button::new("rename").primary().label("Rename")),
                    ),
            )
            // Enter in the input confirms too.
            .on_ok(move |_, _, cx| {
                let value = input.read(cx).value();
                // An untouched title stays the agent's, and follows it.
                if value != title {
                    on_rename(value.to_string(), cx);
                }
                true
            })
    });
}

/// A question asked before doing something, in an alert.
pub struct Confirm {
    pub title: SharedString,
    pub description: SharedString,
    /// The label of the button that goes ahead.
    pub ok_text: SharedString,
    /// Whether going ahead loses something, which colors its button.
    pub danger: bool,
}

/// Asks `confirm`'s question, and calls `on_ok` once the user goes ahead.
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    confirm: Confirm,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = Rc::clone(&on_ok);
        alert
            .confirm()
            .title(confirm.title.clone())
            .description(confirm.description.clone())
            .ok_text(confirm.ok_text.clone())
            .when(confirm.danger, |alert| {
                alert.ok_variant(ButtonVariant::Danger)
            })
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}

/// What a database about to be imported holds.
pub struct Import {
    /// The database file picked.
    pub path: PathBuf,
    pub projects: Vec<store::Project>,
    /// Where the database it replaces is backed up, as shown.
    pub backup_dir: String,
}

/// Shows what `import`'s database holds, and asks whether to restart with
/// it in place of the user's. `on_import` is called once they confirm, and
/// `on_close` whenever the dialog closes.
pub fn import(
    window: &mut Window,
    cx: &mut App,
    import: Import,
    on_import: impl Fn(&mut Window, &mut App) + 'static,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) {
    let Import {
        path,
        projects,
        backup_dir: dir,
    } = import;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let chats: usize = projects.iter().map(|project| project.chats.len()).sum();
    let holds = match (projects.len(), chats) {
        (0, _) => format!("{name} has no projects."),
        (projects, chats) => format!(
            "{name} has {} and {}:",
            plural(projects, "project"),
            plural(chats, "chat")
        ),
    };
    let projects: Rc<[store::Project]> = projects.into();
    let (on_import, on_close) = (Rc::new(on_import), Rc::new(on_close));
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let theme = cx.theme();
        let (on_import, on_close) = (Rc::clone(&on_import), Rc::clone(&on_close));
        alert
            .confirm()
            .width(px(520.))
            .title("Replace your chats with these?")
            .description(holds.clone())
            .when(!projects.is_empty(), |alert| {
                alert.child(render_import(&projects, theme))
            })
            .child(
                div()
                    .text_sm()
                    .line_height(rems(1.4))
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "Agents Hub restarts to use them, which stops the agents running \
                         now. Your projects, chats and settings are replaced; the database \
                         holding them is backed up first in {dir}, and importing that \
                         backup brings them back."
                    )),
            )
            .ok_text("Import and restart")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                on_import(window, cx);
                true
            })
            .on_close(move |_, window, cx| on_close(window, cx))
    });
}

/// The projects of a database about to be imported, each with its first few
/// chats.
fn render_import(projects: &[store::Project], theme: &Theme) -> impl IntoElement {
    /// The chats listed under each project; the others are counted.
    const SHOWN_CHATS: usize = 3;
    v_flex()
        .id("import-projects")
        .max_h(px(260.))
        .overflow_y_scroll()
        .gap_3()
        .p_3()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .children(projects.iter().map(|project| {
            let more = project.chats.len().saturating_sub(SHOWN_CHATS);
            v_flex()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_medium()
                                .text_color(theme.foreground)
                                .child(project.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(home_relative(&project.path)),
                        ),
                )
                .children(project.chats.iter().take(SHOWN_CHATS).map(|chat| {
                    div()
                        .pl_3()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(chat.title.clone().unwrap_or_else(|| "New chat".into()))
                }))
                .when(more > 0, |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child(format!("and {} more", plural(more, "chat"))),
                    )
                })
                .when(project.chats.is_empty(), |list| {
                    list.child(
                        div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child("No chats"),
                    )
                })
        }))
}
