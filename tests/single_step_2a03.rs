//! Per-opcode tests from Tom Harte's SingleStepTests for the Ricoh 2A03 (the
//! NES's 6502 core, without decimal mode)
//! (<https://github.com/SingleStepTests/65x02>, `nes6502/v1/*.json`), run on
//! the transistor-level model in `netlists/ricoh_2a03.netlist`. The data comes
//! from the `tests/65x02` submodule:
//!
//!     git submodule update --init tests/65x02
//!     cargo test --release --test single_step_2a03
//!
//! The harness is shared with `single_step.rs`; see `common/mod.rs` for how
//! each case is set up and checked.

#[macro_use]
mod common;

use std::path::PathBuf;

use common::Case;
use rustic6502::Netlist;

const NETLIST: &str = include_str!("../netlists/ricoh_2a03.netlist");

/// The 2A03 handles `$4000-$401F` on-chip (APU, OAM DMA, controller ports),
/// meaning these processes cannot be isolated from the CPU in hardware. The
/// presence of these addresses in test data suggests that they were generated
/// by an emulator, and not run on actual hardware.
/// 
/// Because the number of tests doing this is small, and that most of them
/// cover behavior already tested elsewhere, they're not useful to support.
/// Hence, drop them from the test suite with this helper method
/// 
/// cf. https://github.com/SingleStepTests/65x02/issues/11
fn touches_io(case: &Case) -> bool {
    case.cycles.iter().any(|(addr, _, _)| (0x4000..=0x401f).contains(addr))
}

fn run(op: u8) {
    let netlist = Netlist::from_native(NETLIST).expect("2A03 netlist is valid");
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/65x02/nes6502/v1");
    common::run_opcode(&netlist, &data_dir, op, touches_io);
}

opcode_tests!(run);

// Unofficial opcodes that depend on analog behavior the netlist doesn't
// reproduce. Ignored by default; run with
// `cargo test --release --test single_step_2a03 -- --ignored`. Pass rates are
// over the cases that remain after `touches_io` skips the `$4000-$401F` ones
// (6 of 10,000 for `$8B`, 3 for `$AB`).

/// XAA/ANE #imm. Pass rate as of 2026-11-22: 44.28% (4425/9994).
#[test]
#[ignore = "known failing: 44.28% pass rate"]
fn op_8b() {
    run(0x8b);
}

/// LXA/LAX #imm. Pass rate as of 2026-11-22: 17.75% (1774/9997).
#[test]
#[ignore = "known failing: 17.75% pass rate"]
fn op_ab() {
    run(0xab);
}
