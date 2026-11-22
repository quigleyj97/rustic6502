//! Drive a 6502-compatible netlist: clock it, service its bus from a 64 KiB
//! memory, and read back registers for tracing.
//!
//! Bus timing follows Visual6502's `macros.js` (`halfStep`, `handleBusRead`,
//! `handleBusWrite`): when `clk0` falls the memory places the byte at the
//! address bus onto the data bus (if `rw` is high); when `clk0` rises a
//! pending write (`rw` low) is committed to memory.
//!
//! The chip is always clocked through its clock input pin (`clk_in`, or
//! `clk0` when there is no separate input). On chips with an on-chip clock
//! divider, such as the 2A03, `clk0` is the divider's output and
//! [`Netlist::clock_ratio`] says how many input toggles make one CPU
//! half-cycle.

use crate::netlist::{Netlist, NodeId};
use crate::sim::Sim;

/// Node numbers for the pads and registers used by the driver.
#[derive(Debug, Clone)]
pub struct Pins {
    /// The clock input pin the simulator toggles.
    pub clk_in: NodeId,
    /// The CPU clock phase (phi0). The same node as `clk_in` on the 6502.
    pub clk0: NodeId,
    pub res: NodeId,
    pub rw: NodeId,
    pub sync: NodeId,
    pub rdy: NodeId,
    pub irq: NodeId,
    pub nmi: NodeId,
    pub so: NodeId,
    pub ab: [NodeId; 16],
    pub db: [NodeId; 8],
    pub regs: Option<Regs>,
}

/// Internal register nodes; optional because they are only needed for
/// tracing and a variant's netlist might not name them.
#[derive(Debug, Clone)]
pub struct Regs {
    pub a: [NodeId; 8],
    pub x: [NodeId; 8],
    pub y: [NodeId; 8],
    pub s: [NodeId; 8],
    pub p: [Option<NodeId>; 8],
    pub pcl: [NodeId; 8],
    pub pch: [NodeId; 8],
    pub ir: [NodeId; 8],
}

impl Pins {
    pub fn from_netlist(nl: &Netlist) -> Result<Self, String> {
        let get = |n: &str| {
            nl.node(n)
                .ok_or_else(|| format!("netlist has no node named '{n}'"))
        };
        let first = |names: &[&str]| {
            names
                .iter()
                .find_map(|n| nl.node(n))
                .ok_or_else(|| format!("netlist has no node named any of {names:?}"))
        };
        fn bus<const N: usize>(
            f: impl Fn(&str) -> Result<NodeId, String>,
            prefix: &str,
        ) -> Result<[NodeId; N], String> {
            let mut out = [0; N];
            for (i, o) in out.iter_mut().enumerate() {
                *o = f(&format!("{prefix}{i}"))?;
            }
            Ok(out)
        }
        let regs = (|| -> Result<Regs, String> {
            let mut p = [None; 8];
            for (i, slot) in p.iter_mut().enumerate() {
                *slot = nl.node(&format!("p{i}"));
            }
            Ok(Regs {
                a: bus(get, "a")?,
                x: bus(get, "x")?,
                y: bus(get, "y")?,
                s: bus(get, "s")?,
                p,
                pcl: bus(get, "pcl")?,
                pch: bus(get, "pch")?,
                ir: bus(get, "ir")?,
            })
        })()
        .ok();
        let clk0 = first(&["clk0", "phi0", "clk"])?;
        Ok(Pins {
            clk_in: nl.node("clk_in").unwrap_or(clk0),
            clk0,
            res: first(&["res", "reset"])?,
            rw: get("rw")?,
            sync: get("sync")?,
            rdy: get("rdy")?,
            irq: get("irq")?,
            nmi: get("nmi")?,
            so: get("so")?,
            ab: bus(get, "ab")?,
            db: bus(get, "db")?,
            regs,
        })
    }
}

/// A snapshot of the bus and register state, for tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub half_cycle: u64,
    pub clk0: bool,
    pub ab: u16,
    pub db: u8,
    pub rw: bool,
    pub sync: bool,
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub p: u8,
    pub ir: u8,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let flags: String = "NV-BDIZC"
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let bit = 7 - i;
                if bit == 5 {
                    '-'
                } else if self.p & (1 << bit) != 0 {
                    c
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        write!(
            f,
            "halfcyc:{} phi0:{} AB:{:04X} D:{:02X} RnW:{} SYNC:{} PC:{:04X} A:{:02X} X:{:02X} Y:{:02X} SP:{:02X} P:{:02X} {} IR:{:02X}",
            self.half_cycle,
            self.clk0 as u8,
            self.ab,
            self.db,
            self.rw as u8,
            self.sync as u8,
            self.pc,
            self.a,
            self.x,
            self.y,
            self.s,
            self.p,
            flags,
            self.ir
        )
    }
}

/// A transistor-level 6502 attached to 64 KiB of RAM.
pub struct Cpu6502 {
    pub sim: Sim,
    pub pins: Pins,
    pub memory: Box<[u8; 0x10000]>,
    /// Half-cycles since the end of reset.
    pub half_cycles: u64,
    /// Clock input toggles per CPU half-cycle, from [`Netlist::clock_ratio`].
    pub clock_ratio: u32,
    /// Called for every bus write with (address, value).
    /// `Send` so a whole `Cpu6502` can be moved to another thread.
    pub on_write: Option<Box<dyn FnMut(u16, u8) + Send>>,
}

impl Cpu6502 {
    pub fn new(nl: &Netlist) -> Result<Self, String> {
        let pins = Pins::from_netlist(nl)?;
        Ok(Cpu6502 {
            sim: Sim::new(nl),
            pins,
            memory: Box::new([0; 0x10000]),
            half_cycles: 0,
            clock_ratio: nl.clock_ratio.max(1),
            on_write: None,
        })
    }

    /// Power-on reset, as in Visual6502's `initChip()`.
    pub fn reset(&mut self) {
        let p = self.pins.clone();
        self.sim.reset_state();
        self.sim.drive(p.res, false);
        self.sim.drive(p.clk_in, false);
        self.sim.drive(p.rdy, true);
        self.sim.drive(p.so, false);
        self.sim.drive(p.irq, true);
        self.sim.drive(p.nmi, true);
        self.sim.recalc_all();
        for _ in 0..8 * self.clock_ratio {
            self.sim.set_high(p.clk_in);
            self.sim.set_low(p.clk_in);
        }
        self.sim.set_high(p.res);
        for _ in 0..18 {
            self.half_step();
        }
        self.half_cycles = 0;
    }

    /// Advance one CPU clock phase, servicing the bus: toggle the clock
    /// input `clock_ratio` times, then read or write memory according to the
    /// phase `clk0` ended up in.
    pub fn half_step(&mut self) {
        let clk_in = self.pins.clk_in;
        for _ in 0..self.clock_ratio {
            let high = self.sim.is_high(clk_in);
            self.sim.set(clk_in, !high);
        }
        if self.clk0() {
            self.handle_bus_write();
        } else {
            self.handle_bus_read();
        }
        self.half_cycles += 1;
    }

    /// Advance one full clock cycle.
    pub fn step(&mut self) {
        self.half_step();
        self.half_step();
    }

    fn handle_bus_read(&mut self) {
        if self.sim.is_high(self.pins.rw) {
            let a = self.address_bus();
            let d = self.memory[a as usize];
            self.write_data_bus(d);
        }
    }

    fn handle_bus_write(&mut self) {
        if !self.sim.is_high(self.pins.rw) {
            let a = self.address_bus();
            let d = self.data_bus();
            self.memory[a as usize] = d;
            if let Some(cb) = self.on_write.as_mut() {
                cb(a, d);
            }
        }
    }

    fn write_data_bus(&mut self, mut d: u8) {
        let db = self.pins.db;
        for &n in &db {
            self.sim.drive(n, d & 1 != 0);
            d >>= 1;
        }
        self.sim.recalc(&db);
    }

    fn read_bits(&self, nodes: &[NodeId]) -> u16 {
        nodes
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &n)| acc | ((self.sim.is_high(n) as u16) << i))
    }

    pub fn address_bus(&self) -> u16 {
        self.read_bits(&self.pins.ab)
    }
    pub fn data_bus(&self) -> u8 {
        self.read_bits(&self.pins.db) as u8
    }
    pub fn rw(&self) -> bool {
        self.sim.is_high(self.pins.rw)
    }
    pub fn sync(&self) -> bool {
        self.sim.is_high(self.pins.sync)
    }
    pub fn clk0(&self) -> bool {
        self.sim.is_high(self.pins.clk0)
    }

    pub fn set_irq(&mut self, asserted: bool) {
        self.sim.set(self.pins.irq, !asserted);
    }
    pub fn set_nmi(&mut self, asserted: bool) {
        self.sim.set(self.pins.nmi, !asserted);
    }
    pub fn set_rdy(&mut self, high: bool) {
        self.sim.set(self.pins.rdy, high);
    }
    pub fn set_so(&mut self, high: bool) {
        self.sim.set(self.pins.so, high);
    }

    fn reg8(&self, f: impl Fn(&Regs) -> [NodeId; 8]) -> u8 {
        self.pins
            .regs
            .as_ref()
            .map(|r| self.read_bits(&f(r)) as u8)
            .unwrap_or(0)
    }
    pub fn pc(&self) -> u16 {
        (self.reg8(|r| r.pch) as u16) << 8 | self.reg8(|r| r.pcl) as u16
    }
    pub fn a(&self) -> u8 {
        self.reg8(|r| r.a)
    }
    pub fn x(&self) -> u8 {
        self.reg8(|r| r.x)
    }
    pub fn y(&self) -> u8 {
        self.reg8(|r| r.y)
    }
    pub fn sp(&self) -> u8 {
        self.reg8(|r| r.s)
    }
    pub fn ir(&self) -> u8 {
        self.reg8(|r| r.ir)
    }
    pub fn p(&self) -> u8 {
        let Some(r) = &self.pins.regs else { return 0 };
        r.p.iter().enumerate().fold(0, |acc, (i, n)| {
            acc | (n.is_some_and(|n| self.sim.is_high(n)) as u8) << i
        })
    }

    pub fn status(&self) -> Status {
        Status {
            half_cycle: self.half_cycles,
            clk0: self.clk0(),
            ab: self.address_bus(),
            db: self.data_bus(),
            rw: self.rw(),
            sync: self.sync(),
            pc: self.pc(),
            a: self.a(),
            x: self.x(),
            y: self.y(),
            s: self.sp(),
            p: self.p(),
            ir: self.ir(),
        }
    }

    /// Run until an opcode fetch (SYNC high during phi2 is reached), returning
    /// the address of the opcode. Gives up after `max_half_cycles`.
    pub fn run_to_sync(&mut self, max_half_cycles: u64) -> Option<u16> {
        for _ in 0..max_half_cycles {
            self.half_step();
            if self.clk0() && self.sync() {
                return Some(self.address_bus());
            }
        }
        None
    }

    pub fn load(&mut self, addr: u16, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.memory[(addr as usize + i) & 0xffff] = b;
        }
    }
}
