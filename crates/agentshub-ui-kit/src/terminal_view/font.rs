//! The terminals' fonts: JetBrains Mono, then the fonts Ghostty falls back to
//! on a GNOME desktop for what it lacks, as far as GPUI can follow.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

const FAMILY: &str = "JetBrains Mono";

const NOTO_EMOJI: &[u8] = include_bytes!("../../../../assets/fonts/NotoEmoji-Regular.ttf");
const DEJAVU_SANS_MONO: &[u8] = include_bytes!("../../../../assets/fonts/DejaVuSansMono.ttf");

/// Noto Emoji's monochrome emoji for the symbols CLIs draw as text, like ✔
/// and ⏺, then the monospace fonts with the most symbols. Noto Emoji and
/// DejaVu Sans Mono ship with the app.
const FALLBACKS: [&str; 6] = [
    "Noto Emoji",
    "Adwaita Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Menlo",
    "Consolas",
];

/// Double-width emoji are left to the system's color emoji, when GPUI can
/// draw them.
static TEXT: LazyLock<Font> = LazyLock::new(|| with_fallbacks(&FALLBACKS));
static COLOR_EMOJI: LazyLock<Font> = LazyLock::new(|| with_fallbacks(&FALLBACKS[1..]));

static COLOR_EMOJI_DRAWABLE: AtomicBool = AtomicBool::new(false);

/// GPUI draws the faces named NotoColorEmoji in color, from bitmaps (CBDT,
/// sbix) or COLRv0 layers. Distributions like Fedora ship it as COLRv1 only,
/// which it draws as nothing.
const NOTO_COLOR_EMOJI: &str = "NotoColorEmoji";

fn with_fallbacks(fallbacks: &[&str]) -> Font {
    Font {
        fallbacks: Some(FontFallbacks::from_fonts(
            fallbacks.iter().map(|name| name.to_string()).collect(),
        )),
        ..font(FAMILY)
    }
}

pub(super) fn terminal_font() -> Font {
    TEXT.clone()
}

/// For a double-width character, which is an emoji when it isn't CJK.
pub(super) fn wide_font() -> Font {
    if COLOR_EMOJI_DRAWABLE.load(Ordering::Relaxed) {
        COLOR_EMOJI.clone()
    } else {
        TEXT.clone()
    }
}

pub(super) fn init(cx: &mut App) {
    let noto_emoji = with_latin_m(NOTO_EMOJI).expect("Noto Emoji has a format 12 cmap");
    cx.text_system()
        .add_fonts(vec![
            Cow::Owned(noto_emoji),
            Cow::Borrowed(DEJAVU_SANS_MONO),
        ])
        .expect("bundled fonts are valid");
    // Off the main thread, since it reads every installed font.
    let check = cx.background_spawn(async { color_emoji_drawable() });
    cx.spawn(async move |cx| {
        if check.await {
            COLOR_EMOJI_DRAWABLE.store(true, Ordering::Relaxed);
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

fn color_emoji_drawable() -> bool {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let faces: Vec<_> = db
        .faces()
        .filter(|face| face.post_script_name == NOTO_COLOR_EMOJI)
        .map(|face| face.id)
        .collect();
    // Every one, since nothing says which GPUI falls back to.
    !faces.is_empty()
        && faces
            .into_iter()
            .all(|id| db.with_face_data(id, draws_in_color).unwrap_or(false))
}

fn draws_in_color(data: &[u8], index: u32) -> bool {
    let font = font_offset(data, index);
    let has = |tag| font.and_then(|font| table(data, font, tag)).is_some();
    has(b"CBDT")
        || has(b"sbix")
        || font
            .and_then(|font| table(data, font, b"COLR"))
            .is_some_and(|colr| colr.starts_with(&[0, 0]))
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Where the font at `index` starts in an OpenType file or collection.
fn font_offset(data: &[u8], index: u32) -> Option<usize> {
    if data.get(..4)? == b"ttcf" {
        Some(be32(data, 12 + 4 * index as usize)? as usize)
    } else {
        Some(0)
    }
}

/// Where a table's record is in the font's table directory.
fn table_record(data: &[u8], font: usize, tag: &[u8; 4]) -> Option<usize> {
    (0..be16(data, font + 4)? as usize)
        .map(|ix| font + 12 + 16 * ix)
        .find(|record| data.get(*record..*record + 4) == Some(tag))
}

fn table<'a>(data: &'a [u8], font: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let record = table_record(data, font, tag)?;
    let offset = be32(data, record + 8)? as usize;
    data.get(offset..offset + be32(data, record + 12)? as usize)
}

/// A cmap's format 12 groups of characters, each `[first, last, glyph]`, and
/// its format 14 variation sequences.
struct Cmap<'a> {
    groups: Vec<[u32; 3]>,
    variations: Option<&'a [u8]>,
}

fn cmap_subtables(cmap: &[u8]) -> Option<Cmap<'_>> {
    let mut groups = None;
    let mut variations = None;
    for ix in 0..be16(cmap, 2)? as usize {
        let record = 4 + 8 * ix;
        let encoding = (be16(cmap, record)?, be16(cmap, record + 2)?);
        let subtable = cmap.get(be32(cmap, record + 4)? as usize..)?;
        match (encoding, be16(subtable, 0)?) {
            ((0, 4) | (3, 10), 12) => {
                let groups_at = |ix: usize| -> Option<[u32; 3]> {
                    let at = 16 + 12 * ix;
                    Some([
                        be32(subtable, at)?,
                        be32(subtable, at + 4)?,
                        be32(subtable, at + 8)?,
                    ])
                };
                groups = (0..be32(subtable, 12)? as usize)
                    .map(groups_at)
                    .collect::<Option<Vec<_>>>();
            }
            ((0, 5), 14) => variations = subtable.get(..be32(subtable, 2)? as usize),
            _ => {}
        }
    }
    Some(Cmap {
        groups: groups?,
        variations,
    })
}

fn glyph(groups: &[[u32; 3]], ch: char) -> Option<u32> {
    let ch = ch as u32;
    groups
        .iter()
        .find(|[first, last, _]| (*first..=*last).contains(&ch))
        .map(|[first, _, glyph]| glyph + ch - first)
}

/// The font with `m` drawn as its space. GPUI won't fall back to a
/// font without an `m`, which emoji fonts don't have, and the
/// terminal's own font always draws the `m` itself. The new cmap holds
/// the old format 12 mappings, that one and the variation sequences.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the tables it writes are far below the format's u16 and u32 limits"
)]
fn with_latin_m(font: &[u8]) -> Option<Vec<u8>> {
    let record = table_record(font, 0, b"cmap")?;
    let Cmap {
        mut groups,
        variations,
    } = cmap_subtables(table(font, 0, b"cmap")?)?;
    if glyph(&groups, 'm').is_none() {
        let space = glyph(&groups, ' ')?;
        let m = 'm' as u32;
        let ix = groups.partition_point(|[first, ..]| *first < m);
        groups.insert(ix, [m, m, space]);
    }

    let mut mappings = Vec::new();
    mappings.extend(12u16.to_be_bytes());
    mappings.extend(0u16.to_be_bytes());
    mappings.extend((16 + 12 * groups.len() as u32).to_be_bytes());
    mappings.extend(0u32.to_be_bytes());
    mappings.extend((groups.len() as u32).to_be_bytes());
    for group in &groups {
        for value in group {
            mappings.extend(value.to_be_bytes());
        }
    }

    let records: &[(u16, u16)] = if variations.is_some() {
        &[(0, 4), (0, 5), (3, 10)]
    } else {
        &[(0, 4), (3, 10)]
    };
    let mappings_at = 4 + 8 * records.len() as u32;
    let variations_at = mappings_at + mappings.len() as u32;
    let mut cmap = Vec::new();
    cmap.extend(0u16.to_be_bytes());
    cmap.extend((records.len() as u16).to_be_bytes());
    for &(platform, encoding) in records {
        cmap.extend(platform.to_be_bytes());
        cmap.extend(encoding.to_be_bytes());
        let at = if encoding == 5 {
            variations_at
        } else {
            mappings_at
        };
        cmap.extend(at.to_be_bytes());
    }
    cmap.extend(mappings);
    cmap.extend(variations.unwrap_or_default());

    // The new table goes at the end, and the old one is left unused.
    let mut patched = font.to_vec();
    patched.resize(patched.len().next_multiple_of(4), 0);
    let offset = patched.len() as u32;
    patched.extend(&cmap);
    patched[record + 8..record + 12].copy_from_slice(&offset.to_be_bytes());
    patched[record + 12..record + 16].copy_from_slice(&(cmap.len() as u32).to_be_bytes());
    Some(patched)
}

#[cfg(test)]
mod tests {
    use super::{NOTO_EMOJI, cmap_subtables, draws_in_color, glyph, table, with_latin_m};

    /// An OpenType file with these tables and nothing else.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the tables it writes are far below the format's u16 and u32 limits"
    )]
    fn font(tables: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut data = vec![0, 1, 0, 0];
        data.extend((tables.len() as u16).to_be_bytes());
        data.extend([0; 6]);
        let mut offset = 12 + 16 * tables.len();
        for (tag, table) in tables {
            data.extend(*tag);
            data.extend([0; 4]);
            data.extend((offset as u32).to_be_bytes());
            data.extend((table.len() as u32).to_be_bytes());
            offset += table.len();
        }
        for (_, table) in tables {
            data.extend(*table);
        }
        data
    }

    #[test]
    fn tells_which_color_emoji_can_be_drawn() {
        assert!(draws_in_color(&font(&[(b"CBDT", &[0, 3])]), 0));
        assert!(draws_in_color(
            &font(&[(b"glyf", &[]), (b"COLR", &[0, 0, 0, 4])]),
            0
        ));
        assert!(!draws_in_color(
            &font(&[(b"glyf", &[]), (b"COLR", &[0, 1, 0, 0])]),
            0
        ));
        assert!(!draws_in_color(&font(&[(b"glyf", &[1, 2])]), 0));
        assert!(!draws_in_color(b"junk", 0));
    }

    #[test]
    fn gives_noto_emoji_an_m_and_keeps_its_glyphs() {
        let before = cmap_subtables(table(NOTO_EMOJI, 0, b"cmap").unwrap()).unwrap();
        assert_eq!(glyph(&before.groups, 'm'), None);

        let patched = with_latin_m(NOTO_EMOJI).unwrap();
        let after = cmap_subtables(table(&patched, 0, b"cmap").unwrap()).unwrap();
        assert_eq!(glyph(&after.groups, 'm'), glyph(&before.groups, ' '));
        for ch in ['✔', '⏺', '🎉', ' '] {
            assert!(glyph(&before.groups, ch).is_some(), "{ch:?}");
            assert_eq!(
                glyph(&after.groups, ch),
                glyph(&before.groups, ch),
                "{ch:?}"
            );
        }
        assert!(before.variations.is_some());
        assert_eq!(after.variations, before.variations);
        for tag in [b"glyf", b"GSUB", b"hmtx", b"name"] {
            assert_eq!(table(&patched, 0, tag), table(NOTO_EMOJI, 0, tag));
        }
    }
}
