// Copyright 2023-2026 SpaceXAI
// Copyright 2026 Alexey Zaytsev
// SPDX-License-Identifier: Apache-2.0
//
// Adapted from xai-org/grok-build's xai-grok-markdown/src/mermaid.rs,
// the Rust origin of Pi's grok-mermaid 0.2.3. Modified for octet: plain-text
// output, existing parsed graph adapter, no ratatui dependency, Rust 2021,
// hard canvas dimension limits. License: LICENSE-APACHE in this directory.
// The layout algorithms are retained, not replaced with an approximate grid.

use super::{MermaidArt, MermaidError, MAX_MERMAID_ART_HEIGHT, MAX_MERMAID_ART_WIDTH};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_LABEL: usize = 28;
const PAD: usize = 1;
const GAP_X: usize = 3;
const GAP_Y: usize = 2;
const WRAP_WIDTH: usize = 24;
const MAX_LINES: usize = 4;
const LABEL_BREAK_CHARS: [char; 4] = ['_', '-', '.', '/'];
const CONT: char = '\0';
const MAX_CANVAS_CELLS: usize = 1 << 21;

#[derive(Clone, Copy)]
enum Oversize {
    Width,
    Cells,
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Rect,
    Round,
}

struct Node {
    label: String,
    shape: Shape,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Head {
    None,
    Arrow,
}

#[derive(Clone, Copy, PartialEq)]
enum LineKind {
    Solid,
    Dotted,
    Thick,
}

struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
    head_to: Head,
    head_from: Head,
    line: LineKind,
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

struct Group {
    id: String,
    label: String,
    parent: Option<usize>,
}

struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
    groups: Vec<Group>,
    node_group: Vec<Option<usize>>,
    dir: Dir,
}

pub(super) fn layout(
    direction: super::Direction,
    nodes: Vec<super::Node>,
    edges: &[super::Edge],
    groups: &[super::Group],
) -> Result<MermaidArt, MermaidError> {
    let graph = Graph {
        index: nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect(),
        node_group: nodes.iter().map(|n| n.group).collect(),
        nodes: nodes
            .into_iter()
            .map(|n| Node {
                label: n.label,
                shape: if n.rounded { Shape::Round } else { Shape::Rect },
            })
            .collect(),
        edges: edges
            .iter()
            .map(|e| Edge {
                from: e.from,
                to: e.to,
                label: e.label.clone(),
                head_to: if e.directed { Head::Arrow } else { Head::None },
                head_from: Head::None,
                line: match e.stroke {
                    super::Stroke::Solid => LineKind::Solid,
                    super::Stroke::Dotted => LineKind::Dotted,
                    super::Stroke::Thick => LineKind::Thick,
                },
            })
            .collect(),
        groups: groups
            .iter()
            .map(|g| Group {
                id: g.id.clone(),
                label: g.title.clone(),
                parent: g.parent,
            })
            .collect(),
        dir: match direction {
            super::Direction::TopDown => Dir::Down,
            super::Direction::BottomUp => Dir::Up,
            super::Direction::LeftRight => Dir::Right,
            super::Direction::RightLeft => Dir::Left,
        },
    };
    if graph.nodes.is_empty() {
        return Ok(MermaidArt {
            lines: Vec::new(),
            width: 0,
        });
    }
    let lines = if groups.is_empty() {
        layout_flowchart(&graph, None)
    } else {
        render_grouped(&graph, None)
    }
    .map_err(|_| MermaidError::TooLarge {
        detail: format!(
            "diagram exceeds {}x{} or {} cells",
            MAX_MERMAID_ART_WIDTH, MAX_MERMAID_ART_HEIGHT, MAX_CANVAS_CELLS
        ),
    })?;
    let width = lines
        .iter()
        .map(|line| super::display_width(line))
        .max()
        .unwrap_or(0);
    Ok(MermaidArt { lines, width })
}

const U: u8 = 1;
const D: u8 = 2;
const L: u8 = 4;
const R: u8 = 8;

#[derive(Clone, Copy, PartialEq)]
enum Cls {
    Empty,
    Border,
    Text,
    Edge,
    EdgeLabel,
}

const STY_DOT: u8 = 1;
const STY_THICK: u8 = 2;
const STY_SOLID: u8 = 4;

struct Canvas {
    w: usize,
    h: usize,
    ch: Vec<String>,
    cls: Vec<Cls>,
    mask: Vec<u8>,
    style: Vec<u8>,
    occupied: Vec<bool>,
    cur_style: u8,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
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

    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }

    fn set(&mut self, x: usize, y: usize, c: impl ToString, cls: Cls) {
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

    fn add_bits(&mut self, x: usize, y: usize, bits: u8) {
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

    fn blit(&mut self, sub: &Canvas, ox: usize, oy: usize) {
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

    fn junction(&mut self, x: usize, y: usize, bits: u8) {
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

    fn seg_v(&mut self, x: usize, y0: usize, y1: usize) {
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

    fn seg_h(&mut self, y: usize, x0: usize, x1: usize) {
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

    fn finalize_mask(&mut self) {
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
    fn flip_vertical(&mut self) {
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
    fn flip_horizontal(&mut self) {
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

    fn to_lines(&self) -> Vec<String> {
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

struct Placed {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    cx: usize,
    cy: usize,
    rank: usize,
}

struct NodeSizes {
    box_w: Vec<usize>,
    box_h: Vec<usize>,
    lay_w: Vec<usize>,
    lay_h: Vec<usize>,
    extra_h: Vec<usize>,
    self_label_w: Vec<usize>,
}

fn layout_flowchart(graph: &Graph, max_width: Option<usize>) -> Result<Vec<String>, Oversize> {
    let extras: Vec<NodeExtra> = (0..graph.nodes.len()).map(|_| NodeExtra::Plain).collect();
    let mut canvas = layout_canvas(graph, &extras, max_width)?;
    match graph.dir {
        Dir::Up => canvas.flip_vertical(),
        Dir::Left => canvas.flip_horizontal(),
        _ => {}
    }
    Ok(canvas.to_lines())
}

enum NodeExtra {
    Plain,
    Frame(Canvas),
}

fn layout_canvas(
    graph: &Graph,
    extras: &[NodeExtra],
    max_width: Option<usize>,
) -> Result<Canvas, Oversize> {
    let n = graph.nodes.len();
    if n == 0 {
        return Err(Oversize::Cells);
    }

    let ranks = compute_ranks(graph);
    let max_rank = *ranks.iter().max().unwrap_or(&0);

    let mut by_rank: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (idx, &r) in ranks.iter().enumerate() {
        if let Some(row) = by_rank.get_mut(r) {
            row.push(idx);
        }
    }
    order_ranks(&mut by_rank, &graph.edges, &ranks);

    let wrapped: Vec<Vec<String>> = graph
        .nodes
        .iter()
        .map(|node| wrap_label(&node.label, WRAP_WIDTH, MAX_LINES))
        .collect();
    let mut box_w: Vec<usize> = extras
        .iter()
        .zip(graph.nodes.iter())
        .zip(wrapped.iter())
        .map(|((extra, node), wrap)| match extra {
            NodeExtra::Frame(sub) => {
                let title_w = fit_label(&node.label, WRAP_WIDTH).width();
                (sub.w + 2).max(title_w + 4)
            }
            NodeExtra::Plain => {
                wrap.iter().map(|l| l.width()).max().unwrap_or(1).max(1) + 2 * PAD + 2
            }
        })
        .collect();
    let box_h: Vec<usize> = extras
        .iter()
        .zip(wrapped.iter())
        .map(|(extra, wrap)| match extra {
            NodeExtra::Frame(sub) => sub.h + 2,
            NodeExtra::Plain => wrap.len() + 2,
        })
        .collect();

    let mut extra_h = vec![0usize; n];
    let mut self_label_w = vec![0usize; n];
    for e in &graph.edges {
        if e.from == e.to {
            if let Some(h) = extra_h.get_mut(e.from) {
                *h = 2;
            }
            if let Some(l) = &e.label {
                if let Some(w) = self_label_w.get_mut(e.from) {
                    *w = (*w).max(l.width().min(MAX_LABEL));
                }
            }
        }
    }
    for (w, &h) in box_w.iter_mut().zip(&extra_h) {
        if h > 0 {
            *w = (*w).max(7);
        }
    }
    let lay_w: Vec<usize> = box_w
        .iter()
        .zip(&self_label_w)
        .map(|(&w, &sl)| w + if sl > 0 { 2 * (sl + 3) } else { 0 })
        .collect();
    let lay_h: Vec<usize> = box_h.iter().zip(&extra_h).map(|(&h, &e)| h + e).collect();
    let sizes = NodeSizes {
        box_w,
        box_h,
        lay_w,
        lay_h,
        extra_h,
        self_label_w,
    };

    let mut placed: Vec<Placed> = (0..n)
        .map(|_| Placed {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            cx: 0,
            cy: 0,
            rank: 0,
        })
        .collect();

    // BT/RL reuse the TD/LR layout, then flip the finished canvas (so text stays readable) into the bottom-up / right-to-left orientation
    let vertical = matches!(graph.dir, Dir::Down | Dir::Up);
    let plan = if vertical {
        place_td(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
    } else {
        place_lr(&ranks, max_rank, &by_rank, &sizes, graph, &mut placed)
    };
    let (canvas_w, canvas_h) = plan.canvas;

    if let Some(mw) = max_width {
        if canvas_w > mw {
            return Err(Oversize::Width);
        }
    }

    if canvas_w > MAX_MERMAID_ART_WIDTH
        || canvas_h > MAX_MERMAID_ART_HEIGHT
        || canvas_w.saturating_mul(canvas_h) > MAX_CANVAS_CELLS
    {
        return Err(Oversize::Cells);
    }

    let mut canvas = Canvas::new(canvas_w, canvas_h);
    for (idx, extra) in extras.iter().enumerate() {
        let Some(p) = placed.get(idx) else {
            continue;
        };
        match extra {
            NodeExtra::Frame(sub) => {
                let Some(node) = graph.nodes.get(idx) else {
                    continue;
                };
                draw_frame(&mut canvas, p, &node.label, sub)
            }
            NodeExtra::Plain => {
                let Some(wrap) = wrapped.get(idx) else {
                    continue;
                };
                let Some(node) = graph.nodes.get(idx) else {
                    continue;
                };
                draw_box(&mut canvas, p, wrap, node.shape)
            }
        }
    }
    for (i, edge) in graph.edges.iter().enumerate() {
        canvas.cur_style = match edge.line {
            LineKind::Solid => STY_SOLID,
            LineKind::Dotted => STY_DOT,
            LineKind::Thick => STY_THICK,
        };
        if edge.from == edge.to {
            if let Some(p) = placed.get(edge.from) {
                route_self(&mut canvas, p, edge);
            }
            continue;
        }
        let (Some(from), Some(to)) = (placed.get(edge.from), placed.get(edge.to)) else {
            continue;
        };
        let adjacent = to.rank == from.rank + 1;
        let Some(&band) = plan.band_end.get(from.rank) else {
            continue;
        };
        let Some(&edge_bus) = plan.edge_bus.get(i) else {
            continue;
        };
        let Some(&edge_lane) = plan.edge_lane.get(i) else {
            continue;
        };
        let bus = band + edge_bus;
        let lane = plan.lane_base + edge_lane;
        match (vertical, adjacent) {
            (true, true) => route_forward(&mut canvas, from, to, edge, bus),
            (true, false) => route_back(&mut canvas, from, to, edge, lane),
            (false, true) => route_forward_lr(&mut canvas, from, to, edge, bus),
            (false, false) => route_back_lr(&mut canvas, from, to, edge, lane),
        }
    }

    canvas.finalize_mask();
    Ok(canvas)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Node(usize),
    Group(usize),
}

fn render_grouped(graph: &Graph, max_width: Option<usize>) -> Result<Vec<String>, Oversize> {
    let mut proxy: HashMap<usize, usize> = HashMap::new();
    for (gi, g) in graph.groups.iter().enumerate() {
        if let Some(&ni) = graph.index.get(&g.id) {
            proxy.insert(ni, gi);
        }
    }

    let group_chain = |g: Option<usize>| -> Vec<usize> {
        let mut chain = Vec::new();
        let mut cur = g;
        while let Some(gi) = cur {
            chain.push(gi);
            cur = graph.groups.get(gi).and_then(|g| g.parent);
        }
        chain.reverse();
        chain
    };
    let endpoint = |n: usize| -> (Item, Vec<usize>) {
        match proxy.get(&n) {
            Some(&gi) => (
                Item::Group(gi),
                group_chain(graph.groups.get(gi).and_then(|g| g.parent)),
            ),
            None => (
                Item::Node(n),
                group_chain(graph.node_group.get(n).copied().flatten()),
            ),
        }
    };

    let mut scope_edges: HashMap<Option<usize>, Vec<(Item, Item, usize)>> = HashMap::new();
    let mut referenced: Vec<bool> = vec![false; graph.groups.len()];
    for (ei, e) in graph.edges.iter().enumerate() {
        let (item_f, chain_f) = endpoint(e.from);
        let (item_t, chain_t) = endpoint(e.to);
        let k = chain_f
            .iter()
            .zip(&chain_t)
            .take_while(|(a, b)| a == b)
            .count();
        let scope = k.checked_sub(1).and_then(|j| chain_f.get(j)).copied();
        let f = chain_f.get(k).copied().map(Item::Group).unwrap_or(item_f);
        let t = chain_t.get(k).copied().map(Item::Group).unwrap_or(item_t);
        if let Item::Group(gi) = f {
            if let Some(flag) = referenced.get_mut(gi) {
                *flag = true;
            }
        }

        if let Item::Group(gi) = t {
            if let Some(flag) = referenced.get_mut(gi) {
                *flag = true;
            }
        }

        scope_edges.entry(scope).or_default().push((f, t, ei));
    }

    let mut direct_nodes: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (ni, g) in graph.node_group.iter().enumerate() {
        if !proxy.contains_key(&ni) {
            direct_nodes.entry(*g).or_default().push(ni);
        }
    }
    let mut keep = vec![false; graph.groups.len()];
    for gi in (0..graph.groups.len()).rev() {
        let has_nodes = direct_nodes.get(&Some(gi)).is_some_and(|v| !v.is_empty());
        let has_children = (0..graph.groups.len()).any(|c| {
            graph.groups.get(c).is_some_and(|g| g.parent == Some(gi)) && keep.get(c) == Some(&true)
        });
        if let Some(slot) = keep.get_mut(gi) {
            *slot = has_nodes || has_children || referenced.get(gi) == Some(&true);
        }
    }

    let mut canvas = build_scope(graph, None, &scope_edges, &direct_nodes, &keep, max_width)?;
    match graph.dir {
        Dir::Up => canvas.flip_vertical(),
        Dir::Left => canvas.flip_horizontal(),
        _ => {}
    }
    Ok(canvas.to_lines())
}

fn build_scope(
    graph: &Graph,
    scope: Option<usize>,
    scope_edges: &HashMap<Option<usize>, Vec<(Item, Item, usize)>>,
    direct_nodes: &HashMap<Option<usize>, Vec<usize>>,
    keep: &[bool],
    max_width: Option<usize>,
) -> Result<Canvas, Oversize> {
    let mut items: Vec<Item> = Vec::new();
    if let Some(nodes) = direct_nodes.get(&scope) {
        items.extend(nodes.iter().map(|&n| Item::Node(n)));
    }
    let child_groups: Vec<usize> = (0..graph.groups.len())
        .filter(|&gi| {
            graph.groups.get(gi).is_some_and(|g| g.parent == scope) && keep.get(gi) == Some(&true)
        })
        .collect();
    items.extend(child_groups.iter().map(|&gi| Item::Group(gi)));

    if items.is_empty() {
        return Ok(Canvas::new(1, 1));
    }

    let mut index_of: HashMap<Item, usize> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut extras: Vec<NodeExtra> = Vec::new();
    for item in &items {
        index_of.insert(*item, nodes.len());
        match item {
            Item::Node(ni) => {
                let Some(node) = graph.nodes.get(*ni) else {
                    continue;
                };
                nodes.push(Node {
                    label: node.label.clone(),
                    shape: node.shape,
                });
                extras.push(NodeExtra::Plain);
            }
            Item::Group(gi) => {
                let sub = build_scope(graph, Some(*gi), scope_edges, direct_nodes, keep, None)?;
                let Some(group) = graph.groups.get(*gi) else {
                    continue;
                };
                nodes.push(Node {
                    label: group.label.clone(),
                    shape: Shape::Rect,
                });
                extras.push(NodeExtra::Frame(sub));
            }
        }
    }

    let mut edges: Vec<Edge> = Vec::new();
    if let Some(list) = scope_edges.get(&scope) {
        for (f, t, ei) in list {
            let (Some(&fi), Some(&ti)) = (index_of.get(f), index_of.get(t)) else {
                continue;
            };
            let Some(e) = graph.edges.get(*ei) else {
                continue;
            };
            edges.push(Edge {
                from: fi,
                to: ti,
                label: e.label.clone(),
                head_to: e.head_to,
                head_from: e.head_from,
                line: e.line,
            });
        }
    }

    let synth = Graph {
        nodes,
        edges,
        index: HashMap::new(),
        groups: Vec::new(),
        node_group: Vec::new(),
        dir: graph.dir,
    };
    layout_canvas(&synth, &extras, max_width)
}

fn draw_frame(canvas: &mut Canvas, p: &Placed, title: &str, sub: &Canvas) {
    draw_box(canvas, p, &[], Shape::Rect);
    let t = fit_label(title, p.w.saturating_sub(4));
    draw_seq_text(canvas, &format!(" {t} "), p.x + 1, p.y, Cls::Text);
    let ox = p.x + 1 + (p.w - 2 - sub.w) / 2;
    let oy = p.y + 1 + (p.h - 2 - sub.h) / 2;
    canvas.blit(sub, ox, oy);
}

fn bus_spans_td(
    graph: &Graph,
    ranks: &[usize],
    centers: &[usize],
    r: usize,
    exact: bool,
) -> Vec<(usize, usize, usize, usize, usize)> {
    graph
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let (Some(&cf), Some(&ct), Some(&rf), Some(&rt)) = (
                centers.get(e.from),
                centers.get(e.to),
                ranks.get(e.from),
                ranks.get(e.to),
            ) else {
                return false;
            };
            let jogs = if exact { cf != ct } else { cf.abs_diff(ct) > 1 };
            e.from != e.to && rf == r && rt == r + 1 && jogs
        })
        .filter_map(|(i, e)| {
            let cf = *centers.get(e.from)?;
            let ct = *centers.get(e.to)?;
            let a = cf.min(ct);
            let b = cf.max(ct);
            Some((a, b, e.from, e.to, i))
        })
        .collect()
}

fn lane_spans(
    graph: &Graph,
    ranks: &[usize],
    placed: &[Placed],
    vertical: bool,
) -> Vec<(usize, usize, usize, usize, usize)> {
    graph
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.from != e.to
                && ranks
                    .get(e.to)
                    .zip(ranks.get(e.from))
                    .is_some_and(|(&rt, &rf)| rt != rf + 1)
        })
        .filter_map(|(i, e)| {
            let (pf, pt) = (placed.get(e.from)?, placed.get(e.to)?);
            let (a, b) = if vertical {
                (pf.cy.min(pt.cy), pf.cy.max(pt.cy))
            } else {
                (pf.cx.min(pt.cx), pf.cx.max(pt.cx))
            };
            Some((a, b, e.from, e.to, i))
        })
        .collect()
}

fn place_td(
    ranks: &[usize],
    max_rank: usize,
    by_rank: &[Vec<usize>],
    sizes: &NodeSizes,
    graph: &Graph,
    placed: &mut [Placed],
) -> RoutePlan {
    let centers = assign_positions(by_rank, &sizes.lay_w, GAP_X, &graph.edges, ranks);

    let mut edge_bus = vec![0usize; graph.edges.len()];
    let mut bus_tracks = vec![0usize; max_rank + 1];
    for (r, tracks) in bus_tracks.iter_mut().enumerate().take(max_rank) {
        let spans = bus_spans_td(graph, ranks, &centers, r, false);
        if spans.is_empty() {
            continue;
        }
        let (assigned, count) = assign_tracks(&spans);
        for (idx, slot) in assigned {
            if let Some(bus) = edge_bus.get_mut(idx) {
                *bus = slot;
            }
        }
        *tracks = count;
    }

    let rank_h: Vec<usize> = by_rank
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|&i| {
                    Some(sizes.box_h.get(i).copied()? + sizes.extra_h.get(i).copied()?)
                })
                .max()
                .unwrap_or(3)
        })
        .collect();
    let mut rank_y = vec![0usize; max_rank + 1];
    for r in 1..=max_rank {
        let Some(prev) = r.checked_sub(1) else {
            continue;
        };
        let Some(&prev_bus) = bus_tracks.get(prev) else {
            continue;
        };
        let Some(&prev_y) = rank_y.get(prev) else {
            continue;
        };
        let Some(&prev_h) = rank_h.get(prev) else {
            continue;
        };
        let gap = GAP_Y.max(prev_bus + 1);
        if let Some(y) = rank_y.get_mut(r) {
            *y = prev_y + prev_h + gap;
        }
    }
    let canvas_h =
        rank_y.get(max_rank).copied().unwrap_or(0) + rank_h.get(max_rank).copied().unwrap_or(0);
    let band_end: Vec<usize> = (0..=max_rank)
        .filter_map(|r| Some(rank_y.get(r).copied()? + rank_h.get(r).copied()?))
        .collect();

    let mut diagram_w = 1;
    for (r, row) in by_rank.iter().enumerate() {
        for &idx in row {
            let Some(&w) = sizes.box_w.get(idx) else {
                continue;
            };
            let Some(&h) = sizes.box_h.get(idx) else {
                continue;
            };
            let Some(&cx) = centers.get(idx) else {
                continue;
            };
            let extra = sizes.extra_h.get(idx).copied().unwrap_or(0);
            let Some(&ry) = rank_y.get(r) else {
                continue;
            };
            let Some(&rh) = rank_h.get(r) else {
                continue;
            };
            let x = cx.saturating_sub(w / 2);
            let y = ry + (rh - h - extra) / 2;
            if let Some(slot) = placed.get_mut(idx) {
                *slot = Placed {
                    x,
                    y,
                    w,
                    h,
                    cx,
                    cy: y + h / 2,
                    rank: r,
                };
            }
            diagram_w = diagram_w.max(x + w);
            if extra > 0 {
                if let Some(&sl) = sizes.self_label_w.get(idx) {
                    if sl > 0 {
                        diagram_w = diagram_w.max(x + w + 2 + sl);
                    }
                }
            }
        }
    }

    let mut content_w = diagram_w;
    for e in &graph.edges {
        if e.from == e.to {
            continue;
        }
        if let Some(label) = &e.label {
            let lw = label.width().min(MAX_LABEL);
            let adjacent = ranks
                .get(e.to)
                .zip(ranks.get(e.from))
                .is_some_and(|(&rt, &rf)| rt == rf + 1);
            if adjacent {
                if let Some(p) = placed.get(e.to) {
                    content_w = content_w.max(p.cx + 2 + lw);
                }
            } else {
                content_w = content_w.max(diagram_w + lw + 1);
            }
        }
    }

    let mut edge_lane = vec![0usize; graph.edges.len()];
    let lanes = lane_spans(graph, ranks, placed, true);
    let (canvas_w, lane_base) = if lanes.is_empty() {
        (content_w, 0)
    } else {
        let (assigned, count) = assign_tracks(&lanes);
        for (idx, slot) in assigned {
            if let Some(lane) = edge_lane.get_mut(idx) {
                *lane = slot;
            }
        }
        (content_w + 1 + count, content_w + 1)
    };

    RoutePlan {
        canvas: (canvas_w, canvas_h),
        band_end,
        edge_bus,
        lane_base,
        edge_lane,
    }
}

fn place_lr(
    ranks: &[usize],
    max_rank: usize,
    by_rank: &[Vec<usize>],
    sizes: &NodeSizes,
    graph: &Graph,
    placed: &mut [Placed],
) -> RoutePlan {
    let col_w: Vec<usize> = by_rank
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|&i| sizes.box_w.get(i).copied())
                .max()
                .unwrap_or(0)
        })
        .collect();

    let max_label = graph
        .edges
        .iter()
        .filter(|e| {
            e.from == e.to
                || ranks
                    .get(e.to)
                    .zip(ranks.get(e.from))
                    .is_some_and(|(&rt, &rf)| rt == rf + 1)
        })
        .filter_map(|e| e.label.as_ref().map(|l| l.width().min(MAX_LABEL)))
        .max()
        .unwrap_or(0);
    let base_gap = (GAP_X + 1).max(max_label + 3);

    let centers = assign_positions(by_rank, &sizes.lay_h, 1, &graph.edges, ranks);

    let mut edge_bus = vec![0usize; graph.edges.len()];
    let mut bus_tracks = vec![0usize; max_rank + 1];
    for (r, tracks) in bus_tracks.iter_mut().enumerate().take(max_rank) {
        let spans = bus_spans_td(graph, ranks, &centers, r, true);
        if spans.is_empty() {
            continue;
        }
        let (assigned, count) = assign_tracks(&spans);
        for (idx, slot) in assigned {
            if let Some(bus) = edge_bus.get_mut(idx) {
                *bus = slot;
            }
        }
        *tracks = count;
    }

    let mut rank_x = vec![0usize; max_rank + 1];
    for r in 1..=max_rank {
        let Some(prev) = r.checked_sub(1) else {
            continue;
        };
        let Some(&prev_bus) = bus_tracks.get(prev) else {
            continue;
        };
        let Some(&prev_x) = rank_x.get(prev) else {
            continue;
        };
        let Some(&prev_w) = col_w.get(prev) else {
            continue;
        };
        let gap = base_gap.max(prev_bus + 1);
        if let Some(x) = rank_x.get_mut(r) {
            *x = prev_x + prev_w + gap;
        }
    }
    let extra_right = by_rank
        .get(max_rank)
        .into_iter()
        .flatten()
        .filter(|&&i| {
            sizes.extra_h.get(i).copied().unwrap_or(0) > 0
                && sizes.self_label_w.get(i).copied().unwrap_or(0) > 0
        })
        .filter_map(|&i| sizes.self_label_w.get(i).map(|&sl| 2 + sl))
        .max()
        .unwrap_or(0);
    let canvas_w = rank_x.get(max_rank).copied().unwrap_or(0)
        + col_w.get(max_rank).copied().unwrap_or(0)
        + extra_right;
    let band_end: Vec<usize> = (0..=max_rank)
        .filter_map(|r| Some(rank_x.get(r).copied()? + col_w.get(r).copied()?))
        .collect();

    let mut diagram_h = 1;
    for (r, row) in by_rank.iter().enumerate() {
        let Some(&x) = rank_x.get(r) else {
            continue;
        };
        for &idx in row {
            let Some(&w) = sizes.box_w.get(idx) else {
                continue;
            };
            let Some(&h) = sizes.box_h.get(idx) else {
                continue;
            };
            let Some(&cy) = centers.get(idx) else {
                continue;
            };
            let extra = sizes.extra_h.get(idx).copied().unwrap_or(0);
            let y = cy.saturating_sub((h + extra) / 2);
            if let Some(slot) = placed.get_mut(idx) {
                *slot = Placed {
                    x,
                    y,
                    w,
                    h,
                    cx: x + w / 2,
                    cy: y + h / 2,
                    rank: r,
                };
            }
            diagram_h = diagram_h.max(y + h + extra);
        }
    }

    let mut edge_lane = vec![0usize; graph.edges.len()];
    let lanes = lane_spans(graph, ranks, placed, false);
    let (canvas_h, lane_base) = if lanes.is_empty() {
        (diagram_h, 0)
    } else {
        let (assigned, count) = assign_tracks(&lanes);
        for (idx, slot) in assigned {
            if let Some(lane) = edge_lane.get_mut(idx) {
                *lane = slot;
            }
        }
        (diagram_h + 1 + count, diagram_h + 1)
    };

    RoutePlan {
        canvas: (canvas_w, canvas_h),
        band_end,
        edge_bus,
        lane_base,
        edge_lane,
    }
}

struct RoutePlan {
    canvas: (usize, usize),
    band_end: Vec<usize>,
    edge_bus: Vec<usize>,
    lane_base: usize,
    edge_lane: Vec<usize>,
}

fn assign_tracks(spans: &[(usize, usize, usize, usize, usize)]) -> (Vec<(usize, usize)>, usize) {
    let mut sorted = spans.to_vec();
    sorted.sort_unstable();
    let mut tracks: Vec<Vec<(usize, usize, usize, usize)>> = Vec::new();
    let mut out = Vec::with_capacity(sorted.len());
    for &(s, e, f, t, idx) in &sorted {
        let compatible = |members: &Vec<(usize, usize, usize, usize)>| {
            members
                .iter()
                .all(|&(s2, e2, f2, t2)| e2 + 2 <= s || e + 2 <= s2 || f2 == f || t2 == t)
        };
        let slot = match tracks.iter().position(compatible) {
            Some(x) => x,
            None => {
                tracks.push(Vec::new());
                tracks.len() - 1
            }
        };
        if let Some(track) = tracks.get_mut(slot) {
            track.push((s, e, f, t));
        }
        out.push((idx, slot));
    }
    (out, tracks.len())
}

/// Reorder nodes within each rank to minimize edge crossings (Sugiyama-style barycenter sweeps).
/// Alternate down/up passes sort each rank by the mean position of its forward neighbours.
/// The pass keeps the ordering with the fewest crossings between adjacent ranks.
fn order_ranks(by_rank: &mut [Vec<usize>], edges: &[Edge], ranks: &[usize]) {
    let n = ranks.len();
    if by_rank.len() < 2 || n < 3 {
        return;
    }
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        if e.from != e.to
            && ranks
                .get(e.to)
                .zip(ranks.get(e.from))
                .is_some_and(|(&rt, &rf)| rt > rf)
        {
            if let Some(p) = parents.get_mut(e.to) {
                p.push(e.from);
            }
            if let Some(c) = children.get_mut(e.from) {
                c.push(e.to);
            }
        }
    }

    let mut pos = vec![0usize; n];
    let set_pos = |by_rank: &[Vec<usize>], pos: &mut Vec<usize>| {
        for row in by_rank {
            for (i, &v) in row.iter().enumerate() {
                if let Some(slot) = pos.get_mut(v) {
                    *slot = i;
                }
            }
        }
    };
    set_pos(by_rank, &mut pos);

    let mut best: Vec<Vec<usize>> = by_rank.to_vec();
    let mut best_crossings = count_crossings(edges, ranks, &pos);
    if best_crossings == 0 {
        return;
    }

    for it in 0..8 {
        if it % 2 == 0 {
            for row in by_rank.iter_mut().skip(1) {
                sort_by_barycenter(row, &parents, &pos);
                for (i, &v) in row.iter().enumerate() {
                    if let Some(slot) = pos.get_mut(v) {
                        *slot = i;
                    }
                }
            }
        } else {
            let Some(prefix) = by_rank
                .len()
                .checked_sub(1)
                .and_then(|last| by_rank.get_mut(..last))
            else {
                continue;
            };
            for row in prefix.iter_mut().rev() {
                sort_by_barycenter(row, &children, &pos);
                for (i, &v) in row.iter().enumerate() {
                    if let Some(slot) = pos.get_mut(v) {
                        *slot = i;
                    }
                }
            }
        }
        let crossings = count_crossings(edges, ranks, &pos);
        if crossings < best_crossings {
            best_crossings = crossings;
            best = by_rank.to_vec();
        }
        if best_crossings == 0 {
            break;
        }
    }

    for (row, b) in by_rank.iter_mut().zip(best) {
        *row = b;
    }
}

fn sort_by_barycenter(row: &mut [usize], neigh: &[Vec<usize>], pos: &[usize]) {
    let mut keyed: Vec<(f64, usize)> = row
        .iter()
        .map(|&v| {
            let key = match neigh.get(v) {
                Some(ns) if !ns.is_empty() => {
                    ns.iter()
                        .filter_map(|&u| pos.get(u).map(|&p| p as f64))
                        .sum::<f64>()
                        / ns.len() as f64
                }
                _ => pos.get(v).copied().unwrap_or(0) as f64,
            };
            (key, v)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (slot, (_, v)) in row.iter_mut().zip(keyed) {
        *slot = v;
    }
}

fn count_crossings(edges: &[Edge], ranks: &[usize], pos: &[usize]) -> usize {
    let adjacent: Vec<(usize, usize, usize)> = edges
        .iter()
        .filter(|e| {
            e.from != e.to
                && ranks
                    .get(e.to)
                    .zip(ranks.get(e.from))
                    .is_some_and(|(&rt, &rf)| rt == rf + 1)
        })
        .filter_map(|e| Some((*ranks.get(e.from)?, *pos.get(e.from)?, *pos.get(e.to)?)))
        .collect();
    let mut crossings = 0;
    for (i, a) in adjacent.iter().enumerate() {
        let Some(rest) = adjacent.get(i + 1..) else {
            continue;
        };
        for b in rest {
            if a.0 == b.0 && ((a.1 < b.1 && a.2 > b.2) || (a.1 > b.1 && a.2 < b.2)) {
                crossings += 1;
            }
        }
    }
    crossings
}

/// Assign a center coordinate (along the cross-axis) to every node so nodes line up under their neighbours.
/// Iterative barycenter relaxation straightens chains and centers branches.
/// Each node drifts toward the average of its forward neighbours while ranks keep order and a minimum `sep` between boxes.
fn assign_positions(
    by_rank: &[Vec<usize>],
    size: &[usize],
    sep: usize,
    edges: &[Edge],
    ranks: &[usize],
) -> Vec<usize> {
    let n = size.len();
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        if e.from != e.to
            && ranks
                .get(e.to)
                .zip(ranks.get(e.from))
                .is_some_and(|(&rt, &rf)| rt > rf)
        {
            if let Some(p) = parents.get_mut(e.to) {
                p.push(e.from);
            }
            if let Some(c) = children.get_mut(e.from) {
                c.push(e.to);
            }
        }
    }

    let mut pos = vec![0f64; n];
    for row in by_rank {
        let mut x = 0f64;
        for &v in row {
            let half = size.get(v).copied().unwrap_or(0) as f64 / 2.0;
            x += half;
            if let Some(slot) = pos.get_mut(v) {
                *slot = x;
            }
            x += half + sep as f64;
        }
    }

    for it in 0..10 {
        if it % 2 == 0 {
            for row in by_rank.iter() {
                relax_rank(row, &parents, &mut pos, size, sep);
            }
        } else {
            for row in by_rank.iter().rev() {
                relax_rank(row, &children, &mut pos, size, sep);
            }
        }
    }

    let min_left = (0..n)
        .filter_map(|v| Some(*pos.get(v)? - *size.get(v)? as f64 / 2.0))
        .fold(f64::INFINITY, f64::min);
    let min_left = if min_left.is_finite() { min_left } else { 0.0 };
    (0..n)
        .map(|v| {
            (pos.get(v).copied().unwrap_or(0.0) - min_left)
                .round()
                .max(0.0) as usize
        })
        .collect()
}

fn relax_rank(nodes: &[usize], neigh: &[Vec<usize>], pos: &mut [f64], size: &[usize], sep: usize) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let desired: Vec<f64> = nodes
        .iter()
        .map(|&v| match neigh.get(v) {
            Some(ns) if !ns.is_empty() => {
                ns.iter().filter_map(|&u| pos.get(u).copied()).sum::<f64>() / ns.len() as f64
            }
            _ => pos.get(v).copied().unwrap_or(0.0),
        })
        .collect();

    let half = |i: usize| {
        nodes
            .get(i)
            .and_then(|&ni| size.get(ni))
            .copied()
            .unwrap_or(0) as f64
            / 2.0
    };
    let mut left = vec![0f64; n];
    let mut right = vec![0f64; n];
    for i in 0..n {
        let Some(&d) = desired.get(i) else {
            continue;
        };
        let val = if i == 0 {
            d
        } else {
            match i.checked_sub(1).and_then(|j| left.get(j)).copied() {
                Some(prev) => d.max(prev + half(i - 1) + sep as f64 + half(i)),
                None => d,
            }
        };
        if let Some(slot) = left.get_mut(i) {
            *slot = val;
        }
    }
    for i in (0..n).rev() {
        let Some(&d) = desired.get(i) else {
            continue;
        };
        let val = if i == n - 1 {
            d
        } else {
            match right.get(i + 1).copied() {
                Some(next) => d.min(next - half(i + 1) - sep as f64 - half(i)),
                None => d,
            }
        };
        if let Some(slot) = right.get_mut(i) {
            *slot = val;
        }
    }
    for i in 0..n {
        let Some(&ni) = nodes.get(i) else {
            continue;
        };
        let Some(&l) = left.get(i) else {
            continue;
        };
        let Some(&r) = right.get(i) else {
            continue;
        };
        if let Some(slot) = pos.get_mut(ni) {
            *slot = (l + r) / 2.0;
        }
    }
    for i in 1..n {
        let Some(prev) = i.checked_sub(1) else {
            continue;
        };
        let Some(&prev_n) = nodes.get(prev) else {
            continue;
        };
        let Some(&cur_n) = nodes.get(i) else {
            continue;
        };
        let Some(&prev_p) = pos.get(prev_n) else {
            continue;
        };
        let min_p = prev_p + half(prev) + sep as f64 + half(i);
        if let Some(slot) = pos.get_mut(cur_n) {
            if *slot < min_p {
                *slot = min_p;
            }
        }
    }
}

fn wrap_label(label: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in label.split_whitespace() {
        let ww = word.width();
        if ww > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            let mut chunk_w = 0usize;
            for ch in word.graphemes(true) {
                let cw = ch.width();
                if chunk_w + cw > width && !chunk.is_empty() {
                    // Prefer breaking after the last identifier boundary so a long token is not sliced mid-segment; fall back to a per-char break
                    let carry = match chunk.rfind(LABEL_BREAK_CHARS) {
                        Some(p) => chunk.split_off(p + 1),
                        None => String::new(),
                    };
                    lines.push(std::mem::take(&mut chunk));
                    chunk_w = carry.width();
                    chunk = carry;
                }
                chunk.push_str(ch);
                chunk_w += cw;
            }
            cur = chunk;
            cur_w = chunk_w;
        } else if cur.is_empty() {
            cur.push_str(word);
            cur_w = ww;
        } else if cur_w + 1 + ww <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_w = ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            let target = width.saturating_sub(1).max(1);
            let mut s = String::new();
            let mut sw = 0usize;
            for ch in last.graphemes(true) {
                let cw = ch.width();
                if sw + cw > target {
                    break;
                }
                s.push_str(ch);
                sw += cw;
            }
            s.push('…');
            *last = s;
        }
    }
    lines
}

fn fit_label(label: &str, inner: usize) -> String {
    if label.width() <= inner {
        return label.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in label.graphemes(true) {
        let cw = c.width();
        if used + cw + 1 > inner {
            break;
        }
        out.push_str(c);
        used += cw;
    }
    out.push('…');
    out
}

fn draw_box(canvas: &mut Canvas, p: &Placed, lines: &[String], shape: Shape) {
    let (x, y, w, h) = (p.x, p.y, p.w, p.h);
    let right = x + w - 1;
    let bottom = y + h - 1;

    let (tl, tr, bl, br) = match shape {
        Shape::Round => ('╭', '╮', '╰', '╯'),
        Shape::Rect => ('┌', '┐', '└', '┘'),
    };
    canvas.set(x, y, tl, Cls::Border);
    canvas.set(right, y, tr, Cls::Border);
    canvas.set(x, bottom, bl, Cls::Border);
    canvas.set(right, bottom, br, Cls::Border);

    for cx in (x + 1)..right {
        canvas.add_bits(cx, y, L | R);
        canvas.add_bits(cx, bottom, L | R);
    }
    for cy in (y + 1)..bottom {
        canvas.add_bits(x, cy, U | D);
        canvas.add_bits(right, cy, U | D);
    }

    for cy in y..=bottom {
        for cx in x..=right {
            let i = canvas.idx(cx, cy);
            if let Some(occ) = canvas.occupied.get_mut(i) {
                *occ = true;
            }
        }
    }

    let inner = w.saturating_sub(2 * PAD + 2).max(1);
    for (li, line) in lines.iter().enumerate() {
        let row = y + 1 + li;
        let text = fit_label(line, inner);
        let tw = text.width();
        let text_x = x + 1 + PAD + inner.saturating_sub(tw) / 2;
        let mut cur = text_x;
        for c in text.graphemes(true) {
            let cw = c.width();
            if cw == 0 {
                continue;
            }
            canvas.set(cur, row, c, Cls::Text);
            // Wide glyphs (CJK, emoji) own a second column; mark it as a continuation so the line builder doesn't emit a stray space
            for k in 1..cw {
                canvas.set(cur + k, row, CONT, Cls::Text);
            }
            cur += cw;
        }
    }
}

fn route_forward(canvas: &mut Canvas, from: &Placed, to: &Placed, edge: &Edge, bus: usize) {
    let tx = to.cx;
    let bx = if from.cx.abs_diff(tx) <= 1 {
        tx
    } else {
        from.cx
    };
    let by = from.y + from.h - 1;
    let head_row = to.y - 1;

    canvas.junction(bx, by, D);
    canvas.seg_v(bx, by, bus);
    if bx == tx {
        canvas.seg_v(bx, bus, head_row);
    } else {
        canvas.seg_h(bus, bx, tx);
        canvas.seg_v(tx, bus, head_row);
    }

    if edge.head_to == Head::None {
        canvas.add_bits(tx, head_row, U);
    } else {
        canvas.set(tx, head_row, head_glyph(edge.head_to, '▼'), Cls::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(bx, by, head_glyph(edge.head_from, '▲'), Cls::Edge);
    }

    if let Some(label) = &edge.label {
        place_label(canvas, label, head_row, tx + 1);
    }
}

fn head_glyph(_head: Head, arrow: char) -> char {
    arrow
}

fn route_self(canvas: &mut Canvas, p: &Placed, edge: &Edge) {
    let bottom = p.y + p.h - 1;
    let exit_x = p.cx + 1;
    let ret_x = p.x + p.w - 2;
    if ret_x <= exit_x || bottom + 2 >= canvas.h {
        return;
    }
    let (v, h, bl, br) = match edge.line {
        LineKind::Dotted => ('╎', '╌', '╰', '╯'),
        LineKind::Thick => ('┃', '━', '┗', '┛'),
        LineKind::Solid => ('│', '─', '╰', '╯'),
    };
    canvas.junction(exit_x, bottom, D);
    canvas.set(exit_x, bottom + 1, v, Cls::Edge);
    canvas.set(exit_x, bottom + 2, bl, Cls::Edge);
    for x in (exit_x + 1)..ret_x {
        canvas.set(x, bottom + 2, h, Cls::Edge);
    }
    canvas.set(ret_x, bottom + 2, br, Cls::Edge);
    canvas.set(ret_x, bottom + 1, head_glyph(edge.head_to, '▲'), Cls::Edge);
    if let Some(label) = &edge.label {
        place_label(canvas, label, bottom + 1, p.x + p.w + 1);
    }
}

fn route_back(canvas: &mut Canvas, from: &Placed, to: &Placed, edge: &Edge, lane_x: usize) {
    let sx = from.x + from.w - 1;
    let sy = from.cy;
    let tx = to.x + to.w - 1;
    let tyc = to.cy;

    canvas.junction(sx, sy, R);
    canvas.seg_h(sy, sx, lane_x);
    canvas.seg_v(lane_x, sy, tyc);
    canvas.seg_h(tyc, tx + 1, lane_x);

    if edge.head_to == Head::None {
        canvas.add_bits(tx + 1, tyc, R);
    } else {
        canvas.set(tx + 1, tyc, head_glyph(edge.head_to, '◄'), Cls::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(sx, sy, head_glyph(edge.head_from, '◄'), Cls::Edge);
    }

    if let Some(label) = &edge.label {
        place_label(
            canvas,
            label,
            tyc.saturating_sub(1),
            lane_x.saturating_sub(label.width() + 1),
        );
    }
}

fn route_forward_lr(canvas: &mut Canvas, from: &Placed, to: &Placed, edge: &Edge, bus: usize) {
    let rx = from.x + from.w - 1;
    let ry = from.cy;
    let ly = to.cy;
    let head_col = to.x - 1;

    canvas.junction(rx, ry, R);
    canvas.seg_h(ry, rx, bus);
    if ry == ly {
        canvas.seg_h(ry, bus, head_col);
    } else {
        canvas.seg_v(bus, ry, ly);
        canvas.seg_h(ly, bus, head_col);
    }

    if edge.head_to == Head::None {
        canvas.add_bits(head_col, ly, R);
    } else {
        canvas.set(head_col, ly, head_glyph(edge.head_to, '▶'), Cls::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(rx, ry, head_glyph(edge.head_from, '◄'), Cls::Edge);
    }

    if let Some(label) = &edge.label {
        place_label(canvas, label, ly.saturating_sub(1), bus + 1);
    }
}

fn route_back_lr(canvas: &mut Canvas, from: &Placed, to: &Placed, edge: &Edge, lane_y: usize) {
    let sx = from.cx;
    let sy = from.y + from.h - 1;
    let tx = to.cx;
    let ty = to.y + to.h - 1;

    canvas.junction(sx, sy, D);
    canvas.seg_v(sx, sy, lane_y);
    canvas.seg_h(lane_y, sx, tx);
    canvas.seg_v(tx, lane_y, ty + 1);

    if edge.head_to == Head::None {
        canvas.add_bits(tx, ty + 1, D);
    } else {
        canvas.set(tx, ty + 1, head_glyph(edge.head_to, '▲'), Cls::Edge);
    }
    if edge.head_from != Head::None {
        canvas.set(sx, sy, head_glyph(edge.head_from, '▲'), Cls::Edge);
    }

    if let Some(label) = &edge.label {
        place_label(canvas, label, lane_y.saturating_sub(1), (sx + tx) / 2);
    }
}

fn place_label(canvas: &mut Canvas, label: &str, row: usize, start_x: usize) {
    if row >= canvas.h {
        return;
    }
    let text = fit_label(label, MAX_LABEL);
    let mut x = start_x;
    for c in text.graphemes(true) {
        let cw = c.width();
        if cw == 0 {
            continue;
        }
        if x + cw > canvas.w {
            break;
        }
        let blocked = (0..cw).any(|k| {
            let i = canvas.idx(x + k, row);
            canvas.ch.get(i).is_some_and(|c| c != " ")
                || canvas.mask.get(i).is_some_and(|&m| m != 0)
                || canvas.occupied.get(i) == Some(&true)
        });
        if blocked {
            break;
        }
        canvas.set(x, row, c, Cls::EdgeLabel);
        for k in 1..cw {
            canvas.set(x + k, row, CONT, Cls::EdgeLabel);
        }
        x += cw;
    }
}

fn compute_ranks(graph: &Graph) -> Vec<usize> {
    let n = graph.nodes.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for e in &graph.edges {
        if e.from != e.to {
            if let Some(ch) = children.get_mut(e.from) {
                ch.push(e.to);
            }
            if let Some(d) = indeg.get_mut(e.to) {
                *d += 1;
            }
        }
    }

    let mut color = vec![0u8; n];
    let mut dag: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut order: Vec<usize> = Vec::with_capacity(n);

    let roots: Vec<usize> = (0..n).filter(|&i| indeg.get(i) == Some(&0)).collect();
    for start in roots.iter().copied().chain(0..n) {
        if color.get(start) == Some(&0) {
            dfs_dag(start, &children, &mut color, &mut dag, &mut order);
        }
    }

    let mut rank = vec![0usize; n];
    for &u in order.iter().rev() {
        let Some(vs) = dag.get(u) else {
            continue;
        };
        for &v in vs {
            let Some(&ru) = rank.get(u) else {
                continue;
            };
            if let Some(rv) = rank.get_mut(v) {
                *rv = (*rv).max(ru + 1);
            }
        }
    }
    rank
}

fn dfs_dag(
    start: usize,
    children: &[Vec<usize>],
    color: &mut [u8],
    dag: &mut [Vec<usize>],
    order: &mut Vec<usize>,
) {
    let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
    if let Some(c) = color.get_mut(start) {
        *c = 1;
    }
    while let Some(frame) = stack.last_mut() {
        let u = frame.0;
        let kids = children.get(u).map(|c| c.as_slice()).unwrap_or(&[]);
        if frame.1 < kids.len() {
            let Some(&v) = kids.get(frame.1) else {
                break;
            };
            frame.1 += 1;
            if color.get(v) == Some(&1) {
                continue;
            }
            if let Some(d) = dag.get_mut(u) {
                d.push(v);
            }
            if color.get(v) == Some(&0) {
                if let Some(c) = color.get_mut(v) {
                    *c = 1;
                }
                stack.push((v, 0));
            }
        } else {
            if let Some(c) = color.get_mut(u) {
                *c = 2;
            }
            order.push(u);
            stack.pop();
        }
    }
}

fn draw_seq_text(canvas: &mut Canvas, text: &str, x: usize, y: usize, cls: Cls) {
    let mut cur = x;
    for c in text.graphemes(true) {
        let cw = c.width();
        if cw == 0 {
            continue;
        }
        for k in 0..cw {
            if cur + k < canvas.w && y < canvas.h {
                let i = canvas.idx(cur + k, y);
                if let Some(mask) = canvas.mask.get_mut(i) {
                    *mask = 0;
                }
            }
            canvas.set(cur + k, y, if k == 0 { c } else { "\0" }, cls);
        }
        cur += cw;
    }
}
