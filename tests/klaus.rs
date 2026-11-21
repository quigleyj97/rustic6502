//! Klaus Dormann's 6502 functional test, run on the transistor-level model.
//!
//! The test program is GPL-3.0-or-later and is not part of this repository;
//! it comes from the `tests/6502_65C02_functional_tests` git submodule:
//!
//!     git submodule update --init
//!     cargo test --release --test klaus -- --ignored
//!
//! It takes about 18 minutes, so it is `#[ignore]`d by default.

use std::path::Path;

use rustic6502::{Cpu6502, builtin_netlist};

const BIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/6502_65C02_functional_tests/bin_files/6502_functional_test.bin"
);
/// The `jmp *` success trap in the prebuilt binary's listing.
const SUCCESS_PC: u16 = 0x3469;
/// Instruction-level emulators report the same count.
const EXPECTED_CYCLES: u64 = 96_241_367;
const MAX_CYCLES: u64 = 100_000_000;

#[test]
#[ignore = "takes ~18 minutes; needs the functional-test submodule"]
fn klaus_functional_test() {
    let path = Path::new(BIN);
    let Ok(image) = std::fs::read(path) else {
        panic!(
            "{} not found; run `git submodule update --init`",
            path.display()
        );
    };
    assert_eq!(image.len(), 0x10000, "expected a full 64 KiB image");

    let mut cpu = Cpu6502::new(&builtin_netlist()).unwrap();
    cpu.load(0x0000, &image);
    cpu.load(0xfffc, &[0x00, 0x04]); // start at the code segment, $0400
    cpu.reset();
    let start = cpu.half_cycles;

    // Every outcome ends in a JMP-to-self or branch-to-self, which fetches
    // the same opcode again within a few cycles.
    let mut last: Option<(u16, u64)> = None;
    let trap = loop {
        let pc = cpu.run_to_sync(MAX_CYCLES * 2).expect("no opcode fetch");
        if let Some((prev, at)) = last
            && prev == pc
            && cpu.half_cycles - at <= 8
        {
            break pc;
        }
        last = Some((pc, cpu.half_cycles));
        assert!(
            cpu.half_cycles - start < MAX_CYCLES * 2,
            "no trap after {MAX_CYCLES} cycles; PC ${pc:04X}"
        );
    };

    let cycles = (cpu.half_cycles - start) / 2;
    assert_eq!(
        trap,
        SUCCESS_PC,
        "failure trap at ${trap:04X} after {cycles} cycles (see \
         bin_files/6502_functional_test.lst); {}",
        cpu.status()
    );
    assert_eq!(cycles, EXPECTED_CYCLES, "cycle count to the success trap");
}
