//! Steady-state flow simulation of a hand-built factory graph.
//!
//! Each node has input ports (what it wants per minute at full speed) and
//! output ports (what it makes per minute at full speed). Links join an
//! output to an input of the same item, and each carries its own flow, up to
//! its belt's speed. Routers (mergers, splitters, valves and the like) pass
//! on what they get, sharing it as their building does in game.
//!
//! Every node gets a running speed t in [0, 1]. A machine's inputs take
//! exactly what it uses at its speed and its outputs ship what it makes; a
//! sink (or storage, or an output) takes whatever arrives.
//!
//! Where several links share a port or a router, they share the way belts do
//! in game, by progressive filling (with good_lp): raise every sharing link's
//! flow together, freeze those that hit a limit, keep raising the rest. So a
//! splitter gives each output an equal share and the share of a backed-up
//! output to the others; a merger takes equally from its inputs. Ranks order
//! links within a share: an overflow output gets only what the others can't
//! take, a priority merger's low inputs only what its high ones don't fill.
//! A link waits only for better-ranked links of its own shares.
//!
//! Then the machines' speeds are filled the same way (max-min fair), which
//! handles chains and recycling loops, where an iterative "slow down when
//! starved" scheme ratchets down to zero.

use good_lp::{microlp, variable, Expression, ProblemVariables, Solution, SolverModel, Variable};
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
    /// A merger, splitter, valve or the like: what comes in goes out, shared
    /// as `Share` says. Its `inputs` and `outputs` only count its ports.
    pub router: Option<Share>,
    /// Storage: takes whatever arrives and passes on up to that, filling
    /// with the rest; with nothing wired in, it hands out what it holds
    /// (whatever is taken). Its rates only count its ports.
    pub store: bool,
}

impl SimNode {
    /// A router with `ins` input and `outs` output ports.
    pub fn router(ins: usize, outs: usize, share: Share) -> Self {
        SimNode { inputs: vec![0.0; ins], outputs: vec![0.0; outs], router: Some(share), ..Default::default() }
    }
}

/// How a router shares what goes through it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Share {
    /// Per input port, its rank: 0 is taken from first (a Priority Merger's
    /// High). Missing ports are 0.
    pub in_rank: Vec<u8>,
    /// Per output port: None lets nothing through; Some(rank), 0 first (a
    /// Smart Splitter's overflow output is 1). Missing ports are Some(0).
    pub out_rank: Vec<Option<u8>>,
    /// Most per minute through it (a Valve's limit).
    pub limit: Option<f64>,
}

impl Share {
    fn in_rank(&self, p: usize) -> u8 {
        self.in_rank.get(p).copied().unwrap_or(0)
    }
    fn out_rank(&self, p: usize) -> Option<u8> {
        self.out_rank.get(p).copied().unwrap_or(Some(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimLink {
    pub from_node: usize,
    pub from_port: usize,
    pub to_node: usize,
    pub to_port: usize,
    /// Most per minute it carries (its belt or pipe); infinite: no limit.
    pub cap: f64,
    /// Takes only what the other links from its port don't (a factory's own
    /// storage on the master graph).
    pub overflow: bool,
}

impl SimLink {
    pub fn new(from_node: usize, from_port: usize, to_node: usize, to_port: usize) -> Self {
        SimLink { from_node, from_port, to_node, to_port, cap: f64::INFINITY, overflow: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SimResult {
    /// 0..=1 running speed of each node (routers: 1).
    pub efficiency: Vec<f64>,
    /// Per node, per input port: amount received per minute.
    pub received: Vec<Vec<f64>>,
    /// Per node, per output port: amount shipped per minute.
    pub shipped: Vec<Vec<f64>>,
    /// Per link: flow per minute.
    pub link_flow: Vec<f64>,
}

const TOL: f64 = 1e-7;
/// Relative hair a machine may make beyond what it ships: plans round
/// machine counts, and in recycling loops the balance equations are linearly
/// dependent, so a 1e-6 rounding error could leave t = 0 as the only exact
/// answer.
const BALANCE_TOLERANCE: f64 = 1e-5;

pub fn simulate(nodes: &[SimNode], links: &[SimLink]) -> SimResult {
    let m = Model::new(nodes, links);
    let mut b = Bounds {
        t: nodes.iter().enumerate().map(|(i, _)| if m.blocked[i] { (0.0, 0.0) } else { (0.0, 1.0) }).collect(),
        f: vec![(0.0, f64::INFINITY); m.links.len()],
    };
    m.fill_shares(&mut b);
    m.fill_speeds(&mut b);
    // The flows at those speeds: all that's made shipped, and nothing going
    // round through routers for nothing.
    // Should the shares' flows not fit exactly, the speeds decide alone; and
    // as a last resort the speeds get more slack, rather than no answer.
    let loose = Bounds { f: vec![(0.0, f64::INFINITY); m.links.len()], ..b.clone() };
    let looser = Bounds { t: b.t.iter().map(|(lo, hi)| ((lo - 1e-5).max(0.0), (hi + 1e-5).min(1.0))).collect(), ..loose.clone() };
    let solved = m.solve(&b, Goal::Ship).or_else(|| m.solve(&loose, Goal::Ship)).or_else(|| m.solve(&looser, Goal::Ship));
    let (t, f) = match solved {
        Some(s) => (s.t, s.f.into_iter().map(|v| v.max(0.0)).collect::<Vec<f64>>()),
        None => (b.t.iter().map(|r| r.0).collect(), vec![0.0; m.links.len()]),
    };
    let mut res = SimResult {
        efficiency: t.iter().zip(nodes).map(|(v, n)| if n.router.is_some() { 1.0 } else { v.clamp(0.0, 1.0) }).collect(),
        received: m.ins.iter().map(|ps| ps.iter().map(|ks| ks.iter().map(|k| f[*k]).sum()).collect()).collect(),
        shipped: m.outs.iter().map(|ps| ps.iter().map(|ks| ks.iter().map(|k| f[*k]).sum()).collect()).collect(),
        link_flow: vec![0.0; links.len()],
    };
    for (k, (orig, _)) in m.links.iter().enumerate() {
        res.link_flow[*orig] = f[k];
    }
    res
}

/// Which nodes run below full speed only because what's after them takes
/// no more (their belts back up, as in game), not for want of anything
/// coming in. An input is short when every supplier on it (followed back
/// through routers and storage) runs flat out or is short itself, or when a
/// belt or valve on the way is full. Stocked storage (nothing wired in)
/// never runs short.
pub fn held_back(nodes: &[SimNode], links: &[SimLink], r: &SimResult) -> Vec<bool> {
    let m = Model::new(nodes, links);
    let full = |i: usize| r.efficiency.get(i).copied().unwrap_or(1.0) >= 0.995;
    let at_cap = |k: usize| {
        let (orig, l) = &m.links[k];
        l.cap.is_finite() && r.link_flow.get(*orig).copied().unwrap_or(0.0) >= l.cap * (1.0 - 1e-6) - 1e-9
    };
    // 0 unknown, 1 working it out (a loop: not short), 2 short, 3 not.
    let mut state = vec![0u8; nodes.len()];
    fn short(i: usize, m: &Model, full: &dyn Fn(usize) -> bool, at_cap: &dyn Fn(usize) -> bool, st: &mut Vec<u8>) -> bool {
        match st[i] {
            1 | 3 => return false,
            2 => return true,
            _ => {}
        }
        st[i] = 1;
        let n = &m.nodes[i];
        let answer = m.blocked[i]
            || m.ins[i].iter().enumerate().any(|(p, ks)| {
                !ks.is_empty() && (n.router.is_some() || n.store || n.inputs[p] > TOL) && ks.iter().all(|&k| feed_short(k, m, full, at_cap, st))
            });
        st[i] = if answer { 2 } else { 3 };
        answer
    }
    // Whether link k can't bring more: full, or its supplier can't give more.
    fn feed_short(k: usize, m: &Model, full: &dyn Fn(usize) -> bool, at_cap: &dyn Fn(usize) -> bool, st: &mut Vec<u8>) -> bool {
        if at_cap(k) {
            return true;
        }
        let s = m.links[k].1.from_node;
        let n = &m.nodes[s];
        if n.store && m.ins[s].iter().all(|ks| ks.is_empty()) {
            return false;
        }
        if n.router.is_some() || n.store {
            // A valve at its limit is full; otherwise what feeds it decides.
            return short(s, m, full, at_cap, st);
        }
        full(s) || short(s, m, full, at_cap, st)
    }
    (0..nodes.len())
        .map(|i| {
            let n = &nodes[i];
            let makes = n.outputs.iter().any(|o| *o > TOL);
            n.router.is_none() && !n.sink && !n.store && makes && !full(i) && !short(i, &m, &full, &at_cap, &mut state)
        })
        .collect()
}

/// Links sharing a port or router, each with its rank there.
type Group = Vec<(usize, u8)>;

struct Model<'a> {
    nodes: &'a [SimNode],
    /// (original index, link), valid links only.
    links: Vec<(usize, SimLink)>,
    /// Per node, per input / output port: the links (indexes into `links`).
    ins: Vec<Vec<Vec<usize>>>,
    outs: Vec<Vec<Vec<usize>>>,
    /// Nodes that can't run (an input with nothing wired to it).
    blocked: Vec<bool>,
    /// Upper bound on any flow: everything made at full speed.
    total: f64,
    /// Flows this small are rounding: a millionth of the biggest rate.
    noise: f64,
    groups: Vec<Group>,
}

/// Ranges speeds and flows are held to, beyond their own limits.
#[derive(Clone)]
struct Bounds {
    t: Vec<(f64, f64)>,
    f: Vec<(f64, f64)>,
}

/// Holds flow `v` with a hair of slack, so it stays feasible despite
/// rounding (separate solves of a recycling loop differ in the ninth digit).
fn around(v: f64) -> (f64, f64) {
    let e = 1e-7 * v.abs().max(1.0);
    ((v - e).max(0.0), v + e)
}

enum Goal<'a> {
    /// Maximise z with every listed link's flow >= z.
    FlowFloor(&'a [usize]),
    /// Maximise the sum of these links' flows.
    FlowSum(&'a [usize]),
    /// Maximise z with every listed node's speed >= z.
    SpeedFloor(&'a [usize]),
    /// Maximise the sum of these nodes' speeds.
    SpeedSum(&'a [usize]),
    /// Most shipped from machines and sources, each speed at the top of
    /// what it's held to; least passed between routers, and least to
    /// overflow links (a hair of slack in the speeds mustn't spill there).
    Ship,
}

struct Solved {
    z: f64,
    t: Vec<f64>,
    f: Vec<f64>,
}

impl<'a> Model<'a> {
    fn new(nodes: &'a [SimNode], links: &[SimLink]) -> Self {
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
        let mut ins: Vec<Vec<Vec<usize>>> = nodes.iter().map(|n| vec![vec![]; n.inputs.len()]).collect();
        let mut outs: Vec<Vec<Vec<usize>>> = nodes.iter().map(|n| vec![vec![]; n.outputs.len()]).collect();
        for (k, (_, l)) in links.iter().enumerate() {
            outs[l.from_node][l.from_port].push(k);
            ins[l.to_node][l.to_port].push(k);
        }
        let blocked = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| n.router.is_none() && !n.sink && !n.store && n.inputs.iter().enumerate().any(|(p, d)| *d > TOL && ins[i][p].is_empty()))
            .collect();
        let total = nodes.iter().filter(|n| n.router.is_none()).flat_map(|n| n.inputs.iter().chain(&n.outputs)).sum::<f64>() + 1.0;
        // Shares: a router's inputs, and its outputs; a machine's port with
        // several links (a belt split or joined without a router).
        let mut groups = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            match &n.router {
                Some(share) => {
                    let g_in: Group = ins[i].iter().enumerate().flat_map(|(p, ks)| ks.iter().map(move |k| (*k, share.in_rank(p)))).collect();
                    let g_out: Group = outs[i]
                        .iter()
                        .enumerate()
                        .filter_map(|(p, ks)| share.out_rank(p).map(|r| (ks, r)))
                        .flat_map(|(ks, r)| ks.iter().map(move |k| (*k, r)))
                        .collect();
                    groups.extend([g_in, g_out]);
                }
                None => {
                    groups.extend(ins[i].iter().map(|ks| ks.iter().map(|k| (*k, 0)).collect::<Group>()));
                    groups.extend(outs[i].iter().map(|ks| ks.iter().map(|k| (*k, u8::from(links[*k].1.overflow))).collect::<Group>()));
                }
            }
        }
        groups.retain(|g| g.len() > 1);
        let biggest = nodes.iter().flat_map(|n| n.inputs.iter().chain(&n.outputs)).fold(1.0_f64, |a, b| a.max(*b));
        Model { nodes, links, ins, outs, blocked, total, noise: 1e-6 * biggest, groups }
    }

    /// A closed router output's links carry nothing.
    fn closed(&self, k: usize) -> bool {
        let l = &self.links[k].1;
        self.nodes[l.from_node].router.as_ref().is_some_and(|s| s.out_rank(l.from_port).is_none())
    }

    fn solve(&self, b: &Bounds, goal: Goal) -> Option<Solved> {
        let mut vars = ProblemVariables::new();
        let t: Vec<Option<Variable>> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| n.router.is_none().then(|| vars.add(variable().min(b.t[i].0.clamp(0.0, 1.0)).max(b.t[i].1.clamp(b.t[i].0.clamp(0.0, 1.0), 1.0)))))
            .collect();
        let f: Vec<Variable> = self
            .links
            .iter()
            .enumerate()
            .map(|(k, (_, l))| {
                let top = if self.closed(k) { 0.0 } else { l.cap.min(self.total).max(0.0) };
                let hi = b.f[k].1.min(top);
                vars.add(variable().min(b.f[k].0.min(hi).max(0.0)).max(hi))
            })
            .collect();
        let z = vars.add(variable().min(0.0).max(self.total));
        let sum = |ks: &[usize]| ks.iter().fold(Expression::default(), |e, k| e + f[*k]);
        let speeds = |ns: &[usize]| ns.iter().filter_map(|i| t[*i]).fold(Expression::default(), |e, v| e + v);
        let objective: Expression = match goal {
            Goal::FlowFloor(_) | Goal::SpeedFloor(_) => z.into(),
            Goal::FlowSum(ks) => sum(ks),
            Goal::SpeedSum(ns) => speeds(ns),
            Goal::Ship => self.links.iter().enumerate().fold(Expression::default(), |e, (k, (_, l))| {
                let from = &self.nodes[l.from_node];
                let fed = self.ins[l.from_node].iter().any(|ks| !ks.is_empty());
                let w = if l.overflow {
                    -1.0
                } else if from.router.is_some() {
                    -1e-3
                } else if from.store && !fed {
                    // What a stocked store hands out is what's taken.
                    0.0
                } else {
                    1.0
                };
                e + w * f[k]
            }) + 1e-2 * t.iter().flatten().fold(Expression::default(), |e, v| e + *v),
        };
        let mut p = vars.maximise(objective).using(microlp);
        for (i, n) in self.nodes.iter().enumerate() {
            let all = |ports: &[Vec<usize>]| ports.iter().fold(Expression::default(), |e, ks| e + sum(ks));
            if let Some(share) = &n.router {
                if let Some(limit) = share.limit {
                    p.add_constraint(all(&self.outs[i]).leq(limit.max(0.0)));
                }
                p.add_constraint(all(&self.ins[i]).eq(all(&self.outs[i])));
                continue;
            }
            if n.store {
                // Out at most what comes in, when anything is wired in.
                if self.ins[i].iter().any(|ks| !ks.is_empty()) {
                    p.add_constraint(all(&self.outs[i]).leq(all(&self.ins[i])));
                }
                continue;
            }
            let Some(ti) = t[i] else { continue };
            for (port, ks) in self.ins[i].iter().enumerate() {
                if !ks.is_empty() && !n.sink {
                    p.add_constraint(sum(ks).eq(n.inputs[port] * ti));
                }
            }
            for (port, ks) in self.outs[i].iter().enumerate() {
                if !ks.is_empty() {
                    let s = n.outputs[port];
                    p.add_constraint(sum(ks).leq(s * ti));
                    p.add_constraint((sum(ks) - s * ti).geq(-BALANCE_TOLERANCE * s));
                }
            }
            // A port's belt or pipe carries at most its speed.
            let caps = n.inputs.iter().zip(&n.in_caps).chain(n.outputs.iter().zip(&n.out_caps));
            for (&rate, &cap) in caps {
                if rate > TOL && cap.is_finite() {
                    p.add_constraint((rate * ti).leq(cap.max(0.0)));
                }
            }
        }
        match goal {
            Goal::FlowFloor(ks) => ks.iter().for_each(|k| {
                p.add_constraint((f[*k] - z).geq(0.0));
            }),
            Goal::SpeedFloor(ns) => ns.iter().filter_map(|i| t[*i]).for_each(|v| {
                p.add_constraint((v - z).geq(0.0));
            }),
            _ => {}
        }
        let sol = p.solve().ok()?;
        Some(Solved { z: sol.value(z), t: t.iter().map(|v| v.map_or(1.0, |v| sol.value(v))).collect(), f: f.iter().map(|v| sol.value(*v)).collect() })
    }

    /// Fills the shared links' flows, rank by rank within each share.
    fn fill_shares(&self, b: &mut Bounds) {
        let mut member: BTreeMap<usize, Vec<(usize, u8)>> = BTreeMap::new();
        for (g, group) in self.groups.iter().enumerate() {
            for &(k, r) in group {
                member.entry(k).or_default().push((g, r));
            }
        }
        let agents: Vec<usize> = member.keys().copied().filter(|k| !self.closed(*k)).collect();
        let mut frozen = vec![false; self.links.len()];
        let mut level = vec![0.0_f64; self.links.len()];
        for _ in 0..agents.len() + 1 {
            let unfrozen: Vec<usize> = agents.iter().copied().filter(|k| !frozen[*k]).collect();
            if unfrozen.is_empty() {
                break;
            }
            // A link waits for better-ranked links of its own shares.
            let waits = |k: usize| member[&k].iter().any(|&(g, r)| self.groups[g].iter().any(|&(m, rm)| rm < r && !frozen[m] && !self.closed(m)));
            let mut active: Vec<usize> = unfrozen.iter().copied().filter(|k| !waits(*k)).collect();
            if active.is_empty() {
                active = unfrozen.clone();
            }
            // Unfrozen links keep what they reached.
            let mut p = b.clone();
            for &k in &unfrozen {
                p.f[k].0 = level[k];
            }
            let Some(floor) = self.solve(&p, Goal::FlowFloor(&active)) else { break };
            // Less than a millionth of the biggest rate is the held flows'
            // slack, not something left over (an overflow mustn't get it).
            let z = if floor.z < self.noise { 0.0 } else { floor.z };
            for &k in &active {
                level[k] = level[k].max(z);
                p.f[k].0 = around(level[k]).0;
            }
            // Which can rise further? One sum first, then each of the rest.
            let up = |s: &Solved, k: usize| s.f[k] > level[k] + 1e-6 * level[k].max(1.0);
            let rising = self.solve(&p, Goal::FlowSum(&active));
            let mut stuck: Vec<usize> = active
                .iter()
                .copied()
                .filter(|&k| !rising.as_ref().is_some_and(|s| up(s, k)))
                .filter(|&k| !self.solve(&p, Goal::FlowSum(&[k])).is_some_and(|s| up(&s, k)))
                .collect();
            if stuck.is_empty() {
                stuck = active;
            }
            // Held at what they carry in the floor's solution: one answer,
            // so everything held stays feasible together.
            for k in stuck {
                frozen[k] = true;
                if floor.f[k] >= self.noise {
                    level[k] = floor.f[k].max(level[k]);
                }
                b.f[k] = around(level[k]);
            }
        }
    }

    /// Max-min fair speeds for the nodes not yet pinned.
    fn fill_speeds(&self, b: &mut Bounds) {
        let n = self.nodes.len();
        let mut fixed: Vec<bool> = (0..n).map(|i| self.nodes[i].router.is_some() || self.blocked[i]).collect();
        for _ in 0..n + 1 {
            let free: Vec<usize> = (0..n).filter(|&i| !fixed[i]).collect();
            if free.is_empty() {
                break;
            }
            let Some(floor) = self.solve(b, Goal::SpeedFloor(&free)) else { break };
            let z = floor.z.min(1.0);
            // Held at their speeds in the floor's solution (a supplier may
            // need to run a hair ahead of its consumer: rounded rates).
            if z >= 1.0 - TOL {
                for i in free {
                    b.t[i] = around(floor.t[i]);
                }
                break;
            }
            let mut p = b.clone();
            for &i in &free {
                p.t[i].0 = (z - 1e-9).max(0.0);
            }
            let up = |s: &Solved, i: usize| s.t[i] > z + 1e-6;
            let rising = self.solve(&p, Goal::SpeedSum(&free));
            let mut stuck: Vec<usize> = free
                .iter()
                .copied()
                .filter(|&i| !rising.as_ref().is_some_and(|s| up(s, i)))
                .filter(|&i| !self.solve(&p, Goal::SpeedSum(&[i])).is_some_and(|s| up(&s, i)))
                .collect();
            if stuck.is_empty() {
                stuck = free;
            }
            for i in stuck {
                fixed[i] = true;
                b.t[i] = around(floor.t[i].max(z));
            }
        }
    }
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
    fn sink() -> SimNode {
        SimNode { inputs: vec![0.0], sink: true, ..Default::default() }
    }
    fn link(a: usize, b: usize) -> SimLink {
        SimLink::new(a, 0, b, 0)
    }
    fn port(a: usize, ap: usize, b: usize, bp: usize) -> SimLink {
        SimLink::new(a, ap, b, bp)
    }
    fn splitter() -> SimNode {
        SimNode::router(1, 1, Share::default())
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
        let r = simulate(&[src(30.0), machine(30.0, 30.0), out(10.0), sink()], &[link(0, 1), link(1, 2), link(1, 3)]);
        assert!(close(r.efficiency[1], 1.0));
        assert!(close(r.received[3][0], 20.0));
        assert!(close(r.link_flow[1] + r.link_flow[2], 30.0));
    }

    #[test]
    fn two_consumers_share_and_one_recovers() {
        // A machine limited by its other input leaves the rest of the shared
        // belt to its neighbour, like a splitter backing up in game.
        let starved = SimNode { inputs: vec![10.0, 10.0], outputs: vec![], throttles: true, ..Default::default() };
        let r = simulate(&[src(10.0), machine(10.0, 10.0), starved, src(1.0), out(10.0)], &[link(0, 1), link(0, 2), port(3, 0, 2, 1), link(1, 4)]);
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
        let r = simulate(&[a, b, src(10.0), out(10.0)], &[port(0, 0, 1, 0), port(1, 0, 0, 0), port(2, 0, 0, 1), port(1, 1, 3, 0)]);
        assert!(r.efficiency.iter().all(|e| close(*e, 1.0)), "{:?}", r.efficiency);
    }

    #[test]
    fn a_splitter_shares_equally() {
        // 45 into a splitter, out to machines taking 60 and 30: 22.5 each.
        let r = simulate(&[src(45.0), splitter(), machine(60.0, 60.0), machine(30.0, 30.0), sink(), sink()], &[link(0, 1), link(1, 2), link(1, 3), link(2, 4), link(3, 5)]);
        assert!(close(r.received[2][0], 22.5) && close(r.received[3][0], 22.5), "{:?}", r.received);
        // A backed-up output's share goes to the others: 50 in, the small
        // machine takes its 10, the big one the other 40.
        let r = simulate(&[src(50.0), splitter(), machine(60.0, 60.0), machine(10.0, 10.0), sink(), sink()], &[link(0, 1), link(1, 2), link(1, 3), link(2, 4), link(3, 5)]);
        assert!(close(r.received[2][0], 40.0) && close(r.received[3][0], 10.0), "{:?}", r.received);
    }

    #[test]
    fn items_stay_on_their_belts() {
        // A -> splitter -> X (wants 60) and -> merger; C -> merger -> Y (wants 30).
        // X can only get A's 30; the merger takes equally from its inputs.
        let nodes = [src(30.0), src(30.0), splitter(), splitter(), machine(60.0, 0.0), machine(30.0, 0.0)];
        let r = simulate(&nodes, &[link(0, 2), link(2, 4), link(2, 3), link(1, 3), link(3, 5)]);
        assert!(close(r.received[4][0], 15.0), "X: {:?}", r.received);
        assert!(close(r.received[5][0], 30.0), "Y: {:?}", r.received);
        assert!(close(r.shipped[1][0], 15.0), "C backs up: {:?}", r.shipped);
    }

    #[test]
    fn a_belt_between_routers_holds_its_speed() {
        // Two sources of 60 merged onto a belt of 60, split to two machines.
        let nodes = [src(60.0), src(60.0), splitter(), splitter(), machine(60.0, 0.0), machine(60.0, 0.0)];
        let mut middle = link(2, 3);
        middle.cap = 60.0;
        let r = simulate(&nodes, &[link(0, 2), link(1, 2), middle, link(3, 4), link(3, 5)]);
        assert!(close(r.received[4][0] + r.received[5][0], 60.0), "{:?}", r.received);
        assert!(close(r.shipped[0][0], 30.0) && close(r.shipped[1][0], 30.0), "{:?}", r.shipped);
    }

    #[test]
    fn overflow_takes_only_the_rest() {
        // A plain splitter into a machine and a sink starves the machine, as in game.
        let r = simulate(&[src(30.0), splitter(), machine(20.0, 0.0), sink()], &[link(0, 1), link(1, 2), link(1, 3)]);
        assert!(close(r.received[2][0], 15.0), "{:?}", r.received);
        // A smart splitter with the sink on its overflow output doesn't.
        let smart = SimNode::router(1, 2, Share { out_rank: vec![Some(0), Some(1)], ..Default::default() });
        let r = simulate(&[src(30.0), smart, machine(20.0, 0.0), sink()], &[link(0, 1), link(1, 2), port(1, 1, 3, 0)]);
        assert!(close(r.received[2][0], 20.0) && close(r.received[3][0], 10.0), "{:?}", r.received);
        // A closed output passes nothing.
        let closed = SimNode::router(1, 2, Share { out_rank: vec![Some(0), None], ..Default::default() });
        let r = simulate(&[src(30.0), closed, machine(20.0, 0.0), sink()], &[link(0, 1), link(1, 2), port(1, 1, 3, 0)]);
        assert!(close(r.received[3][0], 0.0) && close(r.received[2][0], 20.0), "{:?}", r.received);
    }

    #[test]
    fn a_priority_merger_takes_high_first() {
        // Inputs of 40 (low) and 40 (high) into a machine taking 60.
        let merger = SimNode::router(2, 1, Share { in_rank: vec![1, 0], ..Default::default() });
        let r = simulate(&[src(40.0), src(40.0), merger, machine(60.0, 0.0)], &[port(0, 0, 2, 0), port(1, 0, 2, 1), link(2, 3)]);
        assert!(close(r.shipped[1][0], 40.0) && close(r.shipped[0][0], 20.0), "{:?}", r.shipped);
    }

    #[test]
    fn a_valve_limits_the_flow() {
        let valve = SimNode::router(1, 1, Share { limit: Some(25.0), ..Default::default() });
        let r = simulate(&[src(60.0), valve, machine(60.0, 0.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.received[2][0], 25.0), "{:?}", r.received);
    }

    #[test]
    fn a_factory_overflow_link_takes_the_rest() {
        let mut spill = link(0, 2);
        spill.overflow = true;
        let r = simulate(&[src(30.0), machine(20.0, 0.0), sink()], &[link(0, 1), spill]);
        assert!(close(r.received[1][0], 20.0) && close(r.received[2][0], 10.0), "{:?}", r.received);
    }

    #[test]
    fn storage_passes_on_what_arrives() {
        let store = || SimNode { inputs: vec![0.0], outputs: vec![0.0], store: true, ..Default::default() };
        // 60 in, a machine taking 30 after it: 30 out, the rest fills it.
        let r = simulate(&[src(60.0), store(), machine(30.0, 0.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.received[1][0], 60.0) && close(r.shipped[1][0], 30.0), "{:?} {:?}", r.received, r.shipped);
        // 20 in: the machine gets 20.
        let r = simulate(&[src(20.0), store(), machine(30.0, 0.0)], &[link(0, 1), link(1, 2)]);
        assert!(close(r.received[2][0], 20.0), "{:?}", r.received);
        // Nothing wired in: it hands out what's taken.
        let r = simulate(&[store(), machine(30.0, 0.0)], &[link(0, 1)]);
        assert!(close(r.received[1][0], 30.0) && close(r.efficiency[1], 1.0), "{:?}", r.received);
    }

    #[test]
    fn held_back_by_what_follows() {
        // A miner of 60 into a smelter taking 30: the miner is held back.
        let r = simulate(&[src(60.0), machine(30.0, 30.0), out(30.0)], &[link(0, 1), link(1, 2)]);
        assert_eq!(held_back(&[src(60.0), machine(30.0, 30.0), out(30.0)], &[link(0, 1), link(1, 2)], &r), vec![true, false, false]);
        // A source of 15 into a machine taking 30: the machine is short, not held back.
        let nodes = [src(15.0), machine(30.0, 20.0), out(20.0)];
        let r = simulate(&nodes, &[link(0, 1), link(1, 2)]);
        assert_eq!(held_back(&nodes, &[link(0, 1), link(1, 2)], &r), vec![false, false, false]);
        // A slow belt into the machine: short (the source is held back by the belt).
        let mut slow = link(0, 1);
        slow.cap = 15.0;
        let nodes = [src(60.0), machine(30.0, 30.0), out(30.0)];
        let r = simulate(&nodes, &[slow, link(1, 2)]);
        assert_eq!(held_back(&nodes, &[slow, link(1, 2)], &r)[1], false);
        // Two in a chain: the miner and the smelter both wait on a constructor.
        let nodes = [src(60.0), machine(60.0, 60.0), machine(15.0, 15.0), out(15.0)];
        let links = [link(0, 1), link(1, 2), link(2, 3)];
        let r = simulate(&nodes, &links);
        assert_eq!(held_back(&nodes, &links, &r), vec![true, true, false, false]);
    }
}
