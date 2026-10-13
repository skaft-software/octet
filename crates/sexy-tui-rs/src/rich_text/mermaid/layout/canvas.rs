//! The character canvas the diagram is drawn onto, and its glyph vocabulary.
//!
//! Every cell carries three parallel planes — the character, the semantic class
//! used to colour it, and the line-mask that decides how box-drawing glyphs join
//! up — so they must be written together or the diagram turns into a grid of
//! disconnected strokes. The directional bits (`U`/`D`/`L`/`R`) and the style
//! bits (`STY_*`) are consumed by the layout and routing code, which is why the
//! canvas surface is `pub(super)`; the glyph tables stay private because nothing
//! outside the canvas has any business choosing a box-drawing character.

pub(super) const U: u8 = 1;
pub(super) const D: u8 = 2;
pub(super) const L: u8 = 4;
pub(super) const R: u8 = 8;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Cls {
    Empty,
    Border,
    Text,
    Edge,
    EdgeLabel,
}

pub(super) const STY_DOT: u8 = 1;
pub(super) const STY_THICK: u8 = 2;
pub(super) const STY_SOLID: u8 = 4;

pub(super) struct Canvas {
    pub(super) w: usize,
    pub(super) h: usize,
    pub(super) ch: Vec<String>,
    pub(super) cls: Vec<Cls>,
    pub(super) mask: Vec<u8>,
    pub(super) style: Vec<u8>,
    pub(super) occupied: Vec<bool>,
    pub(super) cur_style: u8,
}

impl Canvas {
    pub(super) fn new(w: usize, h: usize) -> Self {
        let n = w * h;
        Self {
            w,
            h,
            ch: vec![" ".into(); n],
            cls: vec![Cls::Empty; n],
            mask: vec![0; n],
            style: vec![0; n],
            occupied: vec![false; n],
            cur_style: STY_SOLID,
        }
    }

    pub(super) fn idx(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }

    pub(super) fn set(&mut self, x: usize, y: usize, c: impl ToString, cls: Cls) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        if let Some(ch) = self.ch.get_mut(i) {
            *ch = c.to_string();
        }
        if let Some(cl) = self.cls.get_mut(i) {
            *cl = cls;
        }
    }

    pub(super) fn add_bits(&mut self, x: usize, y: usize, bits: u8) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        if self.occupied.get(i) == Some(&true) {
            return;
        }
        if let Some(mask) = self.mask.get_mut(i) {
            *mask |= bits;
        }
        if let Some(style) = self.style.get_mut(i) {
            *style |= self.cur_style;
        }
        if let Some(cls) = self.cls.get_mut(i) {
            if *cls != Cls::Border {
                *cls = Cls::Edge;
            }
        }
    }

    pub(super) fn blit(&mut self, sub: &Canvas, ox: usize, oy: usize) {
        for sy in 0..sub.h {
            for sx in 0..sub.w {
                let (x, y) = (ox + sx, oy + sy);
                if x >= self.w || y >= self.h {
                    continue;
                }
                let si = sub.idx(sx, sy);
                let di = self.idx(x, y);
                let Some(ch) = sub.ch.get(si) else {
                    continue;
                };
                let Some(&cls) = sub.cls.get(si) else {
                    continue;
                };
                let Some(&style) = sub.style.get(si) else {
                    continue;
                };
                if let Some(dch) = self.ch.get_mut(di) {
                    *dch = ch.clone();
                }
                if let Some(dcl) = self.cls.get_mut(di) {
                    *dcl = cls;
                }
                if let Some(dst) = self.style.get_mut(di) {
                    *dst = style;
                }
                if let Some(occ) = self.occupied.get_mut(di) {
                    *occ = true;
                }
            }
        }
    }

    pub(super) fn junction(&mut self, x: usize, y: usize, bits: u8) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = self.idx(x, y);
        if let Some(mask) = self.mask.get_mut(i) {
            *mask |= bits;
        }
        if let Some(cls) = self.cls.get_mut(i) {
            if *cls != Cls::Border {
                *cls = Cls::Edge;
            }
        }
    }

    pub(super) fn seg_v(&mut self, x: usize, y0: usize, y1: usize) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            let mut bits = 0;
            if y > a {
                bits |= U;
            }
            if y < b {
                bits |= D;
            }
            self.add_bits(x, y, bits);
        }
    }

    pub(super) fn seg_h(&mut self, y: usize, x0: usize, x1: usize) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            let mut bits = 0;
            if x > a {
                bits |= L;
            }
            if x < b {
                bits |= R;
            }
            self.add_bits(x, y, bits);
        }
    }

    pub(super) fn finalize_mask(&mut self) {
        for i in 0..self.ch.len() {
            let Some(&mask) = self.mask.get(i) else {
                continue;
            };
            if mask != 0 && self.ch.get(i).is_some_and(|c| c == " ") {
                let c = mask_char(mask);
                let drawn = match self.style.get(i).copied() {
                    Some(STY_DOT) => dotted_char(c),
                    Some(STY_THICK) => thick_char(c),
                    _ => c,
                };
                if let Some(ch) = self.ch.get_mut(i) {
                    *ch = drawn.to_string();
                }
            }
        }
    }

    /// Mirror top-to-bottom for `BT` (rows reorder; within-row text is unaffected, so labels stay readable).
    /// Box-drawing glyphs flip too.
    pub(super) fn flip_vertical(&mut self) {
        for y in 0..self.h / 2 {
            let y2 = self.h - 1 - y;
            for x in 0..self.w {
                let (i, j) = (self.idx(x, y), self.idx(x, y2));
                self.ch.swap(i, j);
                self.cls.swap(i, j);
            }
        }
        for (i, c) in self.ch.iter_mut().enumerate() {
            if !matches!(self.cls[i], Cls::Text | Cls::EdgeLabel) {
                *c = flip_glyph_v(c.chars().next().unwrap()).to_string();
            }
        }
    }

    /// Mirror left-to-right for `RL`.
    /// Mirroring reverses each row, so after flipping glyphs we reverse each text/label run back to reading order.
    pub(super) fn flip_horizontal(&mut self) {
        for y in 0..self.h {
            for x in 0..self.w / 2 {
                let x2 = self.w - 1 - x;
                let (i, j) = (self.idx(x, y), self.idx(x2, y));
                self.ch.swap(i, j);
                self.cls.swap(i, j);
            }
        }
        for (i, c) in self.ch.iter_mut().enumerate() {
            if !matches!(self.cls[i], Cls::Text | Cls::EdgeLabel) {
                *c = flip_glyph_h(c.chars().next().unwrap()).to_string();
            }
        }
        for y in 0..self.h {
            let mut x = 0;
            while x < self.w {
                let Some(&cls) = self.cls.get(self.idx(x, y)) else {
                    break;
                };
                if cls == Cls::Text || cls == Cls::EdgeLabel {
                    let start = self.idx(x, y);
                    while x < self.w && self.cls.get(self.idx(x, y)) == Some(&cls) {
                        x += 1;
                    }
                    let end = self.idx(x, y);
                    if let Some(run) = self.ch.get_mut(start..end) {
                        run.reverse();
                    }
                } else {
                    x += 1;
                }
            }
        }
    }

    pub(super) fn to_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = (0..self.h)
            .map(|y| {
                self.ch[y * self.w..(y + 1) * self.w]
                    .iter()
                    .filter(|c| c.as_str() != "\0")
                    .map(String::as_str)
                    .collect::<String>()
                    .trim_end_matches(' ')
                    .to_owned()
            })
            .collect();
        let first = lines
            .iter()
            .position(|line| !line.is_empty())
            .unwrap_or(lines.len());
        lines.drain(..first);
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }
}

fn mask_char(mask: u8) -> char {
    match mask {
        0 => ' ',
        m if m == U || m == D || m == U | D => '│',
        m if m == L || m == R || m == L | R => '─',
        m if m == D | R => '┌',
        m if m == D | L => '┐',
        m if m == U | R => '└',
        m if m == U | L => '┘',
        m if m == U | D | R => '├',
        m if m == U | D | L => '┤',
        m if m == D | L | R => '┬',
        m if m == U | L | R => '┴',
        _ => '┼',
    }
}

fn dotted_char(c: char) -> char {
    match c {
        '─' => '╌',
        '│' => '╎',
        other => other,
    }
}

fn thick_char(c: char) -> char {
    match c {
        '─' => '━',
        '│' => '┃',
        '┌' => '┏',
        '┐' => '┓',
        '└' => '┗',
        '┘' => '┛',
        '├' => '┣',
        '┤' => '┫',
        '┬' => '┳',
        '┴' => '┻',
        '┼' => '╋',
        other => other,
    }
}

fn flip_glyph_v(c: char) -> char {
    match c {
        '┌' => '└',
        '└' => '┌',
        '┐' => '┘',
        '┘' => '┐',
        '┏' => '┗',
        '┗' => '┏',
        '┓' => '┛',
        '┛' => '┓',
        '╭' => '╰',
        '╰' => '╭',
        '╮' => '╯',
        '╯' => '╮',
        '┬' => '┴',
        '┴' => '┬',
        '┳' => '┻',
        '┻' => '┳',
        '▼' => '▲',
        '▲' => '▼',
        '▽' => '△',
        '△' => '▽',
        other => other,
    }
}

fn flip_glyph_h(c: char) -> char {
    match c {
        '┌' => '┐',
        '┐' => '┌',
        '└' => '┘',
        '┘' => '└',
        '┏' => '┓',
        '┓' => '┏',
        '┗' => '┛',
        '┛' => '┗',
        '╭' => '╮',
        '╮' => '╭',
        '╰' => '╯',
        '╯' => '╰',
        '├' => '┤',
        '┤' => '├',
        '┣' => '┫',
        '┫' => '┣',
        '▶' => '◄',
        '◄' => '▶',
        '▷' => '◁',
        '◁' => '▷',
        other => other,
    }
}
