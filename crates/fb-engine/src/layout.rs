//! Layered (left-to-right) layout for production graphs, built on petgraph.
//!
//! 1. Break cycles (recycling loops) with petgraph's greedy feedback arc set.
//! 2. Longest-path layering on the remaining DAG: raw inputs on the left,
//!    final products on the right.
//! 3. Order nodes in each layer by the barycentre of their predecessors to
//!    cut down on crossing wires.

use petgraph::algo::{greedy_feedback_arc_set, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use std::collections::HashSet;

pub const COLUMN_WIDTH: f32 = 300.0;
pub const ROW_HEIGHT: f32 = 230.0;
/// Space between columns and between stacked nodes.
pub const GAP_X: f32 = 90.0;
pub const GAP_Y: f32 = 40.0;

/// Returns a canvas position for each of `n` nodes, all assumed the same size.
pub fn layered(n: usize, edges: &[(usize, usize)]) -> Vec<[f32; 2]> {
    layered_sized(n, edges, COLUMN_WIDTH, &vec![ROW_HEIGHT - GAP_Y; n])
}

/// Like [`layered`], but stacks nodes by their real heights so tall nodes
/// don't overlap; `width` is the node width.
pub fn layered_sized(n: usize, edges: &[(usize, usize)], width: f32, heights: &[f32]) -> Vec<[f32; 2]> {
    if n == 0 {
        return Vec::new();
    }
    let mut g: DiGraph<(), ()> = DiGraph::with_capacity(n, edges.len());
    for _ in 0..n {
        g.add_node(());
    }
    let mut seen = HashSet::new();
    for &(a, b) in edges {
        if a != b && seen.insert((a, b)) {
            g.add_edge(NodeIndex::new(a), NodeIndex::new(b), ());
        }
    }

    // Drop back edges so the rest is acyclic.
    let back: Vec<_> = greedy_feedback_arc_set(&g).map(|e| e.id()).collect();
    let mut dag = g.clone();
    // Remove highest index first so the remaining edge ids stay valid.
    let mut back_sorted = back;
    back_sorted.sort_by_key(|e| std::cmp::Reverse(e.index()));
    for e in back_sorted {
        dag.remove_edge(e);
    }
    let order = toposort(&dag, None).unwrap_or_else(|_| dag.node_indices().collect());

    // Longest path from any source.
    let mut layer = vec![0usize; n];
    for &v in &order {
        for e in dag.edges_directed(v, Direction::Outgoing) {
            let t = e.target().index();
            layer[t] = layer[t].max(layer[v.index()] + 1);
        }
    }
    // Pull sources (resources, inputs) right next to their first consumer
    // instead of leaving them all in column 0.
    for &v in order.iter().rev() {
        let i = v.index();
        if dag.edges_directed(v, Direction::Incoming).next().is_none() {
            if let Some(min_t) = dag.edges_directed(v, Direction::Outgoing).map(|e| layer[e.target().index()]).min() {
                layer[i] = min_t.saturating_sub(1);
            }
        }
    }

    let max_layer = *layer.iter().max().unwrap_or(&0);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); max_layer + 1];
    for &v in &order {
        columns[layer[v.index()]].push(v.index());
    }
    let mut row = vec![0f32; n];
    for col in columns.iter_mut() {
        let mut keyed: Vec<(f32, usize)> = col
            .iter()
            .map(|&i| {
                let preds: Vec<f32> = dag
                    .edges_directed(NodeIndex::new(i), Direction::Incoming)
                    .map(|e| row[e.source().index()])
                    .collect();
                let key = if preds.is_empty() { i as f32 * 1e-3 } else { preds.iter().sum::<f32>() / preds.len() as f32 };
                (key, i)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        *col = keyed.into_iter().map(|k| k.1).collect();
        for (r, &i) in col.iter().enumerate() {
            row[i] = r as f32;
        }
    }

    let height = |i: usize| heights.get(i).copied().unwrap_or(ROW_HEIGHT - GAP_Y);
    let col_height = |col: &Vec<usize>| col.iter().map(|&i| height(i) + GAP_Y).sum::<f32>() - GAP_Y;
    let tallest = columns.iter().map(col_height).fold(0.0, f32::max);
    let mut pos = vec![[0f32; 2]; n];
    for (c, col) in columns.iter().enumerate() {
        // Centre shorter columns vertically.
        let mut y = 40.0 + (tallest - col_height(col)) / 2.0;
        for &i in col {
            pos[i] = [40.0 + c as f32 * (width + GAP_X), y];
            y += height(i) + GAP_Y;
        }
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_goes_left_to_right() {
        let p = layered(3, &[(0, 1), (1, 2)]);
        assert!(p[0][0] < p[1][0] && p[1][0] < p[2][0]);
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
    fn cycles_do_not_panic() {
        let p = layered(3, &[(0, 1), (1, 2), (2, 1)]);
        assert_eq!(p.len(), 3);
    }
}
