//! A transistor-level 6502 simulator driven by the Visual6502 netlist.
//!
//! * [`netlist`] loads netlists (Visual6502 JS data or a compact native
//!   format), so other 6502 variants can be plugged in.
//! * [`sim`] is the switch-level simulation engine.
//! * [`cpu`] clocks a 6502-compatible netlist against 64 KiB of memory.
//! * [`trace`] records chosen nodes over time (VCD or a plain table), to
//!   check another implementation of the chip against this one.

pub mod cpu;
pub mod netlist;
pub mod sim;
pub mod trace;

pub use cpu::{Cpu6502, Status};
pub use netlist::Netlist;
pub use sim::Sim;
pub use trace::{NodeTracer, Probe, TraceFormat};

/// The netlist compiled into this binary, in native format: the NMOS 6502
/// from visual6502.org (`netlists/6502.netlist`).
pub const BUILTIN_NETLIST: &str = include_str!("../netlists/6502.netlist");

/// Parse [`BUILTIN_NETLIST`].
pub fn builtin_netlist() -> Netlist {
    Netlist::from_native(BUILTIN_NETLIST).expect("built-in netlist is valid")
}
