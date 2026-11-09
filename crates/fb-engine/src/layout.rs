//! Layered (left-to-right) layout for production graphs, built on petgraph.
//! The usual Sugiyama steps:
//!
//! 1. Break cycles (recycling loops) by turning the wires petgraph's greedy
//!    feedback arc set picks around.
//! 2. Layering: longest path from the raw inputs, then each node moves as
//!    close to its consumers as it can when that shortens its wires overall.
//! 3. Wires that skip columns get a lane (a placeholder in every column they
//!    pass), so they take part in ordering and nothing is put in their way.
//! 4. Ordering: barycentre sweeps left to right and back (by the port each
//!    wire leaves from), keeping the order with the fewest crossings, then
//!    swapping neighbours while that removes crossings.
//! 5. Rows: each node moves so its ports line up with the ports they're
//!    wired to, keeping the order and without overlapping (an exact
//!    least-squares fit per column), so wires run as straight as they can.

use petgraph::algo::{greedy_feedback_arc_set, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashSet;

pub const COLUMN_WIDTH: f32 = 300.0;
pub const ROW_HEIGHT: f32 = 230.0;
/// Space between columns and between stacked nodes.
pub const GAP_X: f32 = 90.0;
pub const GAP_Y: f32 = 40.0;
/// Space kept beside a lane (a wire passing through a column).
const LANE_GAP: f32 = GAP_Y;
/// Ordering sweeps tried (each is left-to-right then back).
const SWEEPS: usize = 12;
/// Rounds of levelling nodes with their neighbours.
const ROUNDS: usize = 12;
/// How much the port a wire leaves from counts in ordering, as a fraction
/// of a place in the column (0.3 untangled the most on real plans).
const PORT_FRACTION: f32 = 0.3;

/// Returns a canvas position for each of `n` nodes, all assumed the same size.
pub fn layered(n: usize, edges: &[(usize, usize)]) -> Vec<[f32; 2]> {
    layered_sized(n, edges, COLUMN_WIDTH, &vec![ROW_HEIGHT - GAP_Y; n])
}

/// Like [`layered`], but stacks nodes by their real heights so tall nodes
/// don't overlap; `width` is the node width. Wires are taken to join at the
/// middle of each node.
pub fn layered_sized(n: usize, edges: &[(usize, usize)], width: f32, heights: &[f32]) -> Vec<[f32; 2]> {
    let h = |i: usize| heights.get(i).copied().unwrap_or(ROW_HEIGHT - GAP_Y);
    let wires: Vec<_> = edges.iter().map(|&(a, b)| (a, b, h(a) / 2.0, h(b) / 2.0)).collect();
    layered_ports(n, &wires, width, heights)
}

/// Like [`layered_sized`], with where each wire joins its nodes: `(from,
/// to, from_port_y, to_port_y)`, the port heights measured from each node's
/// top. Nodes line up on their ports, so wires run level.
pub fn layered_ports(n: usize, wires: &[(usize, usize, f32, f32)], width: f32, heights: &[f32]) -> Vec<[f32; 2]> {
    let (layer, y) = place(n, wires, heights);
    (0..n).map(|i| [40.0 + layer[i] as f32 * (width + GAP_X), y[i]]).collect()
}

/// Like [`layered_ports`], for nodes of different widths: each column is as
/// wide as its widest node, and narrower nodes sit in its middle.
pub fn layered_ports_widths(n: usize, wires: &[(usize, usize, f32, f32)], widths: &[f32], heights: &[f32]) -> Vec<[f32; 2]> {
    let (layer, y) = place(n, wires, heights);
    let w = |i: usize| widths.get(i).copied().unwrap_or(COLUMN_WIDTH);
    let columns = layer.iter().max().map_or(0, |m| m + 1);
    let mut col = vec![0.0f32; columns];
    for i in 0..n {
        col[layer[i]] = col[layer[i]].max(w(i));
    }
    let mut left = vec![40.0f32; columns];
    for c in 1..columns {
        left[c] = left[c - 1] + col[c - 1] + GAP_X;
    }
    (0..n).map(|i| [left[layer[i]] + (col[layer[i]] - w(i)) / 2.0, y[i]]).collect()
}

/// Each node's column and top.
fn place(n: usize, wires: &[(usize, usize, f32, f32)], heights: &[f32]) -> (Vec<usize>, Vec<f32>) {
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let pairs: Vec<(usize, usize)> = wires.iter().map(|w| (w.0, w.1)).collect();
    let dag = acyclic(n, &pairs);
    let kept: HashSet<(usize, usize)> = dag.iter().copied().collect();
    // Each wire the way round the DAG has it, with its ports.
    let mut oriented = Vec::new();
    let mut seen = HashSet::new();
    for &(a, b, pa, pb) in wires {
        let w = if kept.contains(&(a, b)) {
            (a, b, pa, pb)
        } else if kept.contains(&(b, a)) {
            (b, a, pb, pa)
        } else {
            continue;
        };
        if seen.insert((w.0, w.1, w.2.to_bits(), w.3.to_bits())) {
            oriented.push(w);
        }
    }
    let layer = layers(n, &dag);
    let mut lg = LayerGraph::new(n, &oriented, &layer, |i| heights.get(i).copied().unwrap_or(ROW_HEIGHT - GAP_Y));
    lg.order();
    let y = lg.rows();
    (layer, y)
}

/// The edges (deduplicated, no self loops) with cycles broken by turning
/// back edges around; if that still leaves a cycle, they're dropped.
fn acyclic(n: usize, edges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut g: DiGraph<(), ()> = DiGraph::with_capacity(n, edges.len());
    for _ in 0..n {
        g.add_node(());
    }
    let mut seen = HashSet::new();
    for &(a, b) in edges {
        if a != b && a < n && b < n && seen.insert((a, b)) {
            g.add_edge(NodeIndex::new(a), NodeIndex::new(b), ());
        }
    }
    let back: HashSet<_> = greedy_feedback_arc_set(&g).map(|e| e.id()).collect();
    let forward = |reverse: bool| {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for e in g.edge_references() {
            let (a, b) = (e.source().index(), e.target().index());
            let (a, b) = match (back.contains(&e.id()), reverse) {
                (false, _) => (a, b),
                (true, true) => (b, a),
                (true, false) => continue,
            };
            if seen.insert((a, b)) {
                out.push((a, b));
            }
        }
        out
    };
    let turned = forward(true);
    let mut check: DiGraph<(), ()> = DiGraph::with_capacity(n, turned.len());
    for _ in 0..n {
        check.add_node(());
    }
    for &(a, b) in &turned {
        check.add_edge(NodeIndex::new(a), NodeIndex::new(b), ());
    }
    if toposort(&check, None).is_ok() {
        turned
    } else {
        forward(false)
    }
}

/// Column of each node: longest path from the sources, then nodes move
/// right towards their consumers when they feed more wires than they take
/// (it shortens more wires than it lengthens). Sources so end up right
/// beside their first consumer.
fn layers(n: usize, dag: &[(usize, usize)]) -> Vec<usize> {
    let mut g: DiGraph<(), ()> = DiGraph::with_capacity(n, dag.len());
    for _ in 0..n {
        g.add_node(());
    }
    for &(a, b) in dag {
        g.add_edge(NodeIndex::new(a), NodeIndex::new(b), ());
    }
    let order: Vec<usize> = toposort(&g, None).map(|o| o.into_iter().map(|v| v.index()).collect()).unwrap_or_else(|_| (0..n).collect());
    let mut succs = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(a, b) in dag {
        succs[a].push(b);
        indeg[b] += 1;
    }
    let mut layer = vec![0usize; n];
    for &v in &order {
        for &t in &succs[v] {
            layer[t] = layer[t].max(layer[v] + 1);
        }
    }
    for &v in order.iter().rev() {
        if succs[v].len() > indeg[v] {
            if let Some(latest) = succs[v].iter().map(|&t| layer[t]).min().map(|m| m.saturating_sub(1)) {
                layer[v] = layer[v].max(latest);
            }
        }
    }
    let min = layer.iter().copied().min().unwrap_or(0);
    layer.iter().map(|l| l - min).collect()
}

/// A wire seen from one end: the node at the other end, this end's port
/// height and the other end's.
type End = (usize, f32, f32);

/// The layered graph: the real nodes plus a lane placeholder in every column
/// a wire passes through, with wires only between neighbouring columns.
struct LayerGraph {
    n_real: usize,
    height: Vec<f32>,
    preds: Vec<Vec<End>>,
    succs: Vec<Vec<End>>,
    /// Nodes in each column, top to bottom.
    cols: Vec<Vec<usize>>,
    /// Index of each node in its column.
    pos: Vec<usize>,
}

impl LayerGraph {
    fn new(n: usize, wires: &[(usize, usize, f32, f32)], layer: &[usize], height: impl Fn(usize) -> f32) -> Self {
        let mut height: Vec<f32> = (0..n).map(height).collect();
        let mut node_layer: Vec<usize> = layer.to_vec();
        let mut preds: Vec<Vec<End>> = vec![Vec::new(); n];
        let mut succs: Vec<Vec<End>> = vec![Vec::new(); n];
        for &(a, b, pa, pb) in wires {
            let (mut prev, mut prev_port) = (a, pa);
            for l in layer[a] + 1..layer[b] {
                let lane = height.len();
                height.push(0.0);
                node_layer.push(l);
                preds.push(vec![(prev, 0.0, prev_port)]);
                succs.push(Vec::new());
                succs[prev].push((lane, prev_port, 0.0));
                (prev, prev_port) = (lane, 0.0);
            }
            succs[prev].push((b, prev_port, pb));
            preds[b].push((prev, pb, prev_port));
        }
        let total = height.len();
        let n_cols = node_layer.iter().max().map_or(0, |m| m + 1);
        let mut cols = vec![Vec::new(); n_cols];
        for (v, &l) in node_layer.iter().enumerate() {
            cols[l].push(v);
        }
        let mut lg = LayerGraph { n_real: n, height, preds, succs, cols, pos: vec![0; total] };
        lg.index();
        lg
    }

    fn index(&mut self) {
        for col in &self.cols {
            for (i, &v) in col.iter().enumerate() {
                self.pos[v] = i;
            }
        }
    }

    /// Where a wire's end sits in its column: the node's place, plus a
    /// fraction for how far down the node its port is.
    fn place(&self, v: usize, port: f32) -> f32 {
        let h = self.height[v];
        self.pos[v] as f32 + if h > 0.0 { PORT_FRACTION * (port / h).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// Crossings between column `c` and the next.
    fn crossings_after(&self, c: usize) -> usize {
        let mut w: Vec<(f32, f32)> = self.cols[c].iter().flat_map(|&a| self.succs[a].iter().map(move |&(b, pa, pb)| (a, pa, b, pb))).map(|(a, pa, b, pb)| (self.place(a, pa), self.place(b, pb))).collect();
        w.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut count = 0;
        for i in 0..w.len() {
            for j in i + 1..w.len() {
                if w[i].0 < w[j].0 && w[i].1 > w[j].1 {
                    count += 1;
                }
            }
        }
        count
    }

    fn crossings(&self) -> usize {
        (0..self.cols.len().saturating_sub(1)).map(|c| self.crossings_after(c)).sum()
    }

    /// Sorts column `c` by the mean place of each node's neighbours on one
    /// side; nodes with none there keep their place.
    fn sort_by_barycentre(&mut self, c: usize, from_left: bool) {
        let mut keyed: Vec<(f32, usize)> = self.cols[c]
            .iter()
            .map(|&v| {
                let side = if from_left { &self.preds[v] } else { &self.succs[v] };
                let key = if side.is_empty() {
                    self.pos[v] as f32
                } else {
                    side.iter().map(|&(u, _, port)| self.place(u, port)).sum::<f32>() / side.len() as f32
                };
                (key, v)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.cols[c] = keyed.into_iter().map(|k| k.1).collect();
        for (i, &v) in self.cols[c].iter().enumerate() {
            self.pos[v] = i;
        }
    }

    /// Crossings among the wires of `u` and `v` (side by side, `u` above)
    /// with the columns either side.
    fn pair_crossings(&self, u: usize, v: usize) -> usize {
        let mut count = 0;
        for side in [&self.preds, &self.succs] {
            for &(a, _, pa) in &side[u] {
                for &(b, _, pb) in &side[v] {
                    if self.place(a, pa) > self.place(b, pb) {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// Swaps neighbours in a column while that removes crossings.
    fn transpose(&mut self) {
        for _ in 0..8 {
            let mut better = false;
            for c in 0..self.cols.len() {
                for i in 0..self.cols[c].len().saturating_sub(1) {
                    let (u, v) = (self.cols[c][i], self.cols[c][i + 1]);
                    if self.pair_crossings(v, u) < self.pair_crossings(u, v) {
                        self.cols[c].swap(i, i + 1);
                        self.pos[u] = i + 1;
                        self.pos[v] = i;
                        better = true;
                    }
                }
            }
            if !better {
                break;
            }
        }
    }

    /// Orders every column to cut down on crossing wires.
    fn order(&mut self) {
        let n_cols = self.cols.len();
        let mut best = (self.crossings(), self.cols.clone());
        for _ in 0..SWEEPS {
            if best.0 == 0 {
                break;
            }
            for c in 1..n_cols {
                self.sort_by_barycentre(c, true);
            }
            for c in (0..n_cols.saturating_sub(1)).rev() {
                self.sort_by_barycentre(c, false);
            }
            let now = self.crossings();
            if now < best.0 {
                best = (now, self.cols.clone());
            }
        }
        self.cols = best.1;
        self.index();
        self.transpose();
    }

    /// Space between two neighbours in a column.
    fn gap(&self, a: usize, b: usize) -> f32 {
        if a >= self.n_real || b >= self.n_real {
            LANE_GAP
        } else {
            GAP_Y
        }
    }

    /// Top of every real node: its ports level with the ports they're wired
    /// to, in column order, without overlapping.
    fn rows(&self) -> Vec<f32> {
        let total = self.height.len();
        let mut y = vec![0f32; total];
        let tallest = self.cols.iter().map(|col| self.stack(col)).fold(0.0, f32::max);
        for col in &self.cols {
            let mut at = (tallest - self.stack(col)) / 2.0;
            for (i, &v) in col.iter().enumerate() {
                y[v] = at;
                at += self.height[v] + col.get(i + 1).map_or(0.0, |&w| self.gap(v, w));
            }
        }
        for round in 0..ROUNDS {
            // Alternate sides; the last rounds look both ways.
            let (left, right) = match round % 2 {
                _ if round + 4 >= ROUNDS => (true, true),
                0 => (true, false),
                _ => (false, true),
            };
            let cols: Vec<usize> = if round % 2 == 0 { (0..self.cols.len()).collect() } else { (0..self.cols.len()).rev().collect() };
            for c in cols {
                let col = &self.cols[c];
                let mut target = Vec::with_capacity(col.len());
                let mut weight = Vec::with_capacity(col.len());
                for &v in col {
                    let mut ends: Vec<End> = Vec::new();
                    if left {
                        ends.extend(&self.preds[v]);
                    }
                    if right {
                        ends.extend(&self.succs[v]);
                    }
                    if ends.is_empty() {
                        target.push(y[v]);
                        weight.push(0.01);
                    } else {
                        // Where this node's top goes for each wire to run level.
                        let mean = ends.iter().map(|&(u, mine, theirs)| y[u] + theirs - mine).sum::<f32>() / ends.len() as f32;
                        target.push(mean);
                        // Lanes count more: long wires stay straight.
                        let lane = if v >= self.n_real { 4.0 } else { 1.0 };
                        weight.push(ends.len() as f32 * lane);
                    }
                }
                let fitted = self.fit(col, &target, &weight);
                for (&v, f) in col.iter().zip(fitted) {
                    y[v] = f;
                }
            }
        }
        let top = y.iter().copied().fold(f32::INFINITY, f32::min);
        y.truncate(self.n_real);
        y.iter().map(|v| v - top + 40.0).collect()
    }

    fn stack(&self, col: &[usize]) -> f32 {
        let h: f32 = col.iter().map(|&v| self.height[v]).sum();
        h + col.windows(2).map(|w| self.gap(w[0], w[1])).sum::<f32>()
    }

    /// The tops closest to `target` (weighted least squares) that keep the
    /// column's order without overlaps: with each node's minimum offset
    /// from the first taken out, that's an isotonic regression, solved
    /// exactly by pool-adjacent-violators.
    fn fit(&self, col: &[usize], target: &[f32], weight: &[f32]) -> Vec<f32> {
        let mut offset = vec![0f32; col.len()];
        for i in 1..col.len() {
            offset[i] = offset[i - 1] + self.height[col[i - 1]] + self.gap(col[i - 1], col[i]);
        }
        // Blocks of (weighted mean, weight, count).
        let mut blocks: Vec<(f32, f32, usize)> = Vec::new();
        for i in 0..col.len() {
            blocks.push((target[i] - offset[i], weight[i], 1));
            while blocks.len() > 1 && blocks[blocks.len() - 2].0 > blocks[blocks.len() - 1].0 {
                let (m2, w2, c2) = blocks.pop().unwrap();
                let (m1, w1, c1) = blocks.pop().unwrap();
                blocks.push(((m1 * w1 + m2 * w2) / (w1 + w2), w1 + w2, c1 + c2));
            }
        }
        let mut out = Vec::with_capacity(col.len());
        for (m, _, c) in blocks {
            for _ in 0..c {
                out.push(m + offset[out.len()]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wires (as straight segments between node centres) that cross.
    fn crossings(p: &[[f32; 2]], heights: &[f32], edges: &[(usize, usize)]) -> usize {
        let seg = |&(a, b): &(usize, usize)| ([p[a][0], p[a][1] + heights[a] / 2.0], [p[b][0], p[b][1] + heights[b] / 2.0]);
        let ccw = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        let mut count = 0;
        for i in 0..edges.len() {
            for j in i + 1..edges.len() {
                let (e, f) = (edges[i], edges[j]);
                if e.0 == f.0 || e.1 == f.1 || e.0 == f.1 || e.1 == f.0 {
                    continue;
                }
                let ((a, b), (c, d)) = (seg(&e), seg(&f));
                if ccw(a, b, c) * ccw(a, b, d) < 0.0 && ccw(c, d, a) * ccw(c, d, b) < 0.0 {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn narrow_nodes_get_narrow_columns() {
        // A wide node, a small one, a wide one.
        let wires = [(0, 1, 50.0, 18.0), (1, 2, 18.0, 50.0)];
        let p = layered_ports_widths(3, &wires, &[300.0, 36.0, 300.0], &[100.0, 36.0, 100.0]);
        assert_eq!(p[0][0], 40.0);
        assert_eq!(p[1][0], 40.0 + 300.0 + GAP_X, "right after the first column");
        assert_eq!(p[2][0], 40.0 + 300.0 + GAP_X + 36.0 + GAP_X, "the small column is only as wide as its node");
    }

    #[test]
    fn chain_goes_left_to_right() {
        let p = layered(3, &[(0, 1), (1, 2)]);
        assert!(p[0][0] < p[1][0] && p[1][0] < p[2][0]);
    }

    #[test]
    fn a_chain_is_level() {
        let p = layered(4, &[(0, 1), (1, 2), (2, 3)]);
        assert!(p.iter().all(|q| (q[1] - p[0][1]).abs() < 0.5), "{p:?}");
    }

    #[test]
    fn ports_line_up() {
        // 0's port is 150 down, 1's is 30 down: 1 sits 120 lower, so the
        // wire is level.
        let p = layered_ports(2, &[(0, 1, 150.0, 30.0)], 300.0, &[200.0, 100.0]);
        assert!((p[0][1] + 150.0 - (p[1][1] + 30.0)).abs() < 0.5, "{p:?}");
    }

    #[test]
    fn tall_nodes_do_not_overlap() {
        // Three sources feeding one machine: the sources share a column.
        let heights = [300.0, 100.0, 250.0, 120.0];
        let p = layered_sized(4, &[(0, 3), (1, 3), (2, 3)], 300.0, &heights);
        let mut col: Vec<usize> = vec![0, 1, 2];
        col.sort_by(|a, b| p[*a][1].total_cmp(&p[*b][1]));
        for w in col.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(p[a][1] + heights[a] + GAP_Y <= p[b][1] + 1e-3, "{a} overlaps {b}: {p:?}");
        }
    }

    #[test]
    fn a_machine_sits_level_with_what_feeds_it() {
        // Two sources into one machine: it's centred between them.
        let heights = [100.0, 100.0, 100.0];
        let p = layered_sized(3, &[(0, 2), (1, 2)], 300.0, &heights);
        let mid = (p[0][1] + p[1][1]) / 2.0;
        assert!((p[2][1] - mid).abs() < 1.0, "{p:?}");
    }

    #[test]
    fn crossings_are_untangled() {
        // Two parallel chains given crossed: 0->3, 1->2 with 2 above 3 in
        // input order, then each on to its own output.
        let edges = [(0, 3), (1, 2), (3, 4), (2, 5)];
        let heights = [100.0; 6];
        let p = layered_sized(6, &edges, 300.0, &heights);
        assert_eq!(crossings(&p, &heights, &edges), 0, "{p:?}");
    }

    #[test]
    fn a_long_wire_gets_a_lane() {
        // 0 feeds 1 -> 2 -> 3 and also 3 directly; 4 feeds 1. The long
        // wire 0 -> 3 mustn't run through a node.
        let edges = [(0, 1), (1, 2), (2, 3), (0, 3), (4, 1), (5, 2)];
        let heights = [100.0; 6];
        let p = layered_sized(6, &edges, 300.0, &heights);
        let (a, b) = ([p[0][0], p[0][1] + 50.0], [p[3][0], p[3][1] + 50.0]);
        for v in [1, 2, 4, 5] {
            let x = p[v][0];
            if x <= a[0] || x >= b[0] {
                continue;
            }
            let y = a[1] + (b[1] - a[1]) * (x - a[0]) / (b[0] - a[0]);
            assert!(y < p[v][1] || y > p[v][1] + heights[v], "the wire 0->3 runs through {v}: {p:?}");
        }
    }

    #[test]
    fn cycles_do_not_panic() {
        let p = layered(3, &[(0, 1), (1, 2), (2, 1)]);
        assert_eq!(p.len(), 3);
        let p = layered(4, &[(0, 1), (1, 2), (2, 3), (3, 1), (2, 1)]);
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn nothing_overlaps_on_a_bigger_graph() {
        // A small web: 3 sources, a few machines sharing products.
        let edges = [(0, 3), (1, 3), (1, 4), (2, 4), (3, 5), (4, 5), (4, 6), (3, 7), (5, 7), (6, 7), (0, 6)];
        let heights = [80.0, 120.0, 90.0, 200.0, 150.0, 180.0, 100.0, 60.0];
        let p = layered_sized(8, &edges, 300.0, &heights);
        for i in 0..8 {
            for j in i + 1..8 {
                if p[i][0] == p[j][0] {
                    let (a, b) = if p[i][1] < p[j][1] { (i, j) } else { (j, i) };
                    assert!(p[a][1] + heights[a] + GAP_Y <= p[b][1] + 1e-3, "{a} overlaps {b}: {p:?}");
                }
            }
        }
    }
}
