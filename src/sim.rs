//! Switch-level NMOS simulator.
//!
//! This is a port of the algorithm in Visual6502's `chipsim.js` (Brian and
//! Barry Silverman), using the flat-array layout and group-value priority of
//! Michael Steil's perfect6502:
//!
//! * Every transistor is an ideal switch between `c1` and `c2`, closed while
//!   its gate node is high.
//! * Nodes connected through closed switches form a *group*. The group's
//!   value is decided by (strongest first): ground, power, an externally
//!   driven pull-down, a pull-up resistor, or any member that was already
//!   high (charge stored on the gate capacitance).
//! * When a node changes, the transistors it gates toggle and their channel
//!   nodes are scheduled for recalculation, until the netlist settles.
//! * Under [`Process::Nmos`] (the default), a large group that would be
//!   raised by pull-up resistors alone waits until everything else has
//!   settled: depletion loads charge a bus slowly, so on the real chip
//!   a latch closing on the same clock edge sees the bus still low.
//!
//! The simulator knows nothing about the 6502; see [`crate::cpu`] for that.

use crate::netlist::{Netlist, NodeId};

/// Give up on a recalculation after this many passes (as `chipsim.js` does);
/// a stable digital netlist never comes close.
const MAX_ITERATIONS: usize = 100;

/// Under [`Process::Nmos`], the smallest group whose pull-up-only rise is
/// treated as slow. Group size stands in for capacitance (the netlist has no
/// node areas): it excludes the very common pair of a pulled-up node and the
/// storage node behind one pass transistor, while every bus conflict seen in
/// the unofficial opcodes involves six or more nodes.
const SLOW_PULLUP_MIN_GROUP: usize = 3;

#[inline]
fn drive_value(d: Drive) -> bool {
    matches!(d, Drive::Vcc | Drive::DrivenHi | Drive::Pullup | Drive::Hi)
}

/// The semiconductor process a netlist is built in, which decides how
/// [`Sim`] resolves the cases a pure switch model cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Process {
    /// Depletion-load NMOS (the original 6502). Pull-ups are resistive and
    /// weak: a node pulled low by a transistor wins against them at once,
    /// but a group left to a pull-up alone charges slowly. A low-to-high
    /// rise caused only by pull-up resistors is therefore deferred until
    /// every other pending event has settled, so a clock edge that closes
    /// a latch beats a bus that is still charging. This is what gives the
    /// unofficial opcodes their AND-like results.
    #[default]
    Nmos,
    /// Plain switch model: every transition settles in netlist order, as
    /// in `chipsim.js` and perfect6502.
    Ideal,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Drive {
    Nothing,
    /// Stored charge.
    Hi,
    /// A pull-up resistor.
    Pullup,
    /// An externally driven-high pin.
    DrivenHi,
    Pulldown,
    Vcc,
    Vss,
}

/// What happens when a gate node switches a transistor: `inc` is added to
/// (or, when switching off, subtracted from) the counters of both `a` and
/// `b`, `a` is scheduled, and when switching off `b` is scheduled too. For
/// a pass transistor `a` and `b` are the channel ends; for a rail
/// transistor `b` is the never-scheduled sink node, so every kind takes the
/// same branch-free path.
#[derive(Clone, Copy)]
struct Act {
    a: NodeId,
    b: NodeId,
    inc: u64,
}

/// Fields of [`Node::counts`].
const VSS_ONE: u64 = 1;
const VCC_ONE: u64 = 1 << 16;
const PASS_ONE: u64 = 1 << 32;

/// How a transistor's channel is connected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Between two ordinary nodes.
    Pass,
    /// Between `node` and ground.
    ToVss(NodeId),
    /// Between `node` and power.
    ToVcc(NodeId),
    /// Both ends on rails (or the same node): never affects anything.
    Inert,
}

#[derive(Clone, Copy, Default)]
struct Node {
    /// Conducting transistors at the node, in 16-bit fields: bits 0..16 to
    /// ground, 16..32 to power, 32..48 pass transistors to other nodes.
    /// Rail connections are counted rather than stored as edges, so group
    /// searches only walk node-to-node pass transistors. Zero pass
    /// transistors means the node is a group on its own and no edge scan is
    /// needed.
    counts: u64,
    /// Generation stamps: member of the group being built / already
    /// settled in the current pass.
    group_mark: u32,
    done_mark: u32,
    /// 0: floating, 1: pull-up resistor, 2: driven low, 3: driven high.
    pull: u8,
    state: bool,
    queued: bool,
}

impl Node {
    #[inline(always)]
    fn pass_on(&self) -> u64 {
        self.counts >> 32
    }
}

/// A vector indexed without bounds checks. Every index used by the
/// simulator comes from the netlist, which `Sim::new` validates
/// (`debug_assert`ed here).
#[derive(Clone)]
struct Arr<T>(Vec<T>);

impl<T> std::ops::Index<usize> for Arr<T> {
    type Output = T;
    #[inline(always)]
    fn index(&self, i: usize) -> &T {
        debug_assert!(i < self.0.len());
        // SAFETY: indices are node/edge numbers checked against the netlist.
        unsafe { self.0.get_unchecked(i) }
    }
}

impl<T> std::ops::IndexMut<usize> for Arr<T> {
    #[inline(always)]
    fn index_mut(&mut self, i: usize) -> &mut T {
        debug_assert!(i < self.0.len());
        // SAFETY: as above.
        unsafe { self.0.get_unchecked_mut(i) }
    }
}

#[derive(Clone)]
pub struct Sim {
    vcc: NodeId,
    vss: NodeId,

    /// Per-node state, packed so one node touch is one cache line.
    nodes: Arr<Node>,
    /// Pull-up resistors from the netlist (restored by `reset_state`).
    resistor: Vec<bool>,
    /// CSR: transistors whose gate is the node.
    gates_start: Arr<u32>,
    /// (A transistor is on exactly when its gate node is high.)
    gates: Arr<Act>,
    /// CSR: (gate, other end) for pass transistors touching the node.
    chan_start: Arr<u32>,
    chan: Arr<(NodeId, NodeId)>,

    // Work lists.
    list: Vec<NodeId>,
    next: Vec<NodeId>,
    group: Vec<NodeId>,
    group_gen: u32,
    done_gen: u32,
    /// Groups whose pull-up-only rise is waiting for the rest of the
    /// netlist to settle (see [`Process::Nmos`]).
    deferred: Vec<NodeId>,
    /// False for the pass that applies `deferred`.
    allow_defer: bool,

    process: Process,

    /// Number of times recalculation hit [`MAX_ITERATIONS`].
    pub unstable_events: u64,
}

impl Sim {
    pub fn new(nl: &Netlist) -> Self {
        let n = nl.node_count as usize;
        let (vcc, vss) = (nl.vcc, nl.vss);
        let is_rail = |x: NodeId| x == vcc || x == vss;
        // The hot loops index without bounds checks, so every node number
        // they can see must be in range.
        assert!(
            nl.pullup.len() == n,
            "pullup table does not match node count"
        );
        assert!(
            (vcc as usize) < n && (vss as usize) < n,
            "rail out of range"
        );
        for t in &nl.transistors {
            assert!(
                [t.gate, t.c1, t.c2].iter().all(|&x| (x as usize) < n),
                "transistor {} references a node out of range",
                t.name
            );
        }

        // Drop exact duplicates (same gate, same channel ends): they always
        // switch together and add nothing.
        let mut seen = std::collections::HashSet::new();
        let trans: Vec<_> = nl
            .transistors
            .iter()
            .filter(|t| seen.insert((t.gate, t.c1.min(t.c2), t.c1.max(t.c2))))
            .collect();
        let mut t_kind = Vec::with_capacity(trans.len());
        let mut gcount = vec![0u32; n + 1];
        let mut ccount = vec![0u32; n + 1];
        for t in &trans {
            let kind = match (t.c1, t.c2) {
                (a, b) if a == b => Kind::Inert,
                (a, b) if is_rail(a) && is_rail(b) => Kind::Inert,
                (a, b) if a == vss => Kind::ToVss(b),
                (a, b) if b == vss => Kind::ToVss(a),
                (a, b) if a == vcc => Kind::ToVcc(b),
                (a, b) if b == vcc => Kind::ToVcc(a),
                _ => Kind::Pass,
            };
            if kind != Kind::Inert {
                gcount[t.gate as usize] += 1;
            }
            if kind == Kind::Pass {
                ccount[t.c1 as usize] += 1;
                ccount[t.c2 as usize] += 1;
            }
            t_kind.push(kind);
        }
        let prefix = |c: &[u32]| {
            let mut s = vec![0u32; n + 1];
            for i in 0..n {
                s[i + 1] = s[i] + c[i];
            }
            s
        };
        let gates_start = prefix(&gcount);
        let chan_start = prefix(&ccount);
        let mut gfill = gates_start.clone();
        let mut cfill = chan_start.clone();
        // Index `n` is an extra sink node for rail transistors (see `Act`).
        let sink = n as NodeId;
        let mut gates = vec![Act { a: 0, b: 0, inc: 0 }; gates_start[n] as usize];
        let mut chan = vec![(0 as NodeId, 0 as NodeId); chan_start[n] as usize];
        for (t, &kind) in trans.iter().zip(&t_kind) {
            let act = match kind {
                Kind::Pass => Act {
                    a: t.c1,
                    b: t.c2,
                    inc: PASS_ONE,
                },
                Kind::ToVss(x) => Act {
                    a: x,
                    b: sink,
                    inc: VSS_ONE,
                },
                Kind::ToVcc(x) => Act {
                    a: x,
                    b: sink,
                    inc: VCC_ONE,
                },
                Kind::Inert => continue,
            };
            gates[gfill[t.gate as usize] as usize] = act;
            gfill[t.gate as usize] += 1;
            if kind == Kind::Pass {
                chan[cfill[t.c1 as usize] as usize] = (t.gate, t.c2);
                cfill[t.c1 as usize] += 1;
                chan[cfill[t.c2 as usize] as usize] = (t.gate, t.c1);
                cfill[t.c2 as usize] += 1;
            }
        }

        let mut s = Sim {
            vcc,
            vss,
            nodes: Arr(vec![Node::default(); n + 1]),
            resistor: nl.pullup.clone(),
            gates_start: Arr(gates_start),
            gates: Arr(gates),
            chan_start: Arr(chan_start),
            chan: Arr(chan),
            list: Vec::new(),
            next: Vec::new(),
            group: Vec::new(),
            group_gen: 0,
            done_gen: 0,
            deferred: Vec::new(),
            allow_defer: true,
            process: Process::default(),
            unstable_events: 0,
        };
        s.reset_state();
        s
    }

    /// Clear every node and transistor to the power-on (all low) state,
    /// keeping pull-ups but dropping any externally driven pins.
    pub fn reset_state(&mut self) {
        for (node, &r) in self.nodes.0.iter_mut().zip(&self.resistor) {
            *node = Node {
                pull: r as u8,
                ..Node::default()
            };
        }
        // The rail sink (see `Act`) is permanently "queued", so it is never
        // scheduled; its counters are never read.
        self.nodes[self.resistor.len()] = Node {
            queued: true,
            ..Node::default()
        };
        self.group_gen = 0;
        self.done_gen = 0;
        self.nodes[self.vcc as usize].state = true;
        // Transistors gated by vcc are permanently on.
        self.ensure_next_capacity();
        let (a, b) = self.gate_range(self.vcc);
        for k in a..b {
            self.switch(self.gates[k], true);
        }
        // `switch` scheduled their channel nodes; leave them for the next
        // recalculation rather than marked as queued forever.
        for n in self.next.drain(..) {
            self.nodes.0[n as usize].queued = false;
        }
    }

    /// Select how ambiguous transitions are resolved (default
    /// [`Process::Nmos`]). Takes effect from the next recalculation.
    pub fn set_process(&mut self, process: Process) {
        self.process = process;
    }

    pub fn process(&self) -> Process {
        self.process
    }

    pub fn node_count(&self) -> usize {
        self.resistor.len()
    }

    #[inline]
    pub fn is_high(&self, n: NodeId) -> bool {
        self.nodes.0[n as usize].state
    }

    /// Externally drive a node (like an input pad) without recalculating.
    #[inline]
    pub fn drive(&mut self, n: NodeId, high: bool) {
        self.nodes.0[n as usize].pull = if high { 3 } else { 2 };
    }

    /// Drive a node high and propagate.
    pub fn set_high(&mut self, n: NodeId) {
        self.drive(n, true);
        self.recalc(&[n]);
    }

    /// Drive a node low and propagate.
    pub fn set_low(&mut self, n: NodeId) {
        self.drive(n, false);
        self.recalc(&[n]);
    }

    pub fn set(&mut self, n: NodeId, high: bool) {
        self.drive(n, high);
        self.recalc(&[n]);
    }

    /// Recalculate every node.
    pub fn recalc_all(&mut self) {
        let all: Vec<NodeId> = (0..self.node_count() as NodeId).collect();
        self.recalc(&all);
    }

    /// Recalculate the given nodes and everything that changes as a result.
    pub fn recalc(&mut self, nodes: &[NodeId]) {
        let mut list = std::mem::take(&mut self.list);
        list.clear();
        for &n in nodes {
            if n != self.vcc && n != self.vss && !self.nodes.0[n as usize].queued {
                self.nodes.0[n as usize].queued = true;
                list.push(n);
            }
        }
        self.next.clear();
        self.ensure_next_capacity();
        self.deferred.clear();
        self.allow_defer = true;
        let mut iter = 0;
        while !list.is_empty() {
            if iter == MAX_ITERATIONS {
                self.unstable_events += 1;
                for &n in &list {
                    self.nodes[n as usize].queued = false;
                }
                list.clear();
                self.deferred.clear();
                break;
            }
            iter += 1;
            for &n in &list {
                self.nodes[n as usize].queued = false;
            }
            self.done_gen = self.done_gen.wrapping_add(1);
            if self.done_gen == 0 {
                self.nodes.0.iter_mut().for_each(|x| x.done_mark = 0);
                self.done_gen = 1;
            }
            for &n in &list {
                self.recalc_node(n);
            }
            std::mem::swap(&mut list, &mut self.next);
            self.next.clear();
            self.ensure_next_capacity();
            // Everything fast has settled: let the slow rises happen (they
            // are recalculated from scratch, so any that no longer apply
            // are dropped), then carry on with whatever they set off.
            self.allow_defer = !list.is_empty() || self.deferred.is_empty();
            if !self.allow_defer {
                let mut deferred = std::mem::take(&mut self.deferred);
                for &n in &deferred {
                    if !self.nodes[n as usize].queued {
                        self.nodes[n as usize].queued = true;
                        list.push(n);
                    }
                }
                deferred.clear();
                self.deferred = deferred;
            }
        }
        self.list = list;
    }

    #[inline]
    fn gate_range(&self, n: NodeId) -> (usize, usize) {
        (
            self.gates_start[n as usize] as usize,
            self.gates_start[n as usize + 1] as usize,
        )
    }

    #[inline]
    fn enqueue(&mut self, n: NodeId) {
        // Branch-free: always write the slot, only advance if newly queued.
        let node = &mut self.nodes[n as usize];
        let fresh = !node.queued;
        node.queued = true;
        let len = self.next.len();
        debug_assert!(len < self.next.capacity());
        // SAFETY: `ensure_next_capacity` reserved room for every node plus
        // one spare slot, and a node is in `next` at most once.
        unsafe {
            self.next.as_mut_ptr().add(len).write(n);
            self.next.set_len(len + fresh as usize);
        }
    }

    /// Room for every node plus one, as `enqueue` requires. (Cheap after
    /// the first call; needed because `Vec::clone` drops spare capacity.)
    #[inline]
    fn ensure_next_capacity(&mut self) {
        let need = self.nodes.0.len() + 1;
        if self.next.capacity() < need {
            self.next.reserve(need - self.next.len());
        }
    }

    /// A transistor's gate switched: keep the per-node counters in sync
    /// and schedule the affected nodes.
    #[inline]
    fn switch(&mut self, act: Act, on: bool) {
        let Act { a, b, inc } = act;
        if on {
            self.nodes[a as usize].counts += inc;
            self.nodes[b as usize].counts = self.nodes[b as usize].counts.wrapping_add(inc);
            // Both ends now share a group: one recalculation covers it.
            self.enqueue(a);
        } else {
            self.nodes[a as usize].counts -= inc;
            self.nodes[b as usize].counts = self.nodes[b as usize].counts.wrapping_sub(inc);
            self.enqueue(a);
            self.enqueue(b); // a no-op for the rail sink
        }
    }

    #[inline]
    fn node_drive(&self, m: usize) -> Drive {
        const TABLE: [Drive; 8] = [
            Drive::Nothing,
            Drive::Hi,
            Drive::Pullup,
            Drive::Pullup,
            Drive::Pulldown,
            Drive::Pulldown,
            Drive::DrivenHi,
            Drive::DrivenHi,
        ];
        // Selects rather than branches: rail state is data-dependent.
        let node = &self.nodes[m];
        let mut d = TABLE[((node.pull as usize) << 1) | node.state as usize];
        if node.counts & (0xffff * VCC_ONE) != 0 {
            d = Drive::Vcc;
        }
        if node.counts & (0xffff * VSS_ONE) != 0 {
            d = Drive::Vss;
        }
        d
    }

    fn recalc_node(&mut self, n: NodeId) {
        let ni = n as usize;
        if self.nodes[ni].done_mark == self.done_gen {
            return; // already settled as part of an earlier group this pass
        }
        if self.nodes[ni].pass_on() == 0 {
            // Fast path: an isolated node.
            self.nodes[ni].done_mark = self.done_gen;
            let value = drive_value(self.node_drive(ni));
            if self.nodes[ni].state != value {
                self.set_node(ni, value);
            }
            return;
        }
        if self.nodes[ni].pass_on() == 1 {
            // Fast path: two nodes joined by a single conducting transistor
            // (the most common group). Same visiting order as `build_group`.
            let other = self.conducting_neighbour(ni) as usize;
            if self.nodes[other].pass_on() == 1 {
                let drive = self.node_drive(ni).max(self.node_drive(other));
                let value = drive_value(drive);
                for m in [ni, other] {
                    self.nodes[m].done_mark = self.done_gen;
                    if self.nodes[m].state != value {
                        self.set_node(m, value);
                    }
                }
                return;
            }
        }
        let drive = self.build_group(n);
        let value = drive_value(drive);
        let group = std::mem::take(&mut self.group);
        if drive == Drive::Pullup && self.defer_slow_rise(n, &group) {
            self.group = group;
            return;
        }
        for &m in &group {
            let mi = m as usize;
            self.nodes[mi].done_mark = self.done_gen;
            if self.nodes[mi].state != value {
                self.set_node(mi, value);
            }
        }
        self.group = group;
    }

    /// A large group about to be raised by pull-up resistors alone:
    /// under [`Process::Nmos`], postpone that until the fast events have
    /// settled. Returns whether it was postponed (the group is then marked
    /// done for this pass and left unchanged).
    #[inline]
    fn defer_slow_rise(&mut self, n: NodeId, group: &[NodeId]) -> bool {
        if self.process != Process::Nmos
            || group.len() < SLOW_PULLUP_MIN_GROUP
            || !self.allow_defer
            || !group.iter().any(|&m| !self.nodes[m as usize].state)
        {
            return false;
        }
        for &m in group {
            self.nodes[m as usize].done_mark = self.done_gen;
        }
        self.deferred.push(n);
        true
    }

    /// The node at the other end of `n`'s only conducting pass transistor.
    #[inline]
    fn conducting_neighbour(&self, n: usize) -> NodeId {
        let (a, b) = (self.chan_start[n] as usize, self.chan_start[n + 1] as usize);
        for k in a..b {
            let (gate, other) = self.chan[k];
            if self.nodes[gate as usize].state {
                return other;
            }
        }
        unreachable!("pass transistor count out of sync")
    }

    #[inline]
    fn set_node(&mut self, m: usize, value: bool) {
        self.nodes[m].state = value;
        let (a, b) = self.gate_range(m as NodeId);
        for k in a..b {
            self.switch(self.gates[k], value);
        }
    }

    /// Collect the group containing `n` into `self.group` (which doubles as
    /// the work stack) and return the strongest drive within it.
    fn build_group(&mut self, n: NodeId) -> Drive {
        self.group.clear();
        self.group_gen = self.group_gen.wrapping_add(1);
        if self.group_gen == 0 {
            self.nodes.0.iter_mut().for_each(|x| x.group_mark = 0);
            self.group_gen = 1;
        }
        let generation = self.group_gen;
        let mut drive = Drive::Nothing;
        self.group.push(n);
        self.nodes[n as usize].group_mark = generation;
        let mut i = 0;
        while i < self.group.len() {
            let mi = self.group[i] as usize;
            i += 1;
            drive = drive.max(self.node_drive(mi));
            if self.nodes[mi].pass_on() == 0 {
                continue;
            }
            let (a, b) = (
                self.chan_start[mi] as usize,
                self.chan_start[mi + 1] as usize,
            );
            for k in a..b {
                let (gate, other) = self.chan[k];
                if self.nodes[gate as usize].state
                    && self.nodes[other as usize].group_mark != generation
                {
                    self.nodes[other as usize].group_mark = generation;
                    self.group.push(other);
                }
            }
        }
        drive
    }

    /// One character per node: `h`/`l`, or `v`/`g` for power and ground
    /// (the format of `chipsim.js:stateString`).
    pub fn state_string(&self) -> String {
        (0..self.node_count())
            .map(|i| {
                if i == self.vss as usize {
                    'g'
                } else if i == self.vcc as usize {
                    'v'
                } else if self.nodes[i].state {
                    'h'
                } else {
                    'l'
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;

    /// An NMOS inverter: `out` has a pull-up and a transistor to ground gated
    /// by `in`. A second inverter makes a buffer.
    #[test]
    fn inverter_chain() {
        let text = "rustic6502-netlist 1\nvcc 1\nvss 0\npullup 3 4\n\
                    t t0 2 3 0\nt t1 3 4 0\nn in 2\nn mid 3\nn out 4\n";
        let nl = Netlist::from_native(text).unwrap();
        let mut s = Sim::new(&nl);
        s.set_low(2);
        s.recalc_all();
        assert!(s.is_high(3));
        assert!(!s.is_high(4));
        s.set_high(2);
        assert!(!s.is_high(3));
        assert!(s.is_high(4));
        s.set_low(2);
        assert!(s.is_high(3));
        assert!(!s.is_high(4));
    }

    /// A pass transistor stores charge on an isolated node.
    #[test]
    fn dynamic_storage() {
        let text = "rustic6502-netlist 1\nvcc 1\nvss 0\nt t0 2 3 4\nn en 2\nn d 3\nn q 4\n";
        let nl = Netlist::from_native(text).unwrap();
        let mut s = Sim::new(&nl);
        s.set_high(2);
        s.set_high(3);
        assert!(s.is_high(4));
        s.set_low(2); // isolate q
        s.set_low(3);
        assert!(s.is_high(4), "q keeps its charge");
        s.set_high(2);
        assert!(!s.is_high(4));
    }

    /// A bus held low through a pass transistor gated by `in`, with a
    /// pull-up of its own, feeds a latch whose enable is `in` delayed by two
    /// inverters. When `in` falls the bus loses its pull-down two steps
    /// before the latch closes. An ideal switch model lets the pull-up win
    /// that race; NMOS keeps the latched 0.
    #[test]
    fn slow_pullup_loses_race_with_latch() {
        // 2 in, 3 d (always low), 4 and 8 bus (pull-up on 4), 5 latch,
        // 6 /in, 7 enable
        let text = "rustic6502-netlist 1\nvcc 1\nvss 0\npullup 4 6 7\n\
                    t t0 1 3 0\nt t1 2 3 4\nt t2 7 4 5\n\
                    t t3 2 6 0\nt t4 6 7 0\nt t5 1 4 8\n";
        let nl = Netlist::from_native(text).unwrap();
        for (process, latched) in [(Process::Ideal, true), (Process::Nmos, false)] {
            let mut s = Sim::new(&nl);
            s.set_process(process);
            s.set_high(2);
            s.recalc_all();
            assert!(s.is_high(7) && !s.is_high(4) && !s.is_high(5));
            s.set_low(2);
            assert!(!s.is_high(7), "latch closed");
            assert!(s.is_high(4), "bus pulled up once undriven");
            assert_eq!(s.is_high(5), latched, "{process:?}");
        }
    }

    /// `Vec::clone` drops spare capacity, which the work lists rely on; a
    /// cloned simulator must still run (debug builds assert the capacity).
    #[test]
    fn clone_keeps_running() {
        let nl = crate::builtin_netlist();
        let mut a = crate::Cpu6502::new(&nl).unwrap();
        a.reset();
        let clk = a.pins.clk0;
        let mut b = a.sim.clone();
        for _ in 0..20 {
            for s in [&mut a.sim, &mut b] {
                s.set(clk, !s.is_high(clk));
            }
        }
        assert_eq!(a.sim.state_string(), b.state_string());
    }
}
