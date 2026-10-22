use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use libghostty_vt::key::{self, Key};
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::RgbColor;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::terminal::{Frame, GridSize, KeyInput, Run, Terminal};

const CONTEXT: &str = "Terminal";
const FONT_FAMILY: &str = "JetBrains Mono";
const FONT_FALLBACKS: [&str; 4] = ["DejaVu Sans Mono", "Noto Sans Mono", "Menlo", "Consolas"];
const FONT_SIZE: Pixels = px(14.);
const LINE_HEIGHT: f32 = 1.3;

actions!(terminal, [Paste]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // The app binds these globally (focus cycling, copy); while a
        // terminal is focused they belong to the CLI.
        KeyBinding::new("tab", NoAction, Some(CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(CONTEXT)),
        KeyBinding::new("ctrl-c", NoAction, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-v", Paste, Some(CONTEXT)),
    ]);
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

pub fn rgb_color(color: Hsla) -> RgbColor {
    let rgba = color.to_rgb();
    RgbColor {
        r: (rgba.r * 255.).round() as u8,
        g: (rgba.g * 255.).round() as u8,
        b: (rgba.b * 255.).round() as u8,
    }
}

pub struct TerminalView {
    terminal: Entity<Terminal>,
    focus_handle: FocusHandle,
    /// Where the grid was last painted, to turn mouse positions into cells.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Trackpads scroll by fractions of a line.
    pending_scroll: f32,
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

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = cell_size(window).height;
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

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.terminal.read(cx).paste(text);
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
        let cell = cell_size(window);
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
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_handle.focus(window, cx)),
            )
            .child(
                canvas(
                    move |grid_bounds, window, cx| {
                        let cell = cell_size(window);
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
                        window.handle_input(
                            &focus_handle,
                            ElementInputHandler::new(grid_bounds, view),
                            cx,
                        );
                        if let Some(frame) = frame {
                            paint_frame(&frame, grid_bounds.origin, cell, focused, window, cx);
                            if let Some((text, cursor)) = marked_text.zip(frame.cursor.as_ref()) {
                                let origin = grid_bounds.origin
                                    + point(
                                        cell.width * cursor.col as f32,
                                        cell.height * cursor.row as f32,
                                    );
                                paint_marked_text(text, &frame, origin, cell, window, cx);
                            }
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// Built once, since every run of every frame needs it.
static TERMINAL_FONT: LazyLock<Font> = LazyLock::new(|| Font {
    fallbacks: Some(FontFallbacks::from_fonts(
        FONT_FALLBACKS.iter().map(|name| name.to_string()).collect(),
    )),
    ..font(FONT_FAMILY)
});

fn terminal_font() -> Font {
    TERMINAL_FONT.clone()
}

fn cell_size(window: &Window) -> Size<Pixels> {
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&terminal_font());
    let width = text_system
        .advance(font_id, FONT_SIZE, 'm')
        .map_or(FONT_SIZE * 0.6, |advance| advance.width);
    size(width, (FONT_SIZE * LINE_HEIGHT).round())
}

/// The complete grapheme under the cursor's cell. A grapheme can span several
/// chars, and a combining mark takes up no cell of its own.
fn cursor_grapheme(run: &Run, col: u16) -> Option<&str> {
    if run.wide {
        Some(run.text.as_str())
    } else {
        run.text.graphemes(true).nth((col - run.col) as usize)
    }
}

fn paint_frame(
    frame: &Frame,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let cell_origin = |col: u16, row: usize| {
        point(
            origin.x + cell.width * col as f32,
            origin.y + cell.height * row as f32,
        )
    };

    for (row, runs) in frame.rows.iter().enumerate() {
        for run in runs {
            if let Some(bg) = run.bg {
                let size = size(cell.width * run.cells as f32, cell.height);
                window.paint_quad(fill(
                    Bounds::new(cell_origin(run.col, row), size),
                    color(bg),
                ));
            }
        }
    }

    // An unfocused block cursor is drawn hollow.
    let solid_cursor = frame
        .cursor
        .as_ref()
        .filter(|cursor| focused && cursor.style == CursorVisualStyle::Block);
    if let Some(cursor) = &frame.cursor {
        let origin = cell_origin(cursor.col, cursor.row as usize);
        let cursor_color = color(cursor.color);
        let quad = match cursor.style {
            CursorVisualStyle::Bar => {
                fill(Bounds::new(origin, size(px(2.), cell.height)), cursor_color)
            }
            CursorVisualStyle::Underline => fill(
                Bounds::new(
                    point(origin.x, origin.y + cell.height - px(2.)),
                    size(cell.width, px(2.)),
                ),
                cursor_color,
            ),
            _ if solid_cursor.is_some() => fill(Bounds::new(origin, cell), cursor_color),
            _ => outline(Bounds::new(origin, cell), cursor_color, BorderStyle::Solid),
        };
        window.paint_quad(quad);
    }

    for (row, runs) in frame.rows.iter().enumerate() {
        for run in runs.iter().filter(|run| run.paints_text()) {
            paint_text(
                run.text.clone(),
                run,
                cell_origin(run.col, row),
                cell,
                None,
                window,
                cx,
            );

            // A solid cursor covers its cell, so that character is drawn
            // again on top of it in the background color.
            let under_cursor = solid_cursor.filter(|cursor| {
                cursor.row as usize == row && (run.col..run.col + run.cells).contains(&cursor.col)
            });
            if let Some(cursor) = under_cursor
                && let Some(text) = cursor_grapheme(run, cursor.col)
            {
                let fg = Some(color(run.bg.unwrap_or(frame.background)));
                let origin = cell_origin(if run.wide { run.col } else { cursor.col }, row);
                paint_text(text.to_string().into(), run, origin, cell, fg, window, cx);
            }
        }
    }
}

fn paint_marked_text(
    text: String,
    frame: &Frame,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let text_color = cx.theme().foreground;
    let text_run = TextRun {
        len: text.len(),
        font: terminal_font(),
        color: text_color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(text_color),
            wavy: false,
        }),
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.into(), FONT_SIZE, &[text_run], None);
    window.paint_quad(fill(
        Bounds::new(origin, size(line.width(), cell.height)),
        color(frame.background),
    ));
    line.paint(origin, cell.height, TextAlign::Left, None, window, cx)
        .ok();
}

fn paint_text(
    text: SharedString,
    run: &Run,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    fg: Option<Hsla>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut text_color = fg.unwrap_or_else(|| color(run.fg));
    if run.attrs.faint {
        text_color = text_color.opacity(0.6);
    }
    let mut font = terminal_font();
    if run.attrs.bold {
        font = font.bold();
    }
    if run.attrs.italic {
        font = font.italic();
    }
    let text_run = TextRun {
        len: text.len(),
        font,
        color: text_color,
        background_color: None,
        underline: run.attrs.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(text_color),
            wavy: false,
        }),
        strikethrough: run.attrs.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(text_color),
        }),
    };
    // Forcing every glyph to the cell width keeps text on the grid even when
    // a fallback font with different metrics renders some characters.
    let force_width = (!run.wide).then_some(cell.width);
    let line = window
        .text_system()
        .shape_line(text, FONT_SIZE, &[text_run], force_width);
    line.paint(origin, cell.height, TextAlign::Left, None, window, cx)
        .ok();
}

fn key_input(keystroke: &Keystroke) -> Option<KeyInput> {
    let modifiers = &keystroke.modifiers;
    let mut mods = key::Mods::empty();
    mods.set(key::Mods::SHIFT, modifiers.shift);
    mods.set(key::Mods::CTRL, modifiers.control);
    mods.set(key::Mods::ALT, modifiers.alt);
    mods.set(key::Mods::SUPER, modifiers.platform);

    // Ctrl and Super combos produce no text; the encoder builds their sequence.
    let mut text = keystroke
        .key_char
        .clone()
        .filter(|_| !modifiers.control && !modifiers.platform);
    let (key, unshifted) = match shifted_key_code(&keystroke.key) {
        // On Linux, GPUI names shifted punctuation by the symbol it types and
        // drops Shift. The encoder builds Ctrl combos like Ctrl+_ from that
        // symbol, so it is passed on as their text. Without Ctrl there is no
        // committed text; a missing key_char means an input method owns the key.
        Some(code) => {
            mods.insert(key::Mods::SHIFT);
            if modifiers.control && !modifiers.platform {
                text = Some(keystroke.key.clone());
            }
            code
        }
        None => key_code(&keystroke.key).unwrap_or((Key::Unidentified, '\0')),
    };
    // On Linux, XKB Compose reports the printable keys it consumes as ordinary
    // presses with no committed text. Encoding them would type them ahead of
    // the composed text the input method sends, so they are dropped. Combos
    // with Ctrl, Alt or Super are built from the key itself and are kept.
    if text.is_none()
        && unshifted != '\0'
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.platform
    {
        return None;
    }
    if key == Key::Unidentified && text.is_none() {
        return None;
    }
    let consumed_mods = if text.is_some() && mods.contains(key::Mods::SHIFT) {
        key::Mods::SHIFT
    } else {
        key::Mods::empty()
    };

    Some(KeyInput {
        key,
        mods,
        consumed_mods,
        text,
        unshifted,
    })
}

/// Maps GPUI key names to physical keys and the character they type unshifted.
fn key_code(name: &str) -> Option<(Key, char)> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ];
    const FUNCTION_KEYS: [Key; 12] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];

    let mut chars = name.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let key = match ch {
            'a'..='z' => LETTERS[ch as usize - 'a' as usize],
            '0'..='9' => DIGITS[ch as usize - '0' as usize],
            '`' => Key::Backquote,
            '-' => Key::Minus,
            '=' => Key::Equal,
            '[' => Key::BracketLeft,
            ']' => Key::BracketRight,
            '\\' => Key::Backslash,
            ';' => Key::Semicolon,
            '\'' => Key::Quote,
            ',' => Key::Comma,
            '.' => Key::Period,
            '/' => Key::Slash,
            _ => return None,
        };
        return Some((key, ch));
    }

    let key = match name {
        "space" => return Some((Key::Space, ' ')),
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        _ => {
            let number = name.strip_prefix('f')?.parse::<usize>().ok()?;
            *FUNCTION_KEYS.get(number.checked_sub(1)?)?
        }
    };
    Some((key, '\0'))
}

/// Maps the symbols that Shift types on a US layout to their physical key and
/// the character it types unshifted.
fn shifted_key_code(name: &str) -> Option<(Key, char)> {
    let unshifted = match name {
        "~" => '`',
        "!" => '1',
        "@" => '2',
        "#" => '3',
        "$" => '4',
        "%" => '5',
        "^" => '6',
        "&" => '7',
        "*" => '8',
        "(" => '9',
        ")" => '0',
        "_" => '-',
        "+" => '=',
        "{" => '[',
        "}" => ']',
        "|" => '\\',
        ":" => ';',
        "\"" => '\'',
        "<" => ',',
        ">" => '.',
        "?" => '/',
        _ => return None,
    };
    key_code(&unshifted.to_string())
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Keystroke, Modifiers};
    use libghostty_vt::key::{self, Key};

    use super::{key_code, key_input};

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
