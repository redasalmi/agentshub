//! The symbols Ghostty fits to their cells, and how many cells it lets each
//! cover: two when the next one is blank, as its `constraintWidth` decides.

/// Private use icons, and the arrows, dingbats, enclosed alphanumerics,
/// miscellaneous symbols, pictographs, emoticons and transport symbols.
pub(super) fn is_symbol(ch: char) -> bool {
    matches!(
        ch as u32,
        0xE000..=0xF8FF
            | 0xF0000..=0xFFFFD
            | 0x100000..=0x10FFFD
            | 0x2190..=0x21FF
            | 0x2460..=0x24FF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
            | 0x1F100..=0x1F1FF
            | 0x1F300..=0x1F5FF
            | 0x1F600..=0x1F64F
            | 0x1F680..=0x1F6FF
    )
}

/// Box drawing, blocks, legacy computing and Powerline glyphs, which a
/// symbol after them may still spread from.
fn is_graphics(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x259F | 0x1FB00..=0x1FBFF | 0xE0B0..=0xE0D7
    )
}

/// How many cells the narrow symbol at `col` may cover. `chars` holds each
/// cell's first character, a space where nothing is drawn.
pub(super) fn constraint_width(chars: &[char], col: u16) -> u16 {
    let col = col as usize;
    if col + 1 >= chars.len() {
        return 1;
    }
    // So a row of icons stays aligned.
    if let Some(&prev) = col.checked_sub(1).and_then(|prev| chars.get(prev))
        && is_symbol(prev)
        && !is_graphics(prev)
    {
        return 1;
    }
    if matches!(chars[col + 1], ' ' | '\u{2002}') {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::{constraint_width, is_symbol};

    fn chars(row: &str) -> Vec<char> {
        row.chars().collect()
    }

    #[test]
    fn spreads_a_symbol_only_into_a_blank_cell() {
        assert_eq!(constraint_width(&chars("A✔B"), 1), 1);
        assert_eq!(constraint_width(&chars("[✔]done"), 1), 1);
        assert_eq!(constraint_width(&chars("✔ atlassian"), 0), 2);
        assert_eq!(constraint_width(&chars("A✔"), 1), 1);
    }

    #[test]
    fn keeps_runs_of_icons_to_a_cell_each() {
        assert_eq!(constraint_width(&chars("★✔ "), 1), 1);
        // Graphics before a symbol don't count.
        assert_eq!(constraint_width(&chars("│✔ "), 1), 2);
    }

    #[test]
    fn tells_symbols_from_text() {
        for ch in ['✔', '★', '→', '①', '\u{E0A0}', '🎉', '🚀'] {
            assert!(is_symbol(ch), "{ch:?}");
        }
        for ch in ['a', '⏺', '│', '█', '●'] {
            assert!(!is_symbol(ch), "{ch:?}");
        }
    }
}
