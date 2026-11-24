//! The characters Ghostty draws itself instead of taking from a font: box
//! drawing, block elements, braille, sextants and Powerline separators. Drawn
//! on the cell's own device pixels, as Ghostty's sprite font draws them, they
//! meet their neighbours without the seams and gaps that font glyphs leave at
//! fractional positions or a line height taller than the font's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::SQRT_2;
use std::rc::Rc;

use gpui_kit::*;

/// Whether `ch` is drawn here rather than by the font.
pub(super) fn covers(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x259F | 0x2800..=0x28FF | 0xE0B0..=0xE0BF | 0x1FB00..=0x1FB3B
    )
}

/// A cell's size and the thickness of a light line, in device pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct Metrics {
    pub width: i32,
    pub height: i32,
    pub thickness: i32,
}

pub(super) fn paint(
    ch: char,
    origin: Point<Pixels>,
    metrics: Metrics,
    scale: f32,
    color: Hsla,
    window: &mut Window,
) {
    let logical = |device: i32| px(device as f32 / scale);
    if let Some(rects) = cached_rects(ch, metrics) {
        for rect in rects.iter() {
            let bounds = Bounds::new(
                point(origin.x + logical(rect.x), origin.y + logical(rect.y)),
                size(logical(rect.width), logical(rect.height)),
            );
            window.paint_quad(fill(bounds, color.opacity(rect.alpha as f32 / 255.)));
        }
    } else if let Some(mut path) = path(ch, metrics, scale) {
        path.scale(1. / scale);
        path.translate(origin);
        let cell = Bounds::new(
            origin,
            size(logical(metrics.width), logical(metrics.height)),
        );
        if let Ok(path) = path.build() {
            window.with_content_mask(Some(ContentMask { bounds: cell }), |window| {
                window.paint_path(path, color)
            });
        }
    }
}

/// A filled rectangle of a sprite, in device pixels from the cell's corner.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    alpha: u8,
}

type Sprites = HashMap<(char, Metrics), Option<Rc<[Rect]>>>;

thread_local! {
    static RECTS: RefCell<Sprites> = RefCell::new(HashMap::new());
}

fn cached_rects(ch: char, metrics: Metrics) -> Option<Rc<[Rect]>> {
    RECTS.with_borrow_mut(|cache| {
        // Zooming leaves every earlier size's sprites behind.
        if cache.len() > 4096 {
            cache.clear();
        }
        cache
            .entry((ch, metrics))
            .or_insert_with(|| rects(ch, metrics))
            .clone()
    })
}

/// The pixels of a sprite, which overlapping strokes cover only once, so a
/// faint color isn't darker where they cross.
struct Canvas {
    width: i32,
    height: i32,
    alpha: Vec<u8>,
}

impl Canvas {
    fn new(metrics: Metrics) -> Self {
        Self {
            width: metrics.width,
            height: metrics.height,
            alpha: vec![0; (metrics.width * metrics.height).max(0) as usize],
        }
    }

    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        self.shade(x0, y0, x1, y1, 0xff);
    }

    fn shade(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        let (x0, x1) = (x0.clamp(0, self.width), x1.clamp(0, self.width));
        let (y0, y1) = (y0.clamp(0, self.height), y1.clamp(0, self.height));
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = &mut self.alpha[(y * self.width + x) as usize];
                *pixel = (*pixel).max(alpha);
            }
        }
    }

    /// The pixels as rectangles: each row's runs of one alpha, merged with
    /// the same run in the rows below.
    fn into_rects(self) -> Rc<[Rect]> {
        let mut done = Vec::new();
        let mut open: Vec<Rect> = Vec::new();
        for y in 0..self.height {
            let row = &self.alpha[(y * self.width) as usize..((y + 1) * self.width) as usize];
            let mut next = Vec::new();
            let mut x = 0;
            while x < self.width {
                let alpha = row[x as usize];
                let start = x;
                while x < self.width && row[x as usize] == alpha {
                    x += 1;
                }
                if alpha == 0 {
                    continue;
                }
                let width = x - start;
                match open
                    .iter()
                    .position(|rect| rect.x == start && rect.width == width && rect.alpha == alpha)
                {
                    Some(ix) => {
                        let mut rect = open.swap_remove(ix);
                        rect.height += 1;
                        next.push(rect);
                    }
                    None => next.push(Rect {
                        x: start,
                        y,
                        width,
                        height: 1,
                        alpha,
                    }),
                }
            }
            done.append(&mut open);
            open = next;
        }
        done.append(&mut open);
        done.into()
    }
}

fn rects(ch: char, metrics: Metrics) -> Option<Rc<[Rect]>> {
    let mut canvas = Canvas::new(metrics);
    let cp = ch as u32;
    match cp {
        0x2504..=0x250B | 0x254C..=0x254F => dashes(&mut canvas, cp, metrics),
        0x256D..=0x2573 => return None,
        0x2500..=0x257F => lines(&mut canvas, LINES[(cp - 0x2500) as usize], metrics),
        0x2580..=0x259F => blocks(&mut canvas, cp, metrics),
        0x2800..=0x28FF => braille(&mut canvas, cp as u8, metrics),
        0x1FB00..=0x1FB3B => sextants(&mut canvas, cp - 0x1FB00, metrics),
        _ => return None,
    }
    Some(canvas.into_rects())
}

/// Each box drawing line's arms, up, right, down and left: none, light, heavy
/// or double. Dashed lines, arcs and diagonals are drawn on their own.
#[rustfmt::skip]
const LINES: [&[u8; 4]; 128] = [
    b".l.l", b".h.h", b"l.l.", b"h.h.", b"....", b"....", b"....", b"....",
    b"....", b"....", b"....", b"....", b".ll.", b".hl.", b".lh.", b".hh.",
    b"..ll", b"..lh", b"..hl", b"..hh", b"ll..", b"lh..", b"hl..", b"hh..",
    b"l..l", b"l..h", b"h..l", b"h..h", b"lll.", b"lhl.", b"hll.", b"llh.",
    b"hlh.", b"hhl.", b"lhh.", b"hhh.", b"l.ll", b"l.lh", b"h.ll", b"l.hl",
    b"h.hl", b"h.lh", b"l.hh", b"h.hh", b".lll", b".llh", b".hll", b".hlh",
    b".lhl", b".lhh", b".hhl", b".hhh", b"ll.l", b"ll.h", b"lh.l", b"lh.h",
    b"hl.l", b"hl.h", b"hh.l", b"hh.h", b"llll", b"lllh", b"lhll", b"lhlh",
    b"hlll", b"llhl", b"hlhl", b"hllh", b"hhll", b"llhh", b"lhhl", b"hhlh",
    b"lhhh", b"hlhh", b"hhhl", b"hhhh", b"....", b"....", b"....", b"....",
    b".d.d", b"d.d.", b".dl.", b".ld.", b".dd.", b"..ld", b"..dl", b"..dd",
    b"ld..", b"dl..", b"dd..", b"l..d", b"d..l", b"d..d", b"ldl.", b"dld.",
    b"ddd.", b"l.ld", b"d.dl", b"d.dd", b".dld", b".ldl", b".ddd", b"ld.d",
    b"dl.l", b"dd.d", b"ldld", b"dldl", b"dddd", b"....", b"....", b"....",
    b"....", b"....", b"....", b"....", b"...l", b"l...", b".l..", b"..l.",
    b"...h", b"h...", b".h..", b"..h.", b".h.l", b"l.h.", b".l.h", b"h.l.",
];

/// Ghostty's `linesChar`: each arm runs from its edge to the centre, ending
/// where the arms across it need it to, so joints and corners close up.
fn lines(canvas: &mut Canvas, arms: &[u8; 4], metrics: Metrics) {
    let [up, right, down, left] = *arms;
    let Metrics {
        width,
        height,
        thickness: light,
    } = metrics;
    let heavy = light * 2;

    let h_light_top = (height - light).max(0) / 2;
    let h_light_bottom = h_light_top + light;
    let h_heavy_top = (height - heavy).max(0) / 2;
    let h_heavy_bottom = h_heavy_top + heavy;
    let h_double_top = (h_light_top - light).max(0);
    let h_double_bottom = h_light_bottom + light;

    let v_light_left = (width - light).max(0) / 2;
    let v_light_right = v_light_left + light;
    let v_heavy_left = (width - heavy).max(0) / 2;
    let v_heavy_right = v_heavy_left + heavy;
    let v_double_left = (v_light_left - light).max(0);
    let v_double_right = v_light_right + light;

    let up_bottom = if left == b'h' || right == b'h' {
        h_heavy_bottom
    } else if left != right || down == up {
        if left == b'd' || right == b'd' {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if left == b'.' && right == b'.' {
        h_light_bottom
    } else {
        h_light_top
    };
    let down_top = if left == b'h' || right == b'h' {
        h_heavy_top
    } else if left != right || up == down {
        if left == b'd' || right == b'd' {
            h_double_top
        } else {
            h_light_top
        }
    } else if left == b'.' && right == b'.' {
        h_light_top
    } else {
        h_light_bottom
    };
    let left_right = if up == b'h' || down == b'h' {
        v_heavy_right
    } else if up != down || left == right {
        if up == b'd' || down == b'd' {
            v_double_right
        } else {
            v_light_right
        }
    } else if up == b'.' && down == b'.' {
        v_light_right
    } else {
        v_light_left
    };
    let right_left = if up == b'h' || down == b'h' {
        v_heavy_left
    } else if up != down || right == left {
        if up == b'd' || down == b'd' {
            v_double_left
        } else {
            v_light_left
        }
    } else if up == b'.' && down == b'.' {
        v_light_left
    } else {
        v_light_right
    };

    match up {
        b'l' => canvas.fill(v_light_left, 0, v_light_right, up_bottom),
        b'h' => canvas.fill(v_heavy_left, 0, v_heavy_right, up_bottom),
        b'd' => {
            let left_bottom = if left == b'd' { h_light_top } else { up_bottom };
            let right_bottom = if right == b'd' {
                h_light_top
            } else {
                up_bottom
            };
            canvas.fill(v_double_left, 0, v_light_left, left_bottom);
            canvas.fill(v_light_right, 0, v_double_right, right_bottom);
        }
        _ => {}
    }
    match right {
        b'l' => canvas.fill(right_left, h_light_top, width, h_light_bottom),
        b'h' => canvas.fill(right_left, h_heavy_top, width, h_heavy_bottom),
        b'd' => {
            let top_left = if up == b'd' {
                v_light_right
            } else {
                right_left
            };
            let bottom_left = if down == b'd' {
                v_light_right
            } else {
                right_left
            };
            canvas.fill(top_left, h_double_top, width, h_light_top);
            canvas.fill(bottom_left, h_light_bottom, width, h_double_bottom);
        }
        _ => {}
    }
    match down {
        b'l' => canvas.fill(v_light_left, down_top, v_light_right, height),
        b'h' => canvas.fill(v_heavy_left, down_top, v_heavy_right, height),
        b'd' => {
            let left_top = if left == b'd' {
                h_light_bottom
            } else {
                down_top
            };
            let right_top = if right == b'd' {
                h_light_bottom
            } else {
                down_top
            };
            canvas.fill(v_double_left, left_top, v_light_left, height);
            canvas.fill(v_light_right, right_top, v_double_right, height);
        }
        _ => {}
    }
    match left {
        b'l' => canvas.fill(0, h_light_top, left_right, h_light_bottom),
        b'h' => canvas.fill(0, h_heavy_top, left_right, h_heavy_bottom),
        b'd' => {
            let top_right = if up == b'd' { v_light_left } else { left_right };
            let bottom_right = if down == b'd' {
                v_light_left
            } else {
                left_right
            };
            canvas.fill(0, h_double_top, top_right, h_light_top);
            canvas.fill(0, h_light_bottom, bottom_right, h_double_bottom);
        }
        _ => {}
    }
}

/// Dashed lines, with half a gap at each end so they tile into an even line.
fn dashes(canvas: &mut Canvas, cp: u32, metrics: Metrics) {
    let light = metrics.thickness;
    let heavy = light * 2;
    let (count, thickness, gap, vertical) = match cp {
        0x2504 => (3, light, light.max(4), false),
        0x2505 => (3, heavy, light.max(4), false),
        0x2506 => (3, light, light.max(4), true),
        0x2507 => (3, heavy, light.max(4), true),
        0x2508 => (4, light, light.max(4), false),
        0x2509 => (4, heavy, light.max(4), false),
        0x250A => (4, light, light.max(4), true),
        0x250B => (4, heavy, light.max(4), true),
        0x254C => (2, light, light, false),
        0x254D => (2, heavy, heavy, false),
        0x254E => (2, light, heavy, true),
        _ => (2, heavy, heavy, true),
    };
    let (length, across) = if vertical {
        (metrics.height, metrics.width)
    } else {
        (metrics.width, metrics.height)
    };
    let offset = (across - thickness).max(0) / 2;
    if length < count * 2 {
        let light_offset = (across - light).max(0) / 2;
        if vertical {
            canvas.fill(light_offset, 0, light_offset + light, length);
        } else {
            canvas.fill(0, light_offset, length, light_offset + light);
        }
        return;
    }
    let mut segment = |start: i32, end: i32| {
        if vertical {
            canvas.fill(offset, start, offset + thickness, end);
        } else {
            canvas.fill(start, offset, end, offset + thickness);
        }
    };
    let gap = gap.min(length / (2 * count));
    let dashes = length - count * gap;
    let (dash, mut extra) = (dashes / count, dashes % count);
    // Ghostty starts vertical dashes at the top edge.
    let mut start = if vertical { 0 } else { gap / 2 };
    for _ in 0..count {
        let mut end = start + dash;
        if extra > 0 {
            extra -= 1;
            end += 1;
        }
        segment(start, end);
        start = end + gap;
    }
}

/// Where a fraction of a cell's side starts and ends, rounded as Ghostty
/// rounds it, so halves meet on odd sizes.
fn fraction_start(fraction: f32, side: i32) -> i32 {
    side - ((1. - fraction) * side as f32).round() as i32
}

fn fraction_end(fraction: f32, side: i32) -> i32 {
    (fraction * side as f32).round() as i32
}

fn blocks(canvas: &mut Canvas, cp: u32, metrics: Metrics) {
    let Metrics { width, height, .. } = metrics;
    let lower = |canvas: &mut Canvas, eighths: f32| {
        let h = (height as f32 * eighths / 8.).round() as i32;
        canvas.fill(0, height - h, width, height);
    };
    let upper = |canvas: &mut Canvas, eighths: f32| {
        let h = (height as f32 * eighths / 8.).round() as i32;
        canvas.fill(0, 0, width, h);
    };
    let left = |canvas: &mut Canvas, eighths: f32| {
        let w = (width as f32 * eighths / 8.).round() as i32;
        canvas.fill(0, 0, w, height);
    };
    let right = |canvas: &mut Canvas, eighths: f32| {
        let w = (width as f32 * eighths / 8.).round() as i32;
        canvas.fill(width - w, 0, width, height);
    };
    let quadrants = |canvas: &mut Canvas, [tl, tr, bl, br]: [bool; 4]| {
        let (x_start, x_end) = (fraction_start(0.5, width), fraction_end(0.5, width));
        let (y_start, y_end) = (fraction_start(0.5, height), fraction_end(0.5, height));
        if tl {
            canvas.fill(0, 0, x_end, y_end);
        }
        if tr {
            canvas.fill(x_start, 0, width, y_end);
        }
        if bl {
            canvas.fill(0, y_start, x_end, height);
        }
        if br {
            canvas.fill(x_start, y_start, width, height);
        }
    };
    match cp {
        0x2580 => upper(canvas, 4.),
        0x2581..=0x2587 => lower(canvas, (cp - 0x2580) as f32),
        0x2588 => canvas.fill(0, 0, width, height),
        0x2589..=0x258F => left(canvas, (0x2590 - cp) as f32),
        0x2590 => right(canvas, 4.),
        0x2591 => canvas.shade(0, 0, width, height, 0x40),
        0x2592 => canvas.shade(0, 0, width, height, 0x80),
        0x2593 => canvas.shade(0, 0, width, height, 0xc0),
        0x2594 => upper(canvas, 1.),
        0x2595 => right(canvas, 1.),
        0x2596 => quadrants(canvas, [false, false, true, false]),
        0x2597 => quadrants(canvas, [false, false, false, true]),
        0x2598 => quadrants(canvas, [true, false, false, false]),
        0x2599 => quadrants(canvas, [true, false, true, true]),
        0x259A => quadrants(canvas, [true, false, false, true]),
        0x259B => quadrants(canvas, [true, true, true, false]),
        0x259C => quadrants(canvas, [true, true, false, true]),
        0x259D => quadrants(canvas, [false, true, false, false]),
        0x259E => quadrants(canvas, [false, true, true, false]),
        _ => quadrants(canvas, [false, true, true, true]),
    }
}

/// Ghostty's braille: square dots, sized and spaced to spread evenly over
/// the cell.
fn braille(canvas: &mut Canvas, dots: u8, metrics: Metrics) {
    let Metrics { width, height, .. } = metrics;
    let mut w = (width / 4).min(height / 8);
    let mut x_spacing = width / 4;
    let mut y_spacing = height / 8;
    let mut x_margin = x_spacing / 2;
    let mut y_margin = y_spacing / 2;
    let mut x_left = width - 2 * x_margin - x_spacing - 2 * w;
    let mut y_left = height - 2 * y_margin - 3 * y_spacing - 4 * w;
    if x_left >= 2 && y_left >= 4 && w == 0 {
        w += 1;
        x_left -= 2;
        y_left -= 4;
    }
    if x_left >= 2 && x_margin == 0 {
        x_margin = 1;
        x_left -= 2;
    }
    if y_left >= 2 && y_margin == 0 {
        y_margin = 1;
        y_left -= 2;
    }
    if x_left >= 1 {
        x_spacing += 1;
        x_left -= 1;
    }
    if y_left >= 3 {
        y_spacing += 1;
        y_left -= 3;
    }
    if x_left >= 2 {
        x_margin += 1;
        x_left -= 2;
    }
    if y_left >= 2 {
        y_margin += 1;
        y_left -= 2;
    }
    if x_left >= 2 && y_left >= 4 {
        w += 1;
    }
    let x = [x_margin, x_margin + w + x_spacing];
    let y = [0, 1, 2, 3].map(|row| y_margin + row * (w + y_spacing));
    // Dots 1 to 8, as Unicode numbers them, by bit.
    let positions = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for (bit, (column, row)) in positions.into_iter().enumerate() {
        if dots & (1 << bit) != 0 {
            canvas.fill(x[column], y[row], x[column] + w, y[row] + w);
        }
    }
}

/// Two columns of three blocks. The 64 patterns skip the four that other
/// characters already are: empty, full and either half.
fn sextants(canvas: &mut Canvas, index: u32, metrics: Metrics) {
    let Metrics { width, height, .. } = metrics;
    let pattern = index + index / 0x14 + 1;
    let columns = [
        (0, fraction_end(0.5, width)),
        (fraction_start(0.5, width), width),
    ];
    let rows = [
        (0, fraction_end(1. / 3., height)),
        (
            fraction_start(1. / 3., height),
            fraction_end(2. / 3., height),
        ),
        (fraction_start(2. / 3., height), height),
    ];
    for bit in 0..6 {
        if pattern & (1 << bit) != 0 {
            let (x0, x1) = columns[bit % 2];
            let (y0, y1) = rows[bit / 2];
            canvas.fill(x0, y0, x1, y1);
        }
    }
}

/// The curved and slanted sprites, in device pixels, anti-aliased by the
/// path renderer and clipped to the cell.
fn path(ch: char, metrics: Metrics, scale: f32) -> Option<PathBuilder> {
    let (w, h) = (metrics.width as f32, metrics.height as f32);
    let light = metrics.thickness as f32;
    let at = |x: f32, y: f32| point(px(x), px(y));
    let stroke = || PathBuilder::stroke(px(light / scale));
    let triangle = |points: [(f32, f32); 3]| {
        let mut path = PathBuilder::fill();
        path.add_polygon(&points.map(|(x, y)| at(x, y)), true);
        path
    };
    let diagonal = |rising: bool| {
        let (sx, sy) = ((w / h).min(1.), (h / w).min(1.));
        let mut path = stroke();
        if rising {
            path.move_to(at(w + 0.5 * sx, -0.5 * sy));
            path.line_to(at(-0.5 * sx, h + 0.5 * sy));
        } else {
            path.move_to(at(-0.5 * sx, -0.5 * sy));
            path.line_to(at(w + 0.5 * sx, h + 0.5 * sy));
        }
        path
    };
    let mirror = |x: f32, flip: bool| if flip { w - x } else { x };
    Some(match ch as u32 {
        cp @ 0x256D..=0x2570 => {
            // Ghostty's arcs: a light line from each edge, turning through a
            // curve as wide as half the cell.
            let thickness = metrics.thickness;
            let cx = ((metrics.width - thickness).max(0) / 2) as f32 + light / 2.;
            let cy = ((metrics.height - thickness).max(0) / 2) as f32 + light / 2.;
            let r = w.min(h) / 2.;
            let s = 0.25;
            let (down, right) = match cp {
                0x256D => (1., 1.),
                0x256E => (1., -1.),
                0x256F => (-1., -1.),
                _ => (-1., 1.),
            };
            let mut path = stroke();
            path.move_to(at(cx, if down > 0. { h } else { 0. }));
            path.line_to(at(cx, cy + down * r));
            path.cubic_bezier_to(
                at(cx + right * r, cy),
                at(cx, cy + down * s * r),
                at(cx + right * s * r, cy),
            );
            path.line_to(at(if right > 0. { w } else { 0. }, cy));
            path
        }
        0x2571 | 0xE0BB | 0xE0BD => diagonal(true),
        0x2572 | 0xE0B9 | 0xE0BF => diagonal(false),
        0x2573 => {
            let mut path = diagonal(true);
            let (sx, sy) = ((w / h).min(1.), (h / w).min(1.));
            path.move_to(at(-0.5 * sx, -0.5 * sy));
            path.line_to(at(w + 0.5 * sx, h + 0.5 * sy));
            path
        }
        0xE0B0 => triangle([(0., 0.), (w, h / 2.), (0., h)]),
        0xE0B2 => triangle([(w, 0.), (0., h / 2.), (w, h)]),
        0xE0B8 => triangle([(0., 0.), (w, h), (0., h)]),
        0xE0BA => triangle([(w, 0.), (w, h), (0., h)]),
        0xE0BC => triangle([(0., 0.), (w, 0.), (0., h)]),
        0xE0BE => triangle([(0., 0.), (w, 0.), (w, h)]),
        cp @ (0xE0B1 | 0xE0B3) => {
            let flip = cp == 0xE0B3;
            let mut path = stroke();
            path.move_to(at(mirror(0., flip), 0.));
            path.line_to(at(mirror(w, flip), h / 2.));
            path.line_to(at(mirror(0., flip), h));
            path
        }
        cp @ (0xE0B4 | 0xE0B6) => {
            let flip = cp == 0xE0B6;
            let c = (SQRT_2 - 1.) * 4. / 3.;
            let r = w.min(h / 2.);
            let mut path = PathBuilder::fill();
            path.move_to(at(mirror(0., flip), 0.));
            path.cubic_bezier_to(
                at(mirror(r, flip), r),
                at(mirror(r * c, flip), 0.),
                at(mirror(r, flip), r - r * c),
            );
            path.line_to(at(mirror(r, flip), h - r));
            path.cubic_bezier_to(
                at(mirror(0., flip), h),
                at(mirror(r, flip), h - r + r * c),
                at(mirror(r * c, flip), h),
            );
            path.close();
            path
        }
        cp @ (0xE0B5 | 0xE0B7) => {
            // The outline of the half circle, inside its edge.
            let flip = cp == 0xE0B7;
            let c = (SQRT_2 - 1.) * 4. / 3.;
            let (top, bottom) = (light / 2., h - light / 2.);
            let r = (w - light / 2.).min((bottom - top) / 2.);
            let mut path = stroke();
            path.move_to(at(mirror(0., flip), top));
            path.cubic_bezier_to(
                at(mirror(r, flip), top + r),
                at(mirror(r * c, flip), top),
                at(mirror(r, flip), top + r - r * c),
            );
            path.line_to(at(mirror(r, flip), bottom - r));
            path.cubic_bezier_to(
                at(mirror(0., flip), bottom),
                at(mirror(r, flip), bottom - r + r * c),
                at(mirror(r * c, flip), bottom),
            );
            path
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Metrics, covers, path, rects};

    const CELL: Metrics = Metrics {
        width: 9,
        height: 18,
        thickness: 1,
    };

    /// The sprite's pixels, failing if any two of its rectangles overlap.
    fn pixels(ch: char, metrics: Metrics) -> Vec<Vec<u8>> {
        let mut grid = vec![vec![0u8; metrics.width as usize]; metrics.height as usize];
        for rect in rects(ch, metrics).unwrap().iter() {
            assert!(
                rect.x >= 0 && rect.x + rect.width <= metrics.width,
                "{ch:?} {rect:?}"
            );
            assert!(
                rect.y >= 0 && rect.y + rect.height <= metrics.height,
                "{ch:?} {rect:?}"
            );
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let pixel = &mut grid[y as usize][x as usize];
                    assert_eq!(*pixel, 0, "{ch:?} overlaps itself at {x},{y}");
                    *pixel = rect.alpha;
                }
            }
        }
        grid
    }

    fn lit(grid: &[Vec<u8>]) -> usize {
        grid.iter().flatten().filter(|alpha| **alpha > 0).count()
    }

    #[test]
    fn every_sprite_stays_in_its_cell_without_overlapping() {
        for metrics in [
            CELL,
            Metrics {
                width: 17,
                height: 37,
                thickness: 2,
            },
            Metrics {
                width: 3,
                height: 5,
                thickness: 1,
            },
        ] {
            for cp in (0x2500..=0x259F)
                .chain(0x2800..=0x28FF)
                .chain(0x1FB00..=0x1FB3B)
            {
                let ch = char::from_u32(cp).unwrap();
                if rects(ch, metrics).is_some() {
                    pixels(ch, metrics);
                }
            }
        }
    }

    #[test]
    fn lines_span_the_cell_to_join_their_neighbours() {
        let horizontal = pixels('─', CELL);
        assert_eq!(lit(&horizontal), 9);
        assert!(horizontal[8].iter().all(|alpha| *alpha == 0xff));
        assert_eq!(rects('─', CELL).unwrap().len(), 1);

        let vertical = pixels('│', CELL);
        assert_eq!(lit(&vertical), 18);
        assert!(vertical.iter().all(|row| row[4] == 0xff));

        let heavy = pixels('━', CELL);
        assert_eq!(lit(&heavy), 18);
    }

    #[test]
    fn double_corners_nest() {
        let corner = pixels('╔', CELL);
        // The outer stroke turns at the top left, the inner one below and
        // right of it, with nothing between them.
        assert_eq!(corner[7][3], 0xff);
        assert_eq!(corner[7][8], 0xff);
        assert_eq!(corner[17][3], 0xff);
        assert_eq!(corner[9][5], 0xff);
        assert_eq!(corner[9][8], 0xff);
        assert_eq!(corner[17][5], 0xff);
        assert_eq!(corner[8][4], 0);
        assert_eq!(corner[12][4], 0);
    }

    #[test]
    fn blocks_fill_their_share_of_the_cell() {
        assert_eq!(lit(&pixels('█', CELL)), 9 * 18);
        assert_eq!(rects('█', CELL).unwrap().len(), 1);
        assert_eq!(lit(&pixels('▀', CELL)), 9 * 9);
        assert_eq!(lit(&pixels('▄', CELL)), 9 * 9);
        assert_eq!(lit(&pixels('▌', CELL)), 5 * 18);
        assert_eq!(lit(&pixels('▐', CELL)), 5 * 18);
        // Three quadrants, the halves overlapping by the odd column.
        let quadrants = pixels('▛', CELL);
        assert_eq!(quadrants[0][8], 0xff);
        assert_eq!(quadrants[17][0], 0xff);
        assert_eq!(quadrants[17][8], 0);
        assert!(
            pixels('░', CELL)
                .iter()
                .flatten()
                .all(|alpha| *alpha == 0x40)
        );
    }

    #[test]
    fn braille_draws_one_square_per_dot() {
        assert!(rects('\u{2800}', CELL).unwrap().is_empty());
        let full = pixels('⣿', CELL);
        assert_eq!(lit(&full), 8 * 2 * 2);
        let first = pixels('⠁', CELL);
        assert_eq!(lit(&first), 4);
        assert_eq!(first[2][1], 0xff);
    }

    #[test]
    fn sextants_skip_the_patterns_other_characters_cover() {
        // U+1FB13 is sextant 3 and 5, the middle and bottom left; U+1FB14,
        // after the left half, is 2, 3 and 5.
        let left = pixels('\u{1FB13}', CELL);
        assert_eq!(left[0][0], 0);
        assert_eq!(left[10][0], 0xff);
        assert_eq!(left[17][0], 0xff);
        assert_eq!(left[10][8], 0);
        let next = pixels('\u{1FB14}', CELL);
        assert_eq!(next[0][8], 0xff);
        assert_eq!(next[0][0], 0);
    }

    #[test]
    fn curves_and_slants_are_paths() {
        for ch in [
            '╭', '╮', '╯', '╰', '╱', '╲', '╳', '\u{E0B0}', '\u{E0B4}', '\u{E0B7}',
        ] {
            assert!(covers(ch));
            assert!(rects(ch, CELL).is_none(), "{ch:?}");
            assert!(path(ch, CELL, 1.).unwrap().build().is_ok(), "{ch:?}");
        }
    }
}
