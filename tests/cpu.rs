use rustic6502::{Cpu6502, Netlist, builtin_netlist};

fn cpu_with(program: &[u8]) -> Cpu6502 {
    let mut cpu = Cpu6502::new(&builtin_netlist()).unwrap();
    cpu.load(0x0000, program);
    cpu.reset();
    cpu
}

/// Run until an opcode is fetched from `pc`; returns the number of cycles.
fn run_until(cpu: &mut Cpu6502, pc: u16, max_cycles: u64) -> u64 {
    let start = cpu.half_cycles;
    loop {
        let at = cpu.run_to_sync(max_cycles * 2).expect("no opcode fetch");
        if at == pc {
            return (cpu.half_cycles - start) / 2;
        }
        assert!(
            cpu.half_cycles - start < max_cycles * 2,
            "did not reach ${pc:04X}"
        );
    }
}

#[test]
fn loads_stores_and_adds() {
    // LDA #$42; LDX #$05; STX $11; ADC #$10; STA $10; JMP *
    let mut cpu = cpu_with(&[
        0xa9, 0x42, 0xa2, 0x05, 0x8e, 0x11, 0x00, 0x69, 0x10, 0x85, 0x10, 0x4c, 0x0b, 0x00,
    ]);
    run_until(&mut cpu, 0x000b, 100);
    run_until(&mut cpu, 0x000b, 100); // let STA retire (registers update after fetch)
    assert_eq!(cpu.memory[0x10], 0x52);
    assert_eq!(cpu.memory[0x11], 0x05);
    assert_eq!(cpu.a(), 0x52);
    assert_eq!(cpu.x(), 0x05);
}

#[test]
fn instruction_timing() {
    // NOP (2); LDA $1234,X with page cross (5); JSR sub (6); ... sub: RTS (6)
    let prog = [
        0xea, // 0000 NOP
        0xa2, 0xff, // 0001 LDX #$ff        2
        0xbd, 0x01, 0x12, // 0003 LDA $1201,X 5 (page cross)
        0x20, 0x10, 0x00, // 0006 JSR $0010  6
        0x4c, 0x09, 0x00, // 0009 JMP *
        0, 0, 0, 0,    //
        0x60, // 0010 RTS 6
    ];
    let mut cpu = cpu_with(&prog);
    run_until(&mut cpu, 0x0000, 20);
    assert_eq!(run_until(&mut cpu, 0x0001, 20), 2);
    assert_eq!(run_until(&mut cpu, 0x0003, 20), 2);
    assert_eq!(run_until(&mut cpu, 0x0006, 20), 5);
    assert_eq!(run_until(&mut cpu, 0x0010, 20), 6);
    assert_eq!(run_until(&mut cpu, 0x0009, 20), 6);
}

#[test]
fn decimal_mode_and_flags() {
    // SED; CLC; LDA #$19; ADC #$28; STA $20; PHP; PLA; STA $21; JMP *
    let prog = [
        0xf8, 0x18, 0xa9, 0x19, 0x69, 0x28, 0x85, 0x20, 0x08, 0x68, 0x85, 0x21, 0x4c, 0x0c, 0x00,
    ];
    let mut cpu = cpu_with(&prog);
    run_until(&mut cpu, 0x000c, 200);
    assert_eq!(cpu.memory[0x20], 0x47);
    assert_eq!(cpu.memory[0x21] & 0x08, 0x08, "D flag pushed");
    assert_eq!(
        cpu.memory[0x21] & 0x30,
        0x30,
        "B and bit 5 set when pushed by PHP"
    );
}

#[test]
fn nmi_is_taken() {
    let mut prog = vec![0u8; 0x10000];
    prog[0x0000..0x0003].copy_from_slice(&[0x4c, 0x00, 0x00]); // JMP *
    prog[0x0200..0x0205].copy_from_slice(&[0xe6, 0x30, 0x40, 0, 0]); // INC $30; RTI
    prog[0xfffa..0xfffc].copy_from_slice(&[0x00, 0x02]);
    let mut cpu = Cpu6502::new(&builtin_netlist()).unwrap();
    cpu.load(0, &prog);
    cpu.reset();
    run_until(&mut cpu, 0x0000, 50);
    cpu.set_nmi(true);
    run_until(&mut cpu, 0x0200, 50);
    cpu.set_nmi(false);
    run_until(&mut cpu, 0x0000, 50);
    assert_eq!(cpu.memory[0x30], 1);
    assert_eq!(cpu.sp(), 0xfd, "stack balanced after RTI");
}

#[test]
fn irq_respects_i_flag() {
    let mut prog = vec![0u8; 0x10000];
    // CLI; JMP $0001
    prog[0x0000..0x0004].copy_from_slice(&[0x58, 0x4c, 0x01, 0x00]);
    prog[0x0300..0x0303].copy_from_slice(&[0xe6, 0x31, 0x40]); // INC $31; RTI
    prog[0xfffe..0x10000].copy_from_slice(&[0x00, 0x03]);
    let mut cpu = Cpu6502::new(&builtin_netlist()).unwrap();
    cpu.load(0, &prog);
    cpu.reset();
    cpu.set_irq(true); // asserted, but I is set until CLI runs
    run_until(&mut cpu, 0x0300, 100);
    cpu.set_irq(false);
    run_until(&mut cpu, 0x0001, 50);
    assert_eq!(cpu.memory[0x31], 1);
}

#[test]
fn native_round_trip_is_lossless() {
    let nl = builtin_netlist();
    assert_eq!(nl.node_count, 1725);
    assert_eq!(nl.transistors.len(), 3510);
    assert_eq!(Netlist::from_native(&nl.to_native()).unwrap(), nl);
}

/// A "variant" whose pads use different names, adapted with `alias` lines,
/// runs identically: the CPU driver only depends on names.
#[test]
fn pluggable_variant_with_aliases() {
    let mut text = String::new();
    for line in rustic6502::BUILTIN_NETLIST.lines() {
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["n", "res", n] => text.push_str(&format!("n reset {n}\n")),
            ["n", "clk0", n] => text.push_str(&format!("n phi0 {n}\n")),
            _ => {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    let nl = Netlist::from_native(&text).unwrap();
    assert!(nl.node("res").is_none());
    let mut cpu = Cpu6502::new(&nl).unwrap(); // finds `reset`/`phi0` fallbacks
    cpu.load(0, &[0xa9, 0x99, 0x85, 0x40, 0x4c, 0x04, 0x00]);
    cpu.reset();
    run_until(&mut cpu, 0x0004, 50);
    run_until(&mut cpu, 0x0004, 50);
    assert_eq!(cpu.memory[0x40], 0x99);

    text.push_str("alias res reset\n");
    let nl = Netlist::from_native(&text).unwrap();
    assert_eq!(nl.node("res"), nl.node("reset"));
}
