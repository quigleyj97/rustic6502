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
//! The netlist has no way to load registers directly, so each case first
//! runs a short setup program from a scratch memory image:
//!
//!     LDX #s; TXS; LDA #p; PHA; LDA #a; LDX #x; LDY #y; PLP; JMP pc
//!
//! A bus cycle starts when `clk0` falls: address, R/W and SYNC change and a
//! read is sampled straight away; a write's data is driven and committed
//! when `clk0` rises. So memory must be swapped on the rising edge *before*
//! a fetch. Once the JMP's last operand is read the scratch image is swapped
//! for the case's RAM, and from then on every bus cycle is checked against
//! the case's `cycles` list. Registers are read once the following opcode fetch
//! has retired the instruction (the 6502 finishes register writes during the
//! next fetch).

use std::path::PathBuf;

use rustic6502::{Cpu6502, Sim, builtin_netlist};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    initial: State,
    #[serde(rename = "final")]
    expected: State,
    cycles: Vec<(u16, u8, String)>,
}

#[derive(Deserialize)]
struct State {
    pc: u16,
    s: u8,
    a: u8,
    x: u8,
    y: u8,
    p: u8,
    ram: Vec<(u16, u8)>,
}

/// Bits 4 (B) and 5 have no storage in the chip; they only exist on the
/// stack, and the stack is covered by the RAM comparison.
const P_MASK: u8 = 0xcf;

const SETUP: u16 = 0x0200;
const SETUP_JMP: u16 = SETUP + 13;

/// JAM (KIL/HLT) opcodes lock the CPU up: after the cycles in the test it
/// keeps reading `$FFFF` and never fetches another opcode.
const JAM: [u8; 12] = [
    0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xb2, 0xd2, 0xf2,
];
/// How long a jammed CPU must stay jammed for its test to pass.
const JAM_TIMEOUT_CYCLES: usize = 100;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/65x02/6502/v1")
}

/// A CPU just out of reset, kept so each case can start from a clone of it.
struct Harness {
    cpu: Cpu6502,
    template: Sim,
    scratch: Box<[u8; 0x10000]>,
}

impl Harness {
    fn new() -> Self {
        let mut cpu = Cpu6502::new(&builtin_netlist()).unwrap();
        cpu.memory[0xfffc] = SETUP as u8;
        cpu.memory[0xfffd] = (SETUP >> 8) as u8;
        // reset()'s last half-step is the falling edge that fetches the first
        // setup opcode, so that byte is read before we can swap memory. It is
        // always LDX #, so plant it now; the rest comes from each case's
        // scratch image. Snapshot right after, so every case replays the setup
        // program from the same point.
        cpu.memory[SETUP as usize] = 0xa2;
        cpu.reset();
        assert!(
            !cpu.clk0() && cpu.sync() && cpu.address_bus() == SETUP,
            "reset did not end fetching the setup program: {}",
            cpu.status()
        );
        let template = cpu.sim.clone();
        let scratch = Box::new(*cpu.memory);
        Harness {
            cpu,
            template,
            scratch,
        }
    }

    /// Next rising clock edge with SYNC high; returns the opcode address.
    fn to_sync(&mut self) -> u16 {
        self.cpu.run_to_sync(200).expect("no opcode fetch")
    }

    /// Run one case; returns a description of the first mismatch.
    fn run(&mut self, op: u8, case: &Case) -> Result<(), String> {
        let i = &case.initial;
        let cpu = &mut self.cpu;
        cpu.sim.clone_from(&self.template);
        cpu.half_cycles = 0;

        #[rustfmt::skip]
        let setup = [
            0xa2, i.s, 0x9a,             // LDX #s; TXS
            0xa9, i.p, 0x48,             // LDA #p; PHA
            0xa9, i.a, 0xa2, i.x, 0xa0, i.y, // LDA #a; LDX #x; LDY #y
            0x28,                        // PLP
            0x4c, i.pc as u8, (i.pc >> 8) as u8, // JMP pc
        ];
        self.scratch[SETUP as usize..][..setup.len()].copy_from_slice(&setup);
        std::mem::swap(&mut cpu.memory, &mut self.scratch);
        // The first fetch must be the setup program's first opcode, and the
        // JMP is its ninth instruction; bail out rather than spin if not.
        let first = self.to_sync();
        assert_eq!(first, SETUP, "first fetch after snapshot is not the setup program");
        let mut fetches = vec![first];
        while *fetches.last().unwrap() != SETUP_JMP {
            assert!(fetches.len() < 9, "setup program never reached its JMP: fetched {fetches:04x?}");
            fetches.push(self.to_sync());
        }
        // Two more cycles read the JMP's operands; the next falling edge
        // fetches the opcode at pc.
        let cpu = &mut self.cpu;
        cpu.step();
        cpu.step();
        assert_eq!(cpu.address_bus(), SETUP_JMP + 2, "setup JMP misread");
        std::mem::swap(&mut cpu.memory, &mut self.scratch);

        cpu.memory.fill(0);
        for &(addr, v) in &i.ram {
            cpu.memory[addr as usize] = v;
        }

        // Each cycle: the falling edge sets address and R/W and samples a
        // read; the rising edge drives and commits a write.
        for (n, (addr, value, kind)) in case.cycles.iter().enumerate() {
            cpu.half_step();
            assert!(!cpu.clk0());
            let ab = cpu.address_bus();
            let read = cpu.rw();
            let d = if read {
                let d = cpu.data_bus();
                cpu.half_step();
                d
            } else {
                cpu.half_step();
                cpu.data_bus()
            };
            let got_kind = if read { "read" } else { "write" };
            if (ab, d, got_kind) != (*addr, *value, kind.as_str()) {
                return Err(format!(
                    "cycle {n}: got [{ab}, {d}, {got_kind}], expected [{addr}, {value}, {kind}]"
                ));
            }
        }

        let f = &case.expected;
        let pc = if JAM.contains(&op) {
            // Stay jammed for the timeout: no opcode fetch, only reads of
            // $FFFF. The test data gives the final PC as the byte after the
            // opcode (the one read in cycle 2), but the chip's PC register has
            // already been incremented past it, so it reads one higher.
            for n in 0..JAM_TIMEOUT_CYCLES {
                cpu.step();
                if cpu.sync() || !cpu.rw() || cpu.address_bus() != 0xffff {
                    return Err(format!(
                        "not jammed {n} cycles after the test's last cycle: {}",
                        cpu.status()
                    ));
                }
            }
            cpu.pc().wrapping_sub(1)
        } else {
            // The next cycle should be the following opcode fetch; let it run
            // one more cycle so the instruction's register writes complete.
            cpu.half_step();
            if !cpu.sync() {
                return Err(format!(
                    "no opcode fetch after {} cycles: {}",
                    case.cycles.len(),
                    cpu.status()
                ));
            }
            let pc = cpu.address_bus();
            cpu.half_step();
            cpu.step();
            pc
        };

        let got = (pc, cpu.sp(), cpu.a(), cpu.x(), cpu.y(), cpu.p() & P_MASK);
        let want = (f.pc, f.s, f.a, f.x, f.y, f.p & P_MASK);
        if got != want {
            return Err(format!(
                "final (pc, s, a, x, y, p): got {got:02x?}, expected {want:02x?}"
            ));
        }
        for &(addr, v) in &f.ram {
            let m = cpu.memory[addr as usize];
            if m != v {
                return Err(format!("ram[{addr}]: got {m}, expected {v}"));
            }
        }
        Ok(())
    }
}

fn run_opcode(op: u8) {
    let path = data_dir().join(format!("{op:02x}.json"));
    let Ok(text) = std::fs::read(&path) else {
        panic!(
            "{} not found; run `git submodule update --init tests/65x02`",
            path.display()
        );
    };
    let mut cases: Vec<Case> = serde_json::from_slice(&text).unwrap();
    if let Some(n) = std::env::var("SST_LIMIT").ok().and_then(|s| s.parse().ok()) {
        cases.truncate(n);
    }
    let verbose = std::env::var_os("SST_VERBOSE").is_some();

    let mut h = Harness::new();
    let mut failures = Vec::new();
    for case in &cases {
        if verbose {
            eprint!("{op:02x} \"{}\": ", case.name);
        }
        if let Err(e) = h.run(op, case) {
            if verbose {
                eprintln!("FAIL: {e}");
            }
            failures.push((&case.name, e));
        } else if verbose {
            eprintln!("PASS");
        }
    }
    if let Some((name, e)) = failures.first() {
        let total = cases.len();
        let passed = total - failures.len();
        let rate = 100.0 * passed as f64 / total as f64;
        panic!(
            "opcode ${op:02X}: pass rate {rate:.2}% ({passed}/{total} passed, {} failed); first failure was \"{name}\": {e}",
            failures.len(),
        );
    }
}

macro_rules! opcodes {
    ($($name:ident = $op:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                run_opcode($op);
            }
        )*
    };
}

opcodes! {
    op_00 = 0x00, op_01 = 0x01, op_02 = 0x02, op_03 = 0x03, op_04 = 0x04, op_05 = 0x05, op_06 = 0x06, op_07 = 0x07,
    op_08 = 0x08, op_09 = 0x09, op_0a = 0x0a, op_0b = 0x0b, op_0c = 0x0c, op_0d = 0x0d, op_0e = 0x0e, op_0f = 0x0f,
    op_10 = 0x10, op_11 = 0x11, op_12 = 0x12, op_13 = 0x13, op_14 = 0x14, op_15 = 0x15, op_16 = 0x16, op_17 = 0x17,
    op_18 = 0x18, op_19 = 0x19, op_1a = 0x1a, op_1b = 0x1b, op_1c = 0x1c, op_1d = 0x1d, op_1e = 0x1e, op_1f = 0x1f,
    op_20 = 0x20, op_21 = 0x21, op_22 = 0x22, op_23 = 0x23, op_24 = 0x24, op_25 = 0x25, op_26 = 0x26, op_27 = 0x27,
    op_28 = 0x28, op_29 = 0x29, op_2a = 0x2a, op_2b = 0x2b, op_2c = 0x2c, op_2d = 0x2d, op_2e = 0x2e, op_2f = 0x2f,
    op_30 = 0x30, op_31 = 0x31, op_32 = 0x32, op_33 = 0x33, op_34 = 0x34, op_35 = 0x35, op_36 = 0x36, op_37 = 0x37,
    op_38 = 0x38, op_39 = 0x39, op_3a = 0x3a, op_3b = 0x3b, op_3c = 0x3c, op_3d = 0x3d, op_3e = 0x3e, op_3f = 0x3f,
    op_40 = 0x40, op_41 = 0x41, op_42 = 0x42, op_43 = 0x43, op_44 = 0x44, op_45 = 0x45, op_46 = 0x46, op_47 = 0x47,
    op_48 = 0x48, op_49 = 0x49, op_4a = 0x4a, op_4b = 0x4b, op_4c = 0x4c, op_4d = 0x4d, op_4e = 0x4e, op_4f = 0x4f,
    op_50 = 0x50, op_51 = 0x51, op_52 = 0x52, op_53 = 0x53, op_54 = 0x54, op_55 = 0x55, op_56 = 0x56, op_57 = 0x57,
    op_58 = 0x58, op_59 = 0x59, op_5a = 0x5a, op_5b = 0x5b, op_5c = 0x5c, op_5d = 0x5d, op_5e = 0x5e, op_5f = 0x5f,
    op_60 = 0x60, op_61 = 0x61, op_62 = 0x62, op_63 = 0x63, op_64 = 0x64, op_65 = 0x65, op_66 = 0x66, op_67 = 0x67,
    op_68 = 0x68, op_69 = 0x69, op_6a = 0x6a, op_6b = 0x6b, op_6c = 0x6c, op_6d = 0x6d, op_6e = 0x6e, op_6f = 0x6f,
    op_70 = 0x70, op_71 = 0x71, op_72 = 0x72, op_73 = 0x73, op_74 = 0x74, op_75 = 0x75, op_76 = 0x76, op_77 = 0x77,
    op_78 = 0x78, op_79 = 0x79, op_7a = 0x7a, op_7b = 0x7b, op_7c = 0x7c, op_7d = 0x7d, op_7e = 0x7e, op_7f = 0x7f,
    op_80 = 0x80, op_81 = 0x81, op_82 = 0x82, op_83 = 0x83, op_84 = 0x84, op_85 = 0x85, op_86 = 0x86, op_87 = 0x87,
    op_88 = 0x88, op_89 = 0x89, op_8a = 0x8a, op_8c = 0x8c, op_8d = 0x8d, op_8e = 0x8e, op_8f = 0x8f,
    op_90 = 0x90, op_91 = 0x91, op_92 = 0x92, op_93 = 0x93, op_94 = 0x94, op_95 = 0x95, op_96 = 0x96, op_97 = 0x97,
    op_98 = 0x98, op_99 = 0x99, op_9a = 0x9a, op_9b = 0x9b, op_9c = 0x9c, op_9d = 0x9d, op_9e = 0x9e, op_9f = 0x9f,
    op_a0 = 0xa0, op_a1 = 0xa1, op_a2 = 0xa2, op_a3 = 0xa3, op_a4 = 0xa4, op_a5 = 0xa5, op_a6 = 0xa6, op_a7 = 0xa7,
    op_a8 = 0xa8, op_a9 = 0xa9, op_aa = 0xaa, op_ac = 0xac, op_ad = 0xad, op_ae = 0xae, op_af = 0xaf,
    op_b0 = 0xb0, op_b1 = 0xb1, op_b2 = 0xb2, op_b3 = 0xb3, op_b4 = 0xb4, op_b5 = 0xb5, op_b6 = 0xb6, op_b7 = 0xb7,
    op_b8 = 0xb8, op_b9 = 0xb9, op_ba = 0xba, op_bb = 0xbb, op_bc = 0xbc, op_bd = 0xbd, op_be = 0xbe, op_bf = 0xbf,
    op_c0 = 0xc0, op_c1 = 0xc1, op_c2 = 0xc2, op_c3 = 0xc3, op_c4 = 0xc4, op_c5 = 0xc5, op_c6 = 0xc6, op_c7 = 0xc7,
    op_c8 = 0xc8, op_c9 = 0xc9, op_ca = 0xca, op_cb = 0xcb, op_cc = 0xcc, op_cd = 0xcd, op_ce = 0xce, op_cf = 0xcf,
    op_d0 = 0xd0, op_d1 = 0xd1, op_d2 = 0xd2, op_d3 = 0xd3, op_d4 = 0xd4, op_d5 = 0xd5, op_d6 = 0xd6, op_d7 = 0xd7,
    op_d8 = 0xd8, op_d9 = 0xd9, op_da = 0xda, op_db = 0xdb, op_dc = 0xdc, op_dd = 0xdd, op_de = 0xde, op_df = 0xdf,
    op_e0 = 0xe0, op_e1 = 0xe1, op_e2 = 0xe2, op_e3 = 0xe3, op_e4 = 0xe4, op_e5 = 0xe5, op_e6 = 0xe6, op_e7 = 0xe7,
    op_e8 = 0xe8, op_e9 = 0xe9, op_ea = 0xea, op_eb = 0xeb, op_ec = 0xec, op_ed = 0xed, op_ee = 0xee, op_ef = 0xef,
    op_f0 = 0xf0, op_f1 = 0xf1, op_f2 = 0xf2, op_f3 = 0xf3, op_f4 = 0xf4, op_f5 = 0xf5, op_f6 = 0xf6, op_f7 = 0xf7,
    op_f8 = 0xf8, op_f9 = 0xf9, op_fa = 0xfa, op_fb = 0xfb, op_fc = 0xfc, op_fd = 0xfd, op_fe = 0xfe, op_ff = 0xff,
}

// Unofficial opcodes whose analog behavior the netlist doesn't match the test
// data on. Ignored by default; run with `cargo test --test single_step -- --ignored`.

/// XAA/ANE #imm. Pass rate as of 2026-11-21: 45.38% (4538/10000).
#[test]
#[ignore = "known failing: 45.38% pass rate"]
fn op_8b() {
    run_opcode(0x8b);
}

/// LXA/LAX #imm. Pass rate as of 2026-11-21: 18.43% (1843/10000).
#[test]
#[ignore = "known failing: 18.43% pass rate"]
fn op_ab() {
    run_opcode(0xab);
}
