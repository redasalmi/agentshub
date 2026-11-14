//! The settings, shown in place of the chat.

use std::rc::Rc;

use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::alerts;
use crate::appearance;
use crate::palette;
use crate::settings::{
    Appearance, DEFAULT_TERMINAL_FONT_SIZE, MAX_TERMINAL_FONT_SIZE, MIN_TERMINAL_FONT_SIZE, Motion,
    Settings, ThemeFamily,
};

use super::{SHORTCUTS_WORK_ANYWHERE, ShowShortcuts, ToggleSettings, Workspace};

/// How wide the settings' groups grow.
const SETTINGS_WIDTH: Pixels = px(640.);
/// The least room beside the sidebar in which the settings keep wide margins.
const MIN_SETTINGS_MARGINS_WIDTH: Pixels = px(560.);
/// The least room a setting's name and description keep beside its control;
/// with less, the control moves below them.
const MIN_SETTING_TEXT_WIDTH: Pixels = px(200.);
/// How a segmented control's thumb travels to the option picked.
const SEGMENT_SLIDE: SpringConfig = SpringConfig::new(420., 32., 1.);

/// The settings, on a sheet `width` wide.
pub(super) fn render_settings(
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let theme_focus = choice_focus("theme", ThemeFamily::ALL.len(), window, cx);
    let mode_focus = choice_focus("appearance", Appearance::ALL.len(), window, cx);
    let motion_focus = choice_focus("motion", Motion::ALL.len(), window, cx);
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let Settings {
        close_to_tray,
        appearance: current,
        theme: family,
        notification_popups,
        notification_sound,
        worktree_by_default,
        motion,
        terminal_font_size,
        ..
    } = *cx.global::<Settings>();
    let dark = cx.theme().is_dark();

    v_flex()
        .id("settings")
        .size_full()
        .overflow_y_scroll()
        // Narrow, the margins give their room to the settings.
        .map(|this| {
            if width < MIN_SETTINGS_MARGINS_WIDTH {
                this.px_6()
            } else {
                this.px_10()
            }
        })
        .py_8()
        .gap_8()
        .child(
            h_flex()
                .max_w(SETTINGS_WIDTH)
                .justify_between()
                .child(
                    div()
                        .font_family(appearance::DISPLAY)
                        .text_3xl()
                        .child("Settings"),
                )
                .child(
                    Button::new("close-settings")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip_with_action("Close the settings", &ToggleSettings, None)
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.show_settings(false, window, cx)
                            }),
                        ),
                ),
        )
        .child(settings_group(
            "Look and feel",
            vec![
                setting_row(
                    "Theme",
                    None,
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(ThemeFamily::ALL.into_iter().zip(theme_focus).map(
                            |(each, focus)| {
                            let [ground, sheet, primary] = appearance::swatch(each, dark, cx);
                            let picked = each == family;
                            choice(v_flex().id(each.label()), &focus, picked, window, cx)
                                .w(px(112.))
                                .p_1p5()
                                .gap_1p5()
                                .rounded(theme.radius_lg)
                                .border_2()
                                .cursor_pointer()
                                .map(|card| {
                                    if picked {
                                        card.border_color(palette.accent)
                                    } else {
                                        card.border_color(theme.border).hover(|style| {
                                            style.border_color(palette.border_strong)
                                        })
                                    }
                                })
                                // A sidebar beside a sheet with its primary
                                // color on it: the theme in miniature.
                                .child(
                                    h_flex()
                                        .h(px(44.))
                                        .rounded(theme.radius)
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(theme.border)
                                        .child(div().w(px(28.)).h_full().bg(ground))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .h_full()
                                                .p_1p5()
                                                .gap_1()
                                                .bg(sheet)
                                                .child(
                                                    div().w(px(36.)).h(px(5.)).rounded_full().bg(primary),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(48.))
                                                        .h(px(4.))
                                                        .rounded_full()
                                                        .bg(ground),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .px_1()
                                        .text_xs()
                                        .when(picked, |this| this.font_medium())
                                        .child(each.label()),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_theme(each, window, cx)
                                }))
                        },
                        ))
                        .into_any_element(),
                    cx,
                ),
                setting_row(
                    "Mode",
                    Some("System follows your desktop's light or dark preference."),
                    segmented(
                        "appearance",
                        Appearance::ALL.map(|each| (each, each.label())).to_vec(),
                        current,
                        mode_focus,
                        |this, value, window, cx| this.set_appearance(value, window, cx),
                        window,
                        cx,
                    ),
                    cx,
                ),
                setting_row(
                    "Motion",
                    Some(
                        "System follows your desktop's reduced-motion preference. Reduced keeps \
                         changes of color, and slows or stills what moves.",
                    ),
                    segmented(
                        "motion",
                        Motion::ALL.map(|each| (each, each.label())).to_vec(),
                        motion,
                        motion_focus,
                        |this, value, window, cx| this.set_motion(value, window, cx),
                        window,
                        cx,
                    ),
                    cx,
                ),
                setting_row(
                    "Terminal text",
                    Some("Ctrl+scroll over a terminal zooms it too."),
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("terminal-text-smaller")
                                .small()
                                .ghost()
                                .icon(IconName::Minus)
                                .tooltip("Smaller")
                                .disabled(terminal_font_size <= MIN_TERMINAL_FONT_SIZE)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.zoom_terminals(-1., window, cx)
                                })),
                        )
                        .child(
                            div()
                                .id("terminal-text-size")
                                .min_w(px(52.))
                                .text_center()
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .cursor_pointer()
                                .tooltip(|window, cx| {
                                    Tooltip::new("Double click to reset").build(window, cx)
                                })
                                .child(format!("{terminal_font_size} px"))
                                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                    if event.click_count() == 2 {
                                        let reset = DEFAULT_TERMINAL_FONT_SIZE
                                            - cx.global::<Settings>().terminal_font_size;
                                        this.zoom_terminals(reset, window, cx)
                                    }
                                })),
                        )
                        .child(
                            Button::new("terminal-text-bigger")
                                .small()
                                .ghost()
                                .icon(IconName::Plus)
                                .tooltip("Bigger")
                                .disabled(terminal_font_size >= MAX_TERMINAL_FONT_SIZE)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.zoom_terminals(1., window, cx)
                                })),
                        )
                        .into_any_element(),
                    cx,
                ),
            ],
            cx,
        ))
        .child(settings_group(
            "Behavior",
            vec![
                setting_row(
                    "Keep running in the tray",
                    Some(
                        "Agents keep working while the window is closed. Choose Agents Hub in \
                         the tray to open it again, or Quit there to stop them.",
                    ),
                    Switch::new("close-to-tray")
                        .checked(close_to_tray)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            let saved = Settings::set_close_to_tray(&this.store, *checked, cx);
                            this.save_setting(saved, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    cx,
                ),
                setting_row(
                    "Start chats in a git worktree",
                    Some(
                        "In projects that use git, each new chat gets a branch and a checkout of \
                         its own, so agents don't edit each other's files. You can still untick \
                         it when you start a chat. Deleting the chat removes its worktree unless \
                         it has changes nobody committed; its branch stays.",
                    ),
                    Switch::new("worktree-by-default")
                        .checked(worktree_by_default)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            let saved = Settings::set_worktree_by_default(&this.store, *checked, cx);
                            this.save_setting(saved, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    cx,
                ),
            ],
            cx,
        ))
        .child(settings_group(
            "Notifications",
            vec![
                setting_row(
                    "Show a notification",
                    Some(
                        "When an agent finishes, needs you or stops on an error while you're \
                         looking elsewhere. Clicking it opens its chat.",
                    ),
                    Switch::new("notification-popups")
                        .checked(notification_popups)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            let saved = Settings::set_notification_popups(&this.store, *checked, cx);
                            this.save_setting(saved, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    cx,
                ),
                setting_row(
                    "Play a sound",
                    None,
                    Switch::new("notification-sound")
                        .checked(notification_sound)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            let saved = Settings::set_notification_sound(&this.store, *checked, cx);
                            this.save_setting(saved, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    cx,
                ),
                setting_row(
                    "Try them",
                    None,
                    Button::new("test-notification")
                        .small()
                        .label("Send a test")
                        .disabled(!notification_popups && !notification_sound)
                        .on_click(|_, _, cx| alerts::test(cx))
                        .into_any_element(),
                    cx,
                ),
            ],
            cx,
        ))
        .child(settings_group(
            "Keyboard",
            vec![setting_row(
                "Keyboard shortcuts",
                Some(SHORTCUTS_WORK_ANYWHERE),
                Button::new("show-shortcuts")
                    .small()
                    .label(match palette::shortcut(&ShowShortcuts, window) {
                        Some(keys) => format!("Show  {keys}"),
                        None => "Show".into(),
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_shortcuts(&ShowShortcuts, window, cx)
                    }))
                    .into_any_element(),
                cx,
            )],
            cx,
        ))
        .child(settings_group(
            "Data",
            vec![
                setting_row(
                    "Export",
                    Some("Writes a copy of the database, with every chat and its history, where you choose."),
                    Button::new("export-database")
                        .small()
                        .icon(IconName::HardDrive)
                        .label("Export a copy")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.export_database(window, cx)
                        }))
                        .into_any_element(),
                    cx,
                ),
                setting_row(
                    "Import",
                    Some("Replaces the database with one Agents Hub wrote, such as an export, and backs it up first."),
                    Button::new("import-database")
                        .small()
                        .label("Import…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.import_database(window, cx)
                        }))
                        .into_any_element(),
                    cx,
                ),
            ],
            cx,
        ))
        .child(
            div()
                .max_w(SETTINGS_WIDTH)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(concat!("Agents Hub ", env!("CARGO_PKG_VERSION"))),
        )
}

/// A titled group of settings, one row each, in a card.
fn settings_group(title: &'static str, rows: Vec<AnyElement>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let last = rows.len().saturating_sub(1);
    v_flex()
        .max_w(SETTINGS_WIDTH)
        .gap_2()
        .child(div().text_sm().font_semibold().child(title))
        .child(
            v_flex()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    div()
                        .when(index < last, |this| {
                            this.border_b_1().border_color(theme.border)
                        })
                        .child(row)
                })),
        )
}

/// A setting: what it is and what it does on the left, its control on the
/// right.
fn setting_row(
    label: &'static str,
    description: Option<&'static str>,
    control: AnyElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .flex_wrap()
        .px_4()
        .py_3()
        .gap_x_6()
        .gap_y_3()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w(MIN_SETTING_TEXT_WIDTH)
                .gap_0p5()
                .child(div().text_sm().font_medium().child(label))
                .children(description.map(|description| {
                    div()
                        .text_xs()
                        .line_height(rems(1.35))
                        .text_color(theme.muted_foreground)
                        .child(description)
                })),
        )
        .child(div().flex_none().max_w_full().child(control))
        .into_any_element()
}

/// One of a few `options`, `current` marked by a thumb that slides to the
/// next one picked, which `pick` then saves.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: Vec<(T, &'static str)>,
    current: T,
    focus: Vec<FocusHandle>,
    pick: impl Fn(&mut Workspace, T, &mut Window, &mut Context<Workspace>) + 'static,
    window: &Window,
    cx: &Context<Workspace>,
) -> AnyElement {
    let theme = cx.theme();
    let palette = appearance::palette(cx);
    let pick = Rc::new(pick);
    let count = options.len() as f32;
    let at = options
        .iter()
        .position(|(value, _)| *value == current)
        .unwrap_or_default() as f32;
    div()
        .p(px(3.))
        .rounded(theme.radius)
        .bg(palette.strong)
        .child(
            h_flex()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(relative(1. / count))
                        .rounded(theme.radius - px(2.))
                        .bg(theme.background)
                        .shadow_xs()
                        .with_spring(
                            (id, 0u64),
                            SpringAnimation::new(SEGMENT_SLIDE).to(at),
                            move |thumb, at| thumb.left(relative(at / count)),
                        ),
                )
                .children(options.into_iter().zip(focus).enumerate().map(
                    |(index, ((value, label), focus))| {
                        let picked = value == current;
                        let pick = pick.clone();
                        choice(div().id((id, index as u64 + 1)), &focus, picked, window, cx)
                            .relative()
                            .min_w(px(76.))
                            .h(px(26.))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .cursor_pointer()
                            .map(|this| {
                                if picked {
                                    this.text_color(theme.foreground).font_medium()
                                } else {
                                    this.text_color(palette.secondary_foreground)
                                        .hover(|style| style.text_color(theme.foreground))
                                }
                            })
                            .child(label)
                            .on_click(
                                cx.listener(move |this, _, window, cx| {
                                    pick(this, value, window, cx)
                                }),
                            )
                    },
                )),
        )
        .into_any_element()
}

/// A focus for each of `count` choices of the setting `key`, kept from one
/// render to the next.
fn choice_focus(
    key: &'static str,
    count: usize,
    window: &mut Window,
    cx: &mut App,
) -> Vec<FocusHandle> {
    (0..count)
        .map(|index| {
            window
                .use_keyed_state((key, index as u64), cx, |_, cx| {
                    cx.focus_handle().tab_stop(true)
                })
                .read(cx)
                .clone()
        })
        .collect()
}

/// One of a setting's choices, which Tab reaches and Enter or Space picks,
/// and which reads as a radio button, `picked` or not.
fn choice(
    element: Stateful<Div>,
    focus: &FocusHandle,
    picked: bool,
    window: &Window,
    cx: &App,
) -> Stateful<Div> {
    element
        .track_focus(focus)
        .role(Role::RadioButton)
        .aria_toggled(if picked {
            Toggled::True
        } else {
            Toggled::False
        })
        .aria_selected(picked)
        // A click picks it without leaving the focus on it, as on a button.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .when(focus.is_focused(window), |this| {
            this.focus_ring_style(window, cx)
        })
}
