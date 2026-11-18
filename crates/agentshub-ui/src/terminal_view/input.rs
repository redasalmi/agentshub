//! Turning GPUI keys and pointers into the terminal's input.

use std::path::Path;

use gpui_kit::*;
use libghostty_vt::key::{self, Key};
use libghostty_vt::mouse;

use crate::terminal::KeyInput;

pub(super) fn key_mods(modifiers: &Modifiers) -> key::Mods {
    let mut mods = key::Mods::empty();
    mods.set(key::Mods::SHIFT, modifiers.shift);
    mods.set(key::Mods::CTRL, modifiers.control);
    mods.set(key::Mods::ALT, modifiers.alt);
    mods.set(key::Mods::SUPER, modifiers.platform);
    mods
}

pub(super) fn mouse_button(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Middle => Some(mouse::Button::Middle),
        MouseButton::Navigate(_) => None,
    }
}

pub(super) fn key_input(keystroke: &Keystroke) -> Option<KeyInput> {
    let modifiers = &keystroke.modifiers;
    let mut mods = key_mods(modifiers);

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
pub(super) fn key_code(name: &str) -> Option<(Key, char)> {
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

/// The characters a shell reads specially, which a backslash goes before in
/// dropped paths.
const SHELL_SPECIAL: &str = "\\ \"'`$!#&()*;<>?[]{}|~";

/// `None` for a path with a control character in it: a backslash can't keep a
/// newline from ending the command line, so pasting it could run commands.
pub(super) fn shell_escape(path: &Path) -> Option<String> {
    let mut escaped = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_control() {
            return None;
        }
        if SHELL_SPECIAL.contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    Some(escaped)
}
