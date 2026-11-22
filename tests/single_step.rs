//! Per-opcode tests from Tom Harte's SingleStepTests for the NMOS 6502
//! (<https://github.com/SingleStepTests/65x02>, `6502/v1/*.json`), run on the
//! transistor-level model. The data comes from the `tests/65x02` submodule:
//!
//!     git submodule update --init tests/65x02
//!     cargo test --test single_step
//!
//! Each opcode's file holds 10,000 cases. Set `SST_LIMIT=n` to run only the
//! first `n` of each, and `SST_VERBOSE=1` to print every failing case.
//!
//! The harness itself lives in `common/mod.rs`, shared with the 2A03 tests.

#[macro_use]
mod common;

use std::path::PathBuf;

use rustic6502::builtin_netlist;

fn run(op: u8) {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/65x02/6502/v1");
    common::run_opcode(&builtin_netlist(), &data_dir, op, |_| false);
}

opcode_tests!(run);

// Unofficial opcodes whose analog behavior the netlist doesn't match the test
// data on. Ignored by default; run with `cargo test --test single_step -- --ignored`.

/// XAA/ANE #imm. Pass rate as of 2026-11-21: 45.38% (4538/10000).
#[test]
#[ignore = "known failing: 45.38% pass rate"]
fn op_8b() {
    run(0x8b);
}

/// LXA/LAX #imm. Pass rate as of 2026-11-21: 18.43% (1843/10000).
#[test]
#[ignore = "known failing: 18.43% pass rate"]
fn op_ab() {
    run(0xab);
}
