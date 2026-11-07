//! A screen drawn as a PNG: the cell grid of `letibot_vt::Screen`, a 5x7 bitmap font at 2x,
//! the sixteen-slot palette, and a zlib stream from `flate2`. No font file, no image crate, so the
//! picture is the same on every box and a model can read it.

use std::io::Write;

use letibot_vt::{Attr, Cell, Screen};

/// Pixels per cell. The glyph is 5x8 columns at 2x inside it, with a pixel of margin.
pub const CELL_W: usize = 12;
pub const CELL_H: usize = 20;

const DEFAULT_FG: [u8; 3] = [0xd0, 0xd0, 0xd0];
const DEFAULT_BG: [u8; 3] = [0x10, 0x10, 0x10];
const PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x31, 0x31],
    [0x0d, 0xbc, 0x79],
    [0xe5, 0xe5, 0x10],
    [0x24, 0x72, 0xc8],
    [0xbc, 0x3f, 0xbc],
    [0x11, 0xa8, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x66, 0x66, 0x66],
    [0xf1, 0x4c, 0x4c],
    [0x23, 0xd1, 0x8b],
    [0xf5, 0xf5, 0x43],
    [0x3b, 0x8e, 0xea],
    [0xd6, 0x70, 0xd6],
    [0x29, 0xb8, 0xdb],
    [0xff, 0xff, 0xff],
];

/// The colour names `read` prints for a slot, so the text view and the picture agree.
pub const SLOT_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

/// The 5x7 face, ASCII 0x20..0x7e, one column per byte with the top row in bit 0.
#[rustfmt::skip]
const FONT: [[u8; 5]; 95] = [
    [0x00,0x00,0x00,0x00,0x00],[0x00,0x00,0x5f,0x00,0x00],[0x00,0x07,0x00,0x07,0x00],
    [0x14,0x7f,0x14,0x7f,0x14],[0x24,0x2a,0x7f,0x2a,0x12],[0x23,0x13,0x08,0x64,0x62],
    [0x36,0x49,0x56,0x20,0x50],[0x00,0x05,0x03,0x00,0x00],[0x00,0x1c,0x22,0x41,0x00],
    [0x00,0x41,0x22,0x1c,0x00],[0x14,0x08,0x3e,0x08,0x14],[0x08,0x08,0x3e,0x08,0x08],
    [0x00,0x50,0x30,0x00,0x00],[0x08,0x08,0x08,0x08,0x08],[0x00,0x60,0x60,0x00,0x00],
    [0x20,0x10,0x08,0x04,0x02],[0x3e,0x51,0x49,0x45,0x3e],[0x00,0x42,0x7f,0x40,0x00],
    [0x42,0x61,0x51,0x49,0x46],[0x21,0x41,0x45,0x4b,0x31],[0x18,0x14,0x12,0x7f,0x10],
    [0x27,0x45,0x45,0x45,0x39],[0x3c,0x4a,0x49,0x49,0x30],[0x01,0x71,0x09,0x05,0x03],
    [0x36,0x49,0x49,0x49,0x36],[0x06,0x49,0x49,0x29,0x1e],[0x00,0x36,0x36,0x00,0x00],
    [0x00,0x56,0x36,0x00,0x00],[0x08,0x14,0x22,0x41,0x00],[0x14,0x14,0x14,0x14,0x14],
    [0x00,0x41,0x22,0x14,0x08],[0x02,0x01,0x51,0x09,0x06],[0x32,0x49,0x79,0x41,0x3e],
    [0x7e,0x11,0x11,0x11,0x7e],[0x7f,0x49,0x49,0x49,0x36],[0x3e,0x41,0x41,0x41,0x22],
    [0x7f,0x41,0x41,0x22,0x1c],[0x7f,0x49,0x49,0x49,0x41],[0x7f,0x09,0x09,0x09,0x01],
    [0x3e,0x41,0x49,0x49,0x7a],[0x7f,0x08,0x08,0x08,0x7f],[0x00,0x41,0x7f,0x41,0x00],
    [0x20,0x40,0x41,0x3f,0x01],[0x7f,0x08,0x14,0x22,0x41],[0x7f,0x40,0x40,0x40,0x40],
    [0x7f,0x02,0x0c,0x02,0x7f],[0x7f,0x04,0x08,0x10,0x7f],[0x3e,0x41,0x41,0x41,0x3e],
    [0x7f,0x09,0x09,0x09,0x06],[0x3e,0x41,0x51,0x21,0x5e],[0x7f,0x09,0x19,0x29,0x46],
    [0x46,0x49,0x49,0x49,0x31],[0x01,0x01,0x7f,0x01,0x01],[0x3f,0x40,0x40,0x40,0x3f],
    [0x1f,0x20,0x40,0x20,0x1f],[0x3f,0x40,0x38,0x40,0x3f],[0x63,0x14,0x08,0x14,0x63],
    [0x07,0x08,0x70,0x08,0x07],[0x61,0x51,0x49,0x45,0x43],[0x00,0x7f,0x41,0x41,0x00],
    [0x02,0x04,0x08,0x10,0x20],[0x00,0x41,0x41,0x7f,0x00],[0x04,0x02,0x01,0x02,0x04],
    [0x40,0x40,0x40,0x40,0x40],[0x00,0x01,0x02,0x04,0x00],[0x20,0x54,0x54,0x54,0x78],
    [0x7f,0x48,0x44,0x44,0x38],[0x38,0x44,0x44,0x44,0x20],[0x38,0x44,0x44,0x48,0x7f],
    [0x38,0x54,0x54,0x54,0x18],[0x08,0x7e,0x09,0x01,0x02],[0x18,0xa4,0xa4,0x9c,0x78],
    [0x7f,0x08,0x04,0x04,0x78],[0x00,0x44,0x7d,0x40,0x00],[0x40,0x80,0x84,0x7d,0x00],
    [0x7f,0x10,0x28,0x44,0x00],[0x00,0x41,0x7f,0x40,0x00],[0x7c,0x04,0x18,0x04,0x78],
    [0x7c,0x08,0x04,0x04,0x78],[0x38,0x44,0x44,0x44,0x38],[0xfc,0x18,0x24,0x24,0x18],
    [0x18,0x24,0x24,0x18,0xfc],[0x7c,0x08,0x04,0x04,0x08],[0x48,0x54,0x54,0x54,0x20],
    [0x04,0x3f,0x44,0x40,0x20],[0x3c,0x40,0x40,0x20,0x7c],[0x1c,0x20,0x40,0x20,0x1c],
    [0x3c,0x40,0x30,0x40,0x3c],[0x44,0x28,0x10,0x28,0x44],[0x4c,0x90,0x90,0x90,0x7c],
    [0x44,0x64,0x54,0x4c,0x44],[0x00,0x08,0x36,0x41,0x00],[0x00,0x00,0x7f,0x00,0x00],
    [0x00,0x41,0x36,0x08,0x00],[0x10,0x08,0x08,0x10,0x08],
];

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                let i = (yy * self.w + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
        }
    }
}

fn mix(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    [
        ((a[0] as u16 + b[0] as u16) / 2) as u8,
        ((a[1] as u16 + b[1] as u16) / 2) as u8,
        ((a[2] as u16 + b[2] as u16) / 2) as u8,
    ]
}

/// The foreground and background a cell is drawn in, reverse and dim applied.
fn colours(attr: Attr) -> ([u8; 3], [u8; 3]) {
    let mut fg = match attr.fg {
        // Bold takes the bright slot of a normal one, as terminals do.
        Some(s) if attr.bold && s < 8 => PALETTE[(s + 8) as usize],
        Some(s) => PALETTE[(s % 16) as usize],
        None if attr.bold => PALETTE[15],
        None => DEFAULT_FG,
    };
    let mut bg = attr.bg.map_or(DEFAULT_BG, |s| PALETTE[(s % 16) as usize]);
    if attr.dim {
        fg = mix(fg, bg);
    }
    if attr.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

/// Light (1) or heavy (2) arms of a box-drawing character: up, down, left, right.
fn arms(c: char) -> Option<[u8; 4]> {
    let t = match c {
        '─' => [0, 0, 1, 1],
        '━' => [0, 0, 2, 2],
        '│' => [1, 1, 0, 0],
        '┃' => [2, 2, 0, 0],
        '┌' | '╭' => [0, 1, 0, 1],
        '┐' | '╮' => [0, 1, 1, 0],
        '└' | '╰' => [1, 0, 0, 1],
        '┘' | '╯' => [1, 0, 1, 0],
        '├' => [1, 1, 0, 1],
        '┤' => [1, 1, 1, 0],
        '┬' => [0, 1, 1, 1],
        '┴' => [1, 0, 1, 1],
        '┼' => [1, 1, 1, 1],
        '┏' => [0, 2, 0, 2],
        '┓' => [0, 2, 2, 0],
        '┗' => [2, 0, 0, 2],
        '┛' => [2, 0, 2, 0],
        '┣' => [2, 2, 0, 2],
        '┫' => [2, 2, 2, 0],
        '┳' => [0, 2, 2, 2],
        '┻' => [2, 0, 2, 2],
        '╋' => [2, 2, 2, 2],
        '═' => [0, 0, 2, 2],
        '║' => [2, 2, 0, 0],
        '╔' => [0, 2, 0, 2],
        '╗' => [0, 2, 2, 0],
        '╚' => [2, 0, 0, 2],
        '╝' => [2, 0, 2, 0],
        '╠' => [2, 2, 0, 2],
        '╣' => [2, 2, 2, 0],
        '╦' => [0, 2, 2, 2],
        '╩' => [2, 0, 2, 2],
        '╬' => [2, 2, 2, 2],
        _ => return None,
    };
    Some(t)
}

fn glyph(cv: &mut Canvas, x: usize, y: usize, c: char, fg: [u8; 3], bold: bool, wide: usize) {
    let w = CELL_W * wide;
    if let Some(a) = arms(c) {
        let (cx, cy) = (x + w / 2, y + CELL_H / 2);
        let th = |n: u8| if n == 2 { 3 } else { 2 };
        if a[0] > 0 {
            cv.fill(cx - th(a[0]) / 2, y, th(a[0]), CELL_H / 2 + 1, fg);
        }
        if a[1] > 0 {
            cv.fill(cx - th(a[1]) / 2, cy, th(a[1]), CELL_H - CELL_H / 2, fg);
        }
        if a[2] > 0 {
            cv.fill(x, cy - th(a[2]) / 2, w / 2 + 1, th(a[2]), fg);
        }
        if a[3] > 0 {
            cv.fill(cx, cy - th(a[3]) / 2, w - w / 2, th(a[3]), fg);
        }
        return;
    }
    match c {
        '█' => return cv.fill(x, y, w, CELL_H, fg),
        '▀' => return cv.fill(x, y, w, CELL_H / 2, fg),
        '▄' => return cv.fill(x, y + CELL_H / 2, w, CELL_H / 2, fg),
        '▌' => return cv.fill(x, y, w / 2, CELL_H, fg),
        '▐' => return cv.fill(x + w / 2, y, w / 2, CELL_H, fg),
        '▁'..='▇' => {
            let n = (c as u32 - '▁' as u32 + 1) as usize;
            let h = CELL_H * n / 8;
            return cv.fill(x, y + CELL_H - h, w, h, fg);
        }
        '\u{2589}'..='\u{258f}' => {
            let n = (0x2590 - c as u32) as usize;
            return cv.fill(x, y, w * n / 8, CELL_H, fg);
        }
        '·' | '•' | '…' | '›' | '‹' => {
            let (cx, cy) = (x + w / 2, y + CELL_H / 2);
            match c {
                '·' => cv.fill(cx - 1, cy - 1, 2, 2, fg),
                '•' => cv.fill(cx - 2, cy - 2, 4, 4, fg),
                '…' => {
                    for i in 0..3 {
                        cv.fill(x + 1 + i * 4, y + CELL_H - 6, 2, 2, fg);
                    }
                }
                _ => {
                    // A chevron: two steps each way from the middle.
                    for i in 0..4 {
                        let dx = if c == '›' { i } else { 3 - i };
                        cv.fill(cx - 2 + dx, cy - 4 + i, 2, 2, fg);
                        cv.fill(cx - 2 + dx, cy + 4 - i, 2, 2, fg);
                    }
                }
            }
            return;
        }
        '░' | '▒' | '▓' => {
            let step = match c {
                '░' => 4,
                '▒' => 2,
                _ => 1,
            };
            for yy in 0..CELL_H {
                for xx in 0..w {
                    if (xx + yy) % (step + 1) == 0 || step == 1 && (xx + yy) % 4 != 0 {
                        cv.fill(x + xx, y + yy, 1, 1, fg);
                    }
                }
            }
            return;
        }
        '\u{2800}'..='\u{28ff}' => {
            let bits = c as u32 - 0x2800;
            // Dot (col, row) for bits 0..7 in braille numbering.
            let at = [
                (0, 0),
                (0, 1),
                (0, 2),
                (1, 0),
                (1, 1),
                (1, 2),
                (0, 3),
                (1, 3),
            ];
            for (i, (dc, dr)) in at.iter().enumerate() {
                if bits & (1 << i) != 0 {
                    cv.fill(x + 2 + dc * (w / 2 - 1), y + 2 + dr * 5, 3, 3, fg);
                }
            }
            return;
        }
        _ => {}
    }
    let rows: [u8; 5] = match c as u32 {
        0x20..=0x7e => FONT[c as usize - 0x20],
        _ if c.is_whitespace() => return,
        // A glyph this face lacks is a hollow box: present, and not mistaken for a space.
        _ => {
            cv.fill(x + 2, y + 2, w - 4, 1, fg);
            cv.fill(x + 2, y + CELL_H - 4, w - 4, 1, fg);
            cv.fill(x + 2, y + 2, 1, CELL_H - 5, fg);
            cv.fill(x + w - 3, y + 2, 1, CELL_H - 5, fg);
            return;
        }
    };
    for (col, bits) in rows.iter().enumerate() {
        for row in 0..8 {
            if bits & (1 << row) != 0 {
                let (px, py) = (x + 1 + col * 2, y + 2 + row * 2);
                cv.fill(px, py, if bold { 3 } else { 2 }, 2, fg);
            }
        }
    }
}

/// The screen as RGB pixels and its size.
pub fn rgb(screen: &Screen) -> (usize, usize, Vec<u8>) {
    let (rows, cols) = screen.size();
    let (w, h) = (cols * CELL_W, rows * CELL_H);
    let mut cv = Canvas {
        w,
        h,
        px: vec![0; w * h * 3],
    };
    cv.fill(0, 0, w, h, DEFAULT_BG);
    let (cr, cc) = screen.cursor();
    let show_cursor = screen.cursor_visible();
    for (r, row) in screen.rows().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if cell.is_wide_tail() {
                continue;
            }
            let mut attr = cell.attr;
            if show_cursor && r == cr && c == cc {
                attr.reverse = !attr.reverse;
            }
            draw(&mut cv, r, c, cell, attr);
        }
    }
    (w, h, cv.px)
}

fn draw(cv: &mut Canvas, r: usize, c: usize, cell: &Cell, attr: Attr) {
    let (fg, bg) = colours(attr);
    let wide = if cell.is_wide_lead() { 2 } else { 1 };
    let (x, y) = (c * CELL_W, r * CELL_H);
    if bg != DEFAULT_BG {
        cv.fill(x, y, CELL_W * wide, CELL_H, bg);
    }
    if cell.ch != ' ' {
        glyph(cv, x, y, cell.ch, fg, attr.bold, wide);
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&h.finalize().to_be_bytes());
}

/// RGB8 pixels as a PNG.
pub fn encode_png(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(h * (w * 3 + 1));
    for line in rgb.chunks(w * 3) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = z.write_all(&raw);
    let data = z.finish().unwrap_or_default();
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &data);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The screen as a PNG of `cols * CELL_W` by `rows * CELL_H` pixels.
pub fn png(screen: &Screen) -> Vec<u8> {
    let (w, h, px) = rgb(screen);
    encode_png(w, h, &px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_is_the_grid_times_the_cell() {
        let mut s = Screen::new(5, 20);
        s.feed(b"\x1b[7mhi\x1b[0m \x1b[31mred\x1b[0m");
        let bytes = png(&s);
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!((w as usize, h as usize), (20 * CELL_W, 5 * CELL_H));
        let (_, _, px) = rgb(&s);
        // The reversed 'h' cell is the default foreground, the blank cell beside the text is not.
        let at = |x: usize, y: usize| &px[(y * 20 * CELL_W + x) * 3..][..3];
        assert_eq!(at(0, 0), &DEFAULT_FG);
        assert_eq!(at(CELL_W * 10 + 1, 1), &DEFAULT_BG);
    }

    #[test]
    fn every_ascii_glyph_draws_something() {
        for c in ' '..='~' {
            let mut s = Screen::new(3, 10);
            s.feed(c.to_string().as_bytes());
            // Move the cursor off the cell so it is not drawn inverted.
            s.feed(b"\x1b[3;9H");
            let (_, _, px) = rgb(&s);
            let lit = (0..CELL_H)
                .flat_map(|y| (0..CELL_W).map(move |x| (x, y)))
                .any(|(x, y)| px[(y * 10 * CELL_W + x) * 3..][..3] != DEFAULT_BG);
            assert_eq!(lit, c != ' ', "{c:?}");
        }
    }

    /// Prints the face so a person can read it: `cargo test dump_font -- --nocapture --ignored`.
    #[test]
    #[ignore]
    fn dump_font() {
        for row in FONT.chunks(16) {
            for y in 0..8 {
                let line: String = row
                    .iter()
                    .map(|g| {
                        (0..5)
                            .map(|c| if g[c] & (1 << y) != 0 { '#' } else { '.' })
                            .chain(std::iter::once(' '))
                            .collect::<String>()
                    })
                    .collect();
                println!("{line}");
            }
            println!();
        }
    }
}
