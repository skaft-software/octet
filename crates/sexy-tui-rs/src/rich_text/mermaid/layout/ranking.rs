//! Rank assignment and crossing reduction for the layered graph.
//!
//! Placing a node means knowing which row it sits in and how far along that row
//! it goes. This module owns both halves of that answer: the initial ranking by
//! longest path from a source, then the ordering pass that reduces edge
//! crossings by repeatedly sorting each row by the barycentre of its neighbours.
//! Only the resulting plan leaves this module — the coordinates it returns are
//! handed to [`super::place_td`] and [`super::place_lr`], which decide what to
//! draw; keeping the solver separate is what makes it possible to change the
//! ordering heuristic without touching the drawing code.

use super::Edge;

pub(super) struct RoutePlan {
    pub(super) canvas: (usize, usize),
    pub(super) band_end: Vec<usize>,
    pub(super) edge_bus: Vec<usize>,
    pub(super) lane_base: usize,
    pub(super) edge_lane: Vec<usize>,
}

pub(super) fn assign_tracks(
    spans: &[(usize, usize, usize, usize, usize)],
) -> (Vec<(usize, usize)>, usize) {
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
pub(super) fn order_ranks(by_rank: &mut [Vec<usize>], edges: &[Edge], ranks: &[usize]) {
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
pub(super) fn assign_positions(
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
