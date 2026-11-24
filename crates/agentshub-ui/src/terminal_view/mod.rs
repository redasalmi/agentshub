use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use libghostty_vt::mouse;
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::Scrollbar;

use crate::terminal::{Frame, GridSize, MouseInput, Run, Terminal};
use crate::theme as appearance;
use agentshub_app::settings::Settings;

mod font;
mod input;
mod paint;
mod sprite;
mod symbol;

use font::{terminal_font, wide_font};
#[cfg(test)]
use input::key_code;
use input::{key_input, key_mods, mouse_button, shell_escape};
use paint::{cell_size, paint_frame, paint_marked_text, thumb_bounds};

const CONTEXT: &str = "Terminal";
const SCROLLBAR_WIDTH: Pixels = px(6.);
const MIN_THUMB_HEIGHT: Pixels = px(24.);

actions!(terminal, [Copy, Paste, ZoomIn, ZoomOut, ResetZoom]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // The app binds these globally (focus cycling, copy); while a
        // terminal is focused they belong to the CLI.
        KeyBinding::new("tab", NoAction, Some(CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", NoAction, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-c", Copy, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-v", Paste, Some(CONTEXT)),
    ]);
    font::init(cx);
}

pub fn color(rgb: RgbColor) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.,
        g: rgb.g as f32 / 255.,
        b: rgb.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

pub struct TerminalView {
    terminal: Entity<Terminal>,
    focus_handle: FocusHandle,
    /// Where the grid was last painted, to turn mouse positions into cells.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Trackpads scroll by fractions of a line.
    pending_scroll: f32,
    /// Scrolled with Ctrl held, which zooms instead, by fractions of a step.
    pending_zoom: f32,
    /// The buttons that went down on the grid and haven't been released.
    pressed: Vec<mouse::Button>,
    /// While the scrollbar thumb is dragged, how far below its top it was
    /// grabbed.
    thumb_grab: Option<Pixels>,
    /// Text being composed in an input method, shown at the cursor until it
    /// is committed.
    marked_text: Option<String>,
    _observe_terminal: Subscription,
}

impl TerminalView {
    pub fn new(terminal: Entity<Terminal>, cx: &mut Context<Self>) -> Self {
        let observe_terminal = cx.observe(&terminal, |_, _, cx| cx.notify());
        Self {
            terminal,
            focus_handle: cx.focus_handle(),
            bounds: Rc::default(),
            pending_scroll: 0.,
            pending_zoom: 0.,
            pressed: Vec::new(),
            thumb_grab: None,
            marked_text: None,
            _observe_terminal: observe_terminal,
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = key_input(&event.keystroke) {
            self.terminal.read(cx).key(input);
            if self.marked_text.take().is_some() {
                cx.notify();
            }
            cx.stop_propagation();
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let line_height = cell_size(window, cx).height;
        if event.modifiers.control {
            // A wheel's notch is one step, however many lines it scrolls; a
            // trackpad takes a line's worth of travel for each.
            let steps = match event.delta {
                ScrollDelta::Lines(lines) if lines.y == 0. => 0.,
                ScrollDelta::Lines(lines) => lines.y.signum(),
                ScrollDelta::Pixels(pixels) => {
                    self.pending_zoom += pixels.y / line_height;
                    let steps = self.pending_zoom.trunc();
                    self.pending_zoom -= steps;
                    steps
                }
            };
            for _ in 0..steps.abs() as usize {
                if steps > 0. {
                    window.dispatch_action(Box::new(ZoomIn), cx);
                } else {
                    window.dispatch_action(Box::new(ZoomOut), cx);
                }
            }
            cx.stop_propagation();
            return;
        }
        self.pending_scroll += event.delta.pixel_delta(line_height).y / line_height;
        let lines = self.pending_scroll.trunc();
        self.pending_scroll -= lines;
        if lines != 0. {
            let position = event.position - self.bounds.get().origin;
            // GPUI reports scrolling up as positive, the terminal as negative.
            self.terminal
                .read(cx)
                .scroll(-lines as isize, position.x.into(), position.y.into());
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        let Some(button) = mouse_button(event.button) else {
            return;
        };
        let bounds = self.bounds.get();
        let thumb = self
            .terminal
            .read(cx)
            .frame()
            .and_then(|frame| thumb_bounds(&frame.scrollbar, bounds));
        if let Some(thumb) =
            thumb.filter(|_| button == mouse::Button::Left && event.position.x >= bounds.right())
        {
            // A click on the track jumps there, holding the thumb by its middle.
            let y = event.position.y;
            let grab = if (thumb.top()..thumb.bottom()).contains(&y) {
                y - thumb.top()
            } else {
                thumb.size.height / 2.
            };
            self.thumb_grab = Some(grab);
            self.drag_thumb(y, cx);
        } else {
            if !self.pressed.contains(&button) {
                self.pressed.push(button);
            }
            self.mouse(
                mouse::Action::Press,
                Some(button),
                event.position,
                &event.modifiers,
                cx,
            );
        }
    }

    /// Hovering, for CLIs that track every motion.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pressed.is_empty() && self.thumb_grab.is_none() {
            self.mouse(
                mouse::Action::Motion,
                None,
                event.position,
                &event.modifiers,
                cx,
            );
        }
    }

    /// Drags go on past the terminal's bounds, so these come from the window.
    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button.is_none() {
            // The release went elsewhere, e.g. while another workspace was shown.
            self.thumb_grab = None;
            for button in std::mem::take(&mut self.pressed) {
                self.mouse(
                    mouse::Action::Release,
                    Some(button),
                    event.position,
                    &event.modifiers,
                    cx,
                );
            }
        } else if self.thumb_grab.is_some() {
            self.drag_thumb(event.position.y, cx);
        } else if !self.pressed.is_empty() {
            let button = event.pressed_button.and_then(mouse_button);
            self.mouse(
                mouse::Action::Motion,
                button,
                event.position,
                &event.modifiers,
                cx,
            );
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.thumb_grab = None;
        }
        let button = mouse_button(event.button);
        if let Some(i) = self.pressed.iter().position(|&held| Some(held) == button) {
            self.pressed.remove(i);
            self.mouse(
                mouse::Action::Release,
                button,
                event.position,
                &event.modifiers,
                cx,
            );
        }
    }

    fn mouse(
        &self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        position: Point<Pixels>,
        modifiers: &Modifiers,
        cx: &App,
    ) {
        let position = position - self.bounds.get().origin;
        self.terminal.read(cx).mouse(MouseInput {
            action,
            button,
            mods: key_mods(modifiers),
            x: position.x.into(),
            y: position.y.into(),
        });
    }

    fn drag_thumb(&self, y: Pixels, cx: &App) {
        let (Some(grab), Some(frame)) = (self.thumb_grab, self.terminal.read(cx).frame()) else {
            return;
        };
        let bounds = self.bounds.get();
        let Some(thumb) = thumb_bounds(&frame.scrollbar, bounds) else {
            return;
        };
        let travel = (bounds.size.height - thumb.size.height).max(px(1.));
        let fraction = ((y - grab - bounds.top()) / travel).clamp(0., 1.);
        let rows = frame.scrollbar.total - frame.scrollbar.len;
        self.terminal
            .read(cx)
            .scroll_to((fraction * rows as f32).round() as usize);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal.read(cx).copy();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.terminal.read(cx).paste(text);
        }
    }

    /// Pastes the paths of files dropped from outside the app, escaped as
    /// terminals do, so the CLIs attach the images among them as they would
    /// from a paste. Each is followed by a space, so the next drop is a
    /// separate argument.
    fn drop_paths(&mut self, dropped: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let text: String = dropped
            .paths()
            .iter()
            .filter_map(|path| shell_escape(path))
            .map(|path| path + " ")
            .collect();
        if !text.is_empty() {
            self.terminal.read(cx).paste(text);
            self.focus_handle.focus(window, cx);
        }
    }

    fn marked_text_range_utf16(&self) -> Option<Range<usize>> {
        let text = self.marked_text.as_ref()?;
        Some(0..text.encode_utf16().count())
    }
}

/// Input methods commit text through this rather than as key presses. Keys
/// that the key handler turns into input never reach it.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked_text_range_utf16().map_or(0, |range| range.end);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text_range_utf16()
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.terminal.read(cx).text(text.to_string());
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    /// The cursor's cell, where the input method shows its candidates.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let frame = self.terminal.read(cx).frame()?;
        let cursor = frame.cursor.as_ref()?;
        let cell = cell_size(window, cx);
        let origin = self.bounds.get().origin
            + point(
                cell.width * cursor.col as f32,
                cell.height * cursor.row as f32,
            );
        Some(Bounds::new(origin, cell))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let terminal = self.terminal.clone();
        let frame = terminal.read(cx).frame();
        let focused = self.focus_handle.is_focused(window);
        let background = frame
            .as_ref()
            .map_or(cx.theme().background, |frame| color(frame.background));
        let bounds = self.bounds.clone();
        let view = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let marked_text = self.marked_text.clone();
        let drop_color = appearance::palette(cx).accent_soft;

        div()
            .id("terminal")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .px_3()
            .py_2()
            .bg(background)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_color))
            .on_drop(cx.listener(Self::drop_paths))
            .child(
                canvas(
                    move |grid_bounds, window, cx| {
                        let cell = cell_size(window, cx);
                        terminal.update(cx, |terminal, _| {
                            terminal.resize(GridSize {
                                cols: (grid_bounds.size.width / cell.width).max(1.) as u16,
                                rows: (grid_bounds.size.height / cell.height).max(1.) as u16,
                                cell_width: cell.width.into(),
                                cell_height: cell.height.into(),
                            })
                        });
                        bounds.set(grid_bounds);
                        cell
                    },
                    move |grid_bounds, cell, window, cx| {
                        window.on_mouse_event({
                            let view = view.clone();
                            move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    view.update(cx, |this, cx| this.drag(event, cx));
                                }
                            }
                        });
                        window.on_mouse_event({
                            let view = view.clone();
                            move |event: &MouseUpEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    view.update(cx, |this, cx| this.mouse_up(event, cx));
                                }
                            }
                        });
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(grid_bounds, view),
                            cx,
                        );
                        if let Some(frame) = frame {
                            paint_frame(&frame, grid_bounds.origin, cell, focused, window, cx);
                            if let Some(thumb) = thumb_bounds(&frame.scrollbar, grid_bounds) {
                                window.paint_quad(
                                    fill(thumb, cx.theme().scrollbar_thumb)
                                        .corner_radii(SCROLLBAR_WIDTH / 2.),
                                );
                            }
                            if let Some((text, cursor)) = marked_text.zip(frame.cursor.as_ref()) {
                                paint_marked_text(
                                    text,
                                    &frame,
                                    (cursor.col, cursor.row),
                                    grid_bounds.origin,
                                    cell,
                                    window,
                                    cx,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui_kit::{Keystroke, Modifiers};
    use libghostty_vt::key::{self, Key};

    use super::{key_code, key_input, shell_escape};

    #[test]
    fn escapes_dropped_paths_for_a_shell() {
        assert_eq!(
            shell_escape(Path::new("/tmp/shot.png")).as_deref(),
            Some("/tmp/shot.png")
        );
        assert_eq!(
            shell_escape(Path::new("/home/me/My Shots/it's (1).png")).as_deref(),
            Some(r"/home/me/My\ Shots/it\'s\ \(1\).png")
        );
    }

    #[test]
    fn refuses_dropped_paths_with_control_characters() {
        assert_eq!(shell_escape(Path::new("/tmp/shot\npwd\nimage.png")), None);
        assert_eq!(shell_escape(Path::new("/tmp/a\tb.png")), None);
    }

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn maps_key_names() {
        assert_eq!(key_code("a"), Some((Key::A, 'a')));
        assert_eq!(key_code("7"), Some((Key::Digit7, '7')));
        assert_eq!(key_code("enter"), Some((Key::Enter, '\0')));
        assert_eq!(key_code("f12"), Some((Key::F12, '\0')));
        assert_eq!(key_code("f13"), None);
        assert_eq!(key_code("shift"), None);
    }

    #[test]
    fn ctrl_combos_carry_no_text() {
        let input = key_input(&keystroke("c", Some("c"), Modifiers::control())).unwrap();
        assert_eq!(input.key, Key::C);
        assert_eq!(input.mods, key::Mods::CTRL);
        assert_eq!(input.text, None);
    }

    #[test]
    fn shifted_text_consumes_shift() {
        let input = key_input(&keystroke("a", Some("A"), Modifiers::shift())).unwrap();
        assert_eq!(input.text.as_deref(), Some("A"));
        assert_eq!(input.consumed_mods, key::Mods::SHIFT);
    }

    fn encode(keystroke: &Keystroke) -> Vec<u8> {
        let input = key_input(keystroke).unwrap();
        let mut event = key::Event::new().unwrap();
        event
            .set_action(key::Action::Press)
            .set_key(input.key)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed_mods)
            .set_unshifted_codepoint(input.unshifted)
            .set_utf8(input.text);
        let mut bytes = Vec::new();
        key::Encoder::new()
            .unwrap()
            .encode_to_vec(&event, &mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn ctrl_with_shifted_symbols_reaches_the_cli() {
        // GPUI on Linux reports Ctrl+Shift+- as `_` with only Ctrl held.
        assert_eq!(encode(&keystroke("_", None, Modifiers::control())), [0x1f]);
        // Ctrl+Shift+2 is the fixterms special case, sent as CSI u like Ghostty does.
        assert_eq!(
            encode(&keystroke("@", None, Modifiers::control())),
            b"\x1b[64;5u"
        );
        assert_eq!(encode(&keystroke("_", Some("_"), Modifiers::none())), b"_");
    }

    #[test]
    fn modifier_presses_are_ignored() {
        assert!(key_input(&keystroke("shift", None, Modifiers::shift())).is_none());
    }
}
