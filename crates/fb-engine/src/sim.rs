//! Steady-state flow simulation of a hand-built factory graph.
//!
//! Each node has input ports (what it wants per minute at full speed) and
//! output ports (what it makes per minute at full speed). Links join an output
//! to an input of the same item. Ports joined by links form a "manifold" (a
//! connected component, found with petgraph's union-find).
//!
//! Every node gets a running speed t in [0, 1]. In steady state each manifold
//! must balance: supply == pull, or supply >= pull when a sink takes the rest.
//! Among all speeds that balance, the simulation picks the max-min fair one
//! (progressive filling with good_lp): raise everyone together, freeze nodes
//! that hit a bottleneck, keep raising the rest. That matches how belts and
//! splitters share in game, and it handles recycling loops, which an
//! iterative "slow down when starved" scheme ratchets down to zero.

use good_lp::{microlp, variable, Expression, ProblemVariables, Solution, SolverModel, Variable};
use petgraph::unionfind::UnionFind;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimNode {
    /// Demand per input port at 100% speed.
    pub inputs: Vec<f64>,
    /// Supply per output port at 100% speed.
    pub outputs: Vec<f64>,
    /// Machines; sources and outputs also slow (back up / go short) but
    /// that's reported differently by the UI.
    pub throttles: bool,
    /// Takes any amount on its inputs (AWESOME Sink / storage).
    pub sink: bool,
    /// Most per minute each input / output port carries (its belt or pipe);
    /// missing or infinite: no limit.
    pub in_caps: Vec<f64>,
    pub out_caps: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimLink {
    pub from_node: usize,
    pub from_port: usize,
    pub to_node: usize,
    pub to_port: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SimResult {
    /// 0..=1 running speed of each node.
    pub efficiency: Vec<f64>,
    /// Per node, per input port: amount received per minute.
    pub received: Vec<Vec<f64>>,
    /// Per node, per output port: amount shipped per minute.
    pub shipped: Vec<Vec<f64>>,
    /// Per link: flow per minute.
    pub link_flow: Vec<f64>,
}

const TOL: f64 = 1e-7;
/// Relative imbalance a manifold may have and still count as balanced.
const BALANCE_TOLERANCE: f64 = 1e-5;

struct Manifold {
    supply: Vec<(usize, f64)>, // (node, rate at 100%)
    pull: Vec<(usize, f64)>,
    has_sink: bool,
}

pub fn simulate(nodes: &[SimNode], links: &[SimLink]) -> SimResult {
    let mut in_base = Vec::with_capacity(nodes.len());
    let mut out_base = Vec::with_capacity(nodes.len());
    let mut count = 0;
    for n in nodes {
        in_base.push(count);
        count += n.inputs.len();
        out_base.push(count);
        count += n.outputs.len();
    }
    let links: Vec<(usize, SimLink)> = links
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, l)| {
            l.from_node < nodes.len()
                && l.to_node < nodes.len()
                && l.from_port < nodes[l.from_node].outputs.len()
                && l.to_port < nodes[l.to_node].inputs.len()
        })
        .collect();
    let out_port = |l: &SimLink| out_base[l.from_node] + l.from_port;
    let in_port = |l: &SimLink| in_base[l.to_node] + l.to_port;

    let mut uf = UnionFind::<usize>::new(count.max(1));
    let mut connected = vec![false; count];
    for (_, l) in &links {
        uf.union(out_port(l), in_port(l));
        connected[out_port(l)] = true;
        connected[in_port(l)] = true;
    }

    let mut manifolds: BTreeMap<usize, Manifold> = BTreeMap::new();
    let mut blocked = vec![false; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        for (p, &d) in n.inputs.iter().enumerate() {
            let port = in_base[i] + p;
            if !connected[port] {
                if d > TOL && !n.sink {
                    blocked[i] = true; // a machine with an empty input can't run
                }
                continue;
            }
            let m = manifolds.entry(uf.find(port)).or_insert_with(|| Manifold { supply: vec![], pull: vec![], has_sink: false });
            if n.sink {
                m.has_sink = true;
            } else if d > TOL {
                m.pull.push((i, d));
            }
        }
        for (p, &s) in n.outputs.iter().enumerate() {
            let port = out_base[i] + p;
            // Unconnected outputs are flagged by the UI rather than stalling
            // the machine, so partial plans stay readable.
            if connected[port] && s > TOL {
                let m = manifolds.entry(uf.find(port)).or_insert_with(|| Manifold { supply: vec![], pull: vec![], has_sink: false });
                m.supply.push((i, s));
            }
        }
    }

    let speed = progressive_fill(nodes, &manifolds, &blocked);

    // Flows at those speeds.
    let mut res = SimResult {
        efficiency: speed.clone(),
        received: nodes.iter().map(|n| vec![0.0; n.inputs.len()]).collect(),
        shipped: nodes.iter().map(|n| vec![0.0; n.outputs.len()]).collect(),
        link_flow: vec![0.0; links.iter().map(|(k, _)| k + 1).max().unwrap_or(0)],
    };
    let totals: BTreeMap<usize, (f64, f64)> = manifolds
        .iter()
        .map(|(&root, m)| {
            let s: f64 = m.supply.iter().map(|&(i, r)| r * speed[i]).sum();
            let p: f64 = m.pull.iter().map(|&(i, r)| r * speed[i]).sum();
            (root, (s, p))
        })
        .collect();
    for (i, n) in nodes.iter().enumerate() {
        for (p, &d) in n.inputs.iter().enumerate() {
            let port = in_base[i] + p;
            if !connected[port] {
                continue;
            }
            let (s, pull) = totals[&uf.find(port)];
            res.received[i][p] = if n.sink { (s - pull).max(0.0) } else { d * speed[i] };
        }
        for (p, &s) in n.outputs.iter().enumerate() {
            let port = out_base[i] + p;
            if connected[port] {
                // Without a sink, what's shipped is what's taken: the hair
                // of slack the balance allows (see BALANCE_TOLERANCE) isn't
                // reported as more going out than comes in (5.0007 for 5).
                let root = uf.find(port);
                let (supply, pull) = totals.get(&root).copied().unwrap_or((0.0, 0.0));
                let sink = manifolds.get(&root).is_some_and(|m| m.has_sink);
                let fit = if !sink && supply > pull && supply > 0.0 { pull / supply } else { 1.0 };
                res.shipped[i][p] = s * speed[i] * fit;
            }
        }
    }
    // Each output's shipment is shared over its own links by what the
    // receiving ports take.
    for (k, l) in &links {
        let siblings: f64 = links
            .iter()
            .filter(|(_, m)| out_port(m) == out_port(l))
            .map(|(_, m)| res.received[m.to_node][m.to_port])
            .sum();
        let share = if siblings > 1e-12 { res.received[l.to_node][l.to_port] / siblings } else { 0.0 };
        res.link_flow[*k] = res.shipped[l.from_node][l.from_port] * share;
    }
    res
}

/// Max-min fair speeds: repeatedly maximise the common floor z of all
/// unfrozen nodes, then freeze the nodes that can't go above z.
fn progressive_fill(nodes: &[SimNode], manifolds: &BTreeMap<usize, Manifold>, blocked: &[bool]) -> Vec<f64> {
    let n = nodes.len();
    let mut fixed: Vec<Option<f64>> = (0..n).map(|i| if blocked[i] { Some(0.0) } else { None }).collect();
    // Nodes in no manifold at all (and not blocked) just run.
    let mut involved = vec![false; n];
    for m in manifolds.values() {
        for &(i, _) in m.supply.iter().chain(&m.pull) {
            involved[i] = true;
        }
    }
    for i in 0..n {
        if fixed[i].is_none() && !involved[i] {
            fixed[i] = Some(1.0);
        }
    }

    for _round in 0..n + 1 {
        let free: Vec<usize> = (0..n).filter(|&i| fixed[i].is_none()).collect();
        if free.is_empty() {
            break;
        }
        // Highest common floor for the free nodes.
        let Some(z) = solve_floor(nodes, n, manifolds, &fixed, &free, None) else { break };
        if z >= 1.0 - TOL {
            for i in free {
                fixed[i] = Some(1.0);
            }
            break;
        }
        // Which free nodes can individually rise above z?
        let mut froze_any = false;
        let mut stuck = Vec::new();
        for &i in &free {
            match solve_floor(nodes, n, manifolds, &fixed, &free, Some((i, z))) {
                Some(best) if best > z + 1e-6 => {}
                _ => stuck.push(i),
            }
        }
        for i in stuck {
            fixed[i] = Some(z);
            froze_any = true;
        }
        if !froze_any {
            for i in free {
                fixed[i] = Some(z);
            }
            break;
        }
    }
    fixed.into_iter().map(|v| v.unwrap_or(0.0).clamp(0.0, 1.0)).collect()
}

/// With `probe = None`: maximise z such that every free node runs at >= z.
/// With `probe = Some((i, z))`: keep every free node at >= z and maximise t_i.
fn solve_floor(
    nodes: &[SimNode],
    n: usize,
    manifolds: &BTreeMap<usize, Manifold>,
    fixed: &[Option<f64>],
    free: &[usize],
    probe: Option<(usize, f64)>,
) -> Option<f64> {
    let mut vars = ProblemVariables::new();
    let t: Vec<Variable> = (0..n)
        .map(|i| match fixed[i] {
            // Tiny slack keeps frozen values feasible despite rounding.
            Some(v) => vars.add(variable().min((v - 1e-9).max(0.0)).max((v + 1e-9).min(1.0))),
            None => vars.add(variable().min(0.0).max(1.0)),
        })
        .collect();
    let z = vars.add(variable().min(0.0).max(1.0));
    let objective: Expression = match probe {
        None => z.into(),
        Some((i, _)) => t[i].into(),
    };
    let mut problem = vars.maximise(objective).using(microlp);
    for m in manifolds.values() {
        let mut bal = Expression::default();
        for &(i, r) in &m.supply {
            bal.add_mul(r, t[i]);
        }
        for &(i, r) in &m.pull {
            bal.add_mul(-r, t[i]);
        }
        // Consumers never get more than is supplied (bal >= 0). Without a
        // sink, leftovers may be at most a hair of the manifold's throughput.
        // Exact equality is too strict: plans round machine counts, and in
        // recycling loops the balance equations are linearly dependent, so a
        // 1e-6 rounding error can leave t = 0 as the only exact answer.
        let scale: f64 = m.supply.iter().chain(&m.pull).map(|&(_, r)| r).sum::<f64>().max(1.0);
        let slack = BALANCE_TOLERANCE * scale;
        problem.add_constraint(bal.clone().geq(0.0));
        if !m.has_sink {
            problem.add_constraint(bal.leq(slack));
        }
    }
    for &i in free {
        match probe {
            None => problem.add_constraint((t[i] - z).geq(0.0)),
            Some((_, floor)) => problem.add_constraint(Expression::from(t[i]).geq(floor - 1e-9)),
        };
    }
    // A port's belt or pipe carries at most its speed: rate x t <= cap.
    for (i, n) in nodes.iter().enumerate() {
        let caps = n.inputs.iter().zip(n.in_caps.iter()).chain(n.outputs.iter().zip(n.out_caps.iter()));
        for (&rate, &cap) in caps {
            if rate > TOL && cap.is_finite() {
                problem.add_constraint((rate * t[i]).leq(cap.max(0.0)));
            }
        }
    }
    let sol = problem.solve().ok()?;
    Some(match probe {
        None => sol.value(z),
        Some((i, _)) => sol.value(t[i]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(rate: f64) -> SimNode {
        SimNode { outputs: vec![rate], ..Default::default() }
    }
    fn machine(input: f64, output: f64) -> SimNode {
        SimNode { inputs: vec![input], outputs: vec![output], throttles: true, ..Default::default() }
    }
    fn out(rate: f64) -> SimNode {
        SimNode { inputs: vec![rate], ..Default::default() }
    }
    fn link(a: usize, b: usize) -> SimLink {
        SimLink { from_node: a, from_port: 0, to_node: b, to_port: 0 }
    }
    /// Speeds and rates are exact to about BALANCE_TOLERANCE (relative).
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-4 * b.abs().max(1.0)
    }

    #[test]
    fn balanced_chain_runs_full() {
        let r = simulate(&[src(30.0), machine(30.0, 30.0), out(30.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.efficiency[1], 1.0));
        assert!(close(r.received[2][0], 30.0));
        assert!(close(r.link_flow[1], 30.0));
    }

    #[test]
    fn starved_machine_slows() {
        let r = simulate(&[src(15.0), machine(30.0, 20.0), out(20.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.efficiency[1], 0.5), "{:?}", r.efficiency);
        assert!(close(r.received[2][0], 10.0));
    }

    #[test]
    fn backed_up_machine_slows() {
        let r = simulate(&[src(30.0), machine(30.0, 30.0), out(10.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.efficiency[1], 1.0 / 3.0), "{:?}", r.efficiency);
        assert!(close(r.received[1][0], 10.0));
    }

    #[test]
    fn sink_absorbs_overflow() {
        let sink = SimNode { inputs: vec![0.0], sink: true, ..Default::default() };
        let r = simulate(&[src(30.0), machine(30.0, 30.0), out(10.0), sink], &[link(0, 1), link(1, 2), link(1, 3)]);
        assert!(close(r.efficiency[1], 1.0));
        assert!(close(r.received[3][0], 20.0));
        assert!(close(r.link_flow[1] + r.link_flow[2], 30.0));
    }

    #[test]
    fn two_consumers_share_and_one_recovers() {
        // A machine limited by its other input leaves the rest of the shared
        // belt to its neighbour, like a splitter backing up in game.
        let starved = SimNode { inputs: vec![10.0, 10.0], outputs: vec![], throttles: true, ..Default::default() };
        let r = simulate(
            &[src(10.0), machine(10.0, 10.0), starved, src(1.0), out(10.0)],
            &[link(0, 1), link(0, 2), SimLink { from_node: 3, from_port: 0, to_node: 2, to_port: 1 }, link(1, 4)],
        );
        assert!(close(r.efficiency[2], 0.1), "{:?}", r.efficiency);
        assert!(close(r.efficiency[1], 0.9), "{:?}", r.efficiency);
    }

    #[test]
    fn a_slow_belt_holds_a_machine_back() {
        // A source of 120 into a machine taking 120, on a belt of 60.
        let mut m = machine(120.0, 120.0);
        m.in_caps = vec![60.0];
        let r = simulate(&[src(120.0), m, out(120.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.efficiency[1], 0.5), "{:?}", r.efficiency);
        assert!(close(r.received[1][0], 60.0));
        assert!(close(r.received[2][0], 60.0));
    }

    #[test]
    fn unconnected_input_stops_machine() {
        let r = simulate(&[machine(30.0, 30.0)], &[]);
        assert!(close(r.efficiency[0], 0.0));
    }

    #[test]
    fn recycling_loop_runs_full() {
        // A <-> B loop: A makes 10 x, needs 5 y + 10 ore; B makes 5 y + 10 out, needs 10 x.
        let a = SimNode { inputs: vec![5.0, 10.0], outputs: vec![10.0], throttles: true, ..Default::default() };
        let b = SimNode { inputs: vec![10.0], outputs: vec![5.0, 10.0], throttles: true, ..Default::default() };
        let nodes = [a, b, src(10.0), out(10.0)];
        let links = [
            SimLink { from_node: 0, from_port: 0, to_node: 1, to_port: 0 },
            SimLink { from_node: 1, from_port: 0, to_node: 0, to_port: 0 },
            SimLink { from_node: 2, from_port: 0, to_node: 0, to_port: 1 },
            SimLink { from_node: 1, from_port: 1, to_node: 3, to_port: 0 },
        ];
        let r = simulate(&nodes, &links);
        assert!(r.efficiency.iter().all(|e| close(*e, 1.0)), "{:?}", r.efficiency);
    }
}
