//! Benchmark matching perfect6502's `make benchmark`: boot Commodore 64 BASIC
//! V2 on the simulated chip and stop the clock when BASIC first asks the
//! KERNAL for input (CHRIN at $FFCF), i.e. at the `READY.` prompt.
//!
//! Usage:
//!   cargo run --release --example bench -- [path/to/cbmbasic.bin]
//!
//! The ROM image is the one shipped with perfect6502 (`cbmbasic/cbmbasic.bin`,
//! 17591 bytes loaded at $A000); by default it's read from a sibling checkout
//! at `../perfect6502`.
//!
//! As in perfect6502, the KERNAL jump table at $FF90-$FFF2 is filled with
//! `JMP $F800`. When the CPU fetches an opcode from a table entry, the call is
//! serviced on the host and a stub at $F800 loads the result registers and
//! returns. Only the calls BASIC makes on the way to `READY.` do anything.

use std::io::Write as _;
use std::time::Instant;

use rustic6502::Cpu6502;

const ROM_ADDR: u16 = 0xA000;
const ROM_LEN: usize = 17591;
const RAM_BOT: u16 = 0x0800;
const RAM_TOP: u16 = 0xA000;
const STUB: u16 = 0xF800;
const CHRIN: u16 = 0xFFCF;
/// Give up if READY. hasn't been reached by now (perfect6502 needs 33155).
const MAX_HALF_CYCLES: u64 = 1_000_000;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../perfect6502/cbmbasic/cbmbasic.bin".into());
    let rom = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("Error opening {path}: {e}");
        std::process::exit(1);
    });
    if rom.len() != ROM_LEN {
        eprintln!("{path}: expected {ROM_LEN} bytes, got {}", rom.len());
        std::process::exit(1);
    }

    let mut cpu = Cpu6502::new(&rustic6502::builtin_netlist()).unwrap();
    cpu.load(ROM_ADDR, &rom);
    for addr in (0xFF90..0xFFF3).step_by(3) {
        cpu.load(addr, &[0x4C, STUB as u8, (STUB >> 8) as u8]); // JMP $F800
    }
    // BASIC scribbles over $01FE/$01FF, so RESET goes through a JSR to the
    // cold start entry at $E394 rather than jumping straight there.
    cpu.load(0xF000, &[0x20, 0x94, 0xE3]); // JSR $E394
    cpu.load(0xFFFC, &[0x00, 0xF0]);

    // `reset()` fetches the reset vector, so memory has to be set up first.
    cpu.reset();

    // perfect6502 starts its clock after chip setup and reset, so do we.
    // (Our reset also runs the first 18 half-cycles of the reset sequence,
    // which perfect6502 counts; that's ~0.05% of the run.)
    let start = Instant::now();

    let mut out = std::io::stdout().lock();
    while cpu.half_cycles < MAX_HALF_CYCLES {
        cpu.half_step();
        // Check at the start of phi1 of each opcode fetch.
        if cpu.clk0() || !cpu.sync() {
            continue;
        }
        let pc = cpu.address_bus();
        if !(0xFF90..0xFFF3).contains(&pc) || !(pc - 0xFF90).is_multiple_of(3) {
            continue;
        }
        if pc == CHRIN {
            let secs = start.elapsed().as_secs_f64();
            out.flush().ok();
            drop(out);
            println!("Benchmark results:");
            println!("  Half-cycles: {}", cpu.half_cycles);
            println!("  Time: {secs:.3} seconds");
            println!(
                "  Performance: {:.0} cycles/sec",
                cpu.half_cycles as f64 / secs
            );
            println!("{}", cpu.status());
            return;
        }
        kernal_call(&mut cpu, pc, &mut out);
    }
    out.flush().ok();
    eprintln!("READY. not reached after {MAX_HALF_CYCLES} half-cycles");
    eprintln!("{}", cpu.status());
    std::process::exit(1);
}

/// Service a KERNAL call and write a stub at $F800 that loads the results
/// into the registers and returns to the caller.
fn kernal_call(cpu: &mut Cpu6502, pc: u16, out: &mut impl std::io::Write) {
    let (mut a, mut x, mut y, mut p) = (cpu.a(), cpu.x(), cpu.y(), cpu.p());
    let mut c = p & 1 != 0;
    match pc {
        0xFF90 => a = 0,                          // SETMSG: return status
        0xFF99 => [x, y] = RAM_TOP.to_le_bytes(), // MEMTOP
        0xFF9C => [x, y] = RAM_BOT.to_le_bytes(), // MEMBOT
        0xFFB7 => a = 0,                          // READST
        0xFFD2 => {
            // CHROUT
            match a {
                13 => out.write_all(b"\n").unwrap(),
                b' '..=b'~' => out.write_all(&[a]).unwrap(),
                _ => {} // colour, cursor and clear-screen codes
            }
            c = false;
        }
        0xFFF0 if c => (x, y) = (0, 0), // PLOT (read cursor)
        _ => {}                         // CLRCHN etc.: nothing to do
    }
    p = (p & !1) | c as u8;
    cpu.load(
        STUB,
        &[
            0xA9, p, 0x48, // LDA #P; PHA
            0xA9, a, // LDA #A
            0xA2, x, // LDX #X
            0xA0, y,    // LDY #Y
            0x28, // PLP
            0x60, // RTS
        ],
    );
}
