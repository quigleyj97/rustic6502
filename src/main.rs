use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use rustic6502::Cpu6502;
use rustic6502::netlist::{Netlist, Visual6502Options};

#[derive(Parser)]
#[command(
    version,
    about = "Transistor-level 6502 simulator using the Visual6502 netlist"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a program on the simulated chip.
    Run(RunArgs),
    /// Convert Visual6502 JS data (segdefs.js, transdefs.js, nodenames.js)
    /// into the native netlist format.
    Convert {
        /// Directory containing the three .js files.
        dir: PathBuf,
        /// Output file (stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Chip name recorded in the output.
        #[arg(long, default_value = "6502")]
        chip: String,
        /// Name of the ground node (default: vss, then gnd).
        #[arg(long)]
        vss: Option<String>,
        /// Keep transistors flagged as weak.
        #[arg(long)]
        keep_weak: bool,
    },
    /// Print statistics about a netlist.
    Info {
        #[arg(long)]
        netlist: Option<String>,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Netlist to simulate: a native .netlist file or a Visual6502 data
    /// directory. Defaults to the netlist built into the binary.
    #[arg(long)]
    netlist: Option<String>,
    /// Load a binary file into memory: ADDR:FILE (ADDR in hex).
    #[arg(long, value_name = "ADDR:FILE")]
    load: Vec<String>,
    /// Write bytes into memory: ADDR:HEXBYTES, e.g. 0000:a9ff8d0010.
    #[arg(long, value_name = "ADDR:BYTES")]
    poke: Vec<String>,
    /// Set the reset vector ($FFFC) to this address (hex).
    #[arg(long, value_parser = parse_hex16)]
    reset_vector: Option<u16>,
    /// Maximum number of clock cycles to run.
    #[arg(long, default_value_t = 1_000_000)]
    cycles: u64,
    /// Print the chip status every half-cycle.
    #[arg(long)]
    trace: bool,
    /// Print the chip status at every opcode fetch.
    #[arg(long)]
    trace_instructions: bool,
    /// Stop when an opcode is fetched from this address (hex).
    #[arg(long, value_parser = parse_hex16)]
    stop_pc: Vec<u16>,
    /// Stop when the program jumps or branches to itself.
    #[arg(long)]
    stop_on_trap: bool,
    /// Bytes written to this address (hex) are printed to stdout.
    #[arg(long, value_parser = parse_hex16)]
    putc: Option<u16>,
    /// Hex-dump memory at exit: ADDR:LEN (both hex).
    #[arg(long, value_name = "ADDR:LEN")]
    dump: Vec<String>,
    /// Print a status line to stderr every N cycles.
    #[arg(long, value_name = "N")]
    progress: Option<u64>,
    /// Exit with status 1 unless the run stopped at this PC (hex).
    #[arg(long, value_parser = parse_hex16)]
    expect_pc: Option<u16>,
}

fn parse_hex16(s: &str) -> Result<u16, String> {
    let s = s.trim_start_matches("0x").trim_start_matches('$');
    u16::from_str_radix(s, 16).map_err(|e| format!("bad hex address '{s}': {e}"))
}

fn split_addr(s: &str) -> Result<(u16, &str), String> {
    let (a, rest) = s
        .split_once(':')
        .ok_or_else(|| format!("expected ADDR:..., got '{s}'"))?;
    Ok((parse_hex16(a)?, rest))
}

fn load_netlist(path: Option<&str>) -> Result<Netlist, String> {
    match path {
        None => Ok(rustic6502::builtin_netlist()),
        Some(p) => Netlist::load(p).map_err(|e| e.to_string()),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Run(args) => run(args),
        Cmd::Convert {
            dir,
            output,
            chip,
            vss,
            keep_weak,
        } => (|| {
            let opts = Visual6502Options {
                chip,
                vss_name: vss,
                ignore_weak: !keep_weak,
                ..Default::default()
            };
            let nl = Netlist::from_visual6502_dir(&dir, &opts).map_err(|e| e.to_string())?;
            let text = nl.to_native();
            match output {
                Some(o) => std::fs::write(&o, text).map_err(|e| format!("{}: {e}", o.display()))?,
                None => print!("{text}"),
            }
            eprintln!(
                "{} nodes, {} transistors, {} names",
                nl.node_count,
                nl.transistors.len(),
                nl.names.len()
            );
            Ok(true)
        })(),
        Cmd::Info { netlist } => load_netlist(netlist.as_deref()).map(|nl| {
            println!("chip:        {}", nl.chip);
            println!("nodes:       {}", nl.node_count);
            println!("transistors: {}", nl.transistors.len());
            println!("pull-ups:    {}", nl.pullup.iter().filter(|&&p| p).count());
            println!("names:       {}", nl.names.len());
            println!("vcc/vss:     {}/{}", nl.vcc, nl.vss);
            match rustic6502::cpu::Pins::from_netlist(&nl) {
                Ok(p) => println!(
                    "6502 pads:   ok (registers {})",
                    if p.regs.is_some() { "found" } else { "missing" }
                ),
                Err(e) => println!("6502 pads:   {e}"),
            }
            true
        }),
    };
    match res {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(args: RunArgs) -> Result<bool, String> {
    let nl = load_netlist(args.netlist.as_deref())?;
    let mut cpu = Cpu6502::new(&nl)?;
    for l in &args.load {
        let (addr, file) = split_addr(l)?;
        let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        cpu.load(addr, &data);
    }
    for p in &args.poke {
        let (addr, hex) = split_addr(p)?;
        let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        if !hex.len().is_multiple_of(2) {
            return Err(format!("odd number of hex digits in '{p}'"));
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("'{p}': {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        cpu.load(addr, &bytes);
    }
    if let Some(v) = args.reset_vector {
        cpu.load(0xfffc, &v.to_le_bytes());
    }
    if let Some(port) = args.putc {
        cpu.on_write = Some(Box::new(move |a, d| {
            if a == port {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&[d]);
                let _ = out.flush();
            }
        }));
    }

    cpu.reset();
    let start = Instant::now();
    let mut last_fetch: Option<(u16, u64)> = None;
    let mut stopped_at = None;
    let max_half = args.cycles.saturating_mul(2);
    while cpu.half_cycles < max_half {
        cpu.half_step();
        if let Some(every) = args.progress
            && every > 0
            && cpu.half_cycles % (every * 2) == 0
        {
            let secs = start.elapsed().as_secs_f64();
            eprintln!(
                "[{:>8.1}s] {} cycles, {:.1} kHz, PC ${:04X}",
                secs,
                cpu.half_cycles / 2,
                cpu.half_cycles as f64 / 2.0 / secs / 1000.0,
                cpu.pc()
            );
        }
        if args.trace {
            println!("{}", cpu.status());
        }
        // An opcode fetch: SYNC high while the address is valid in phi2.
        if cpu.clk0() && cpu.sync() {
            let pc = cpu.address_bus();
            if args.trace_instructions && !args.trace {
                println!("{}", cpu.status());
            }
            if args.stop_pc.contains(&pc) {
                stopped_at = Some(pc);
                break;
            }
            if args.stop_on_trap {
                // A JMP-to-self or branch-to-self fetches the same opcode
                // again within a few cycles with nothing else happening.
                if let Some((prev, at)) = last_fetch
                    && prev == pc
                    && cpu.half_cycles - at <= 8
                {
                    stopped_at = Some(pc);
                    break;
                }
            }
            last_fetch = Some((pc, cpu.half_cycles));
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cycles = cpu.half_cycles / 2;
    eprintln!();
    match stopped_at {
        Some(pc) => eprintln!("stopped at PC ${pc:04X} after {cycles} cycles"),
        None => eprintln!("cycle limit reached after {cycles} cycles"),
    }
    eprintln!("{}", cpu.status());
    eprintln!(
        "{:.2}s, {:.1} kHz{}",
        elapsed,
        cycles as f64 / elapsed.max(1e-9) / 1000.0,
        if cpu.sim.unstable_events > 0 {
            format!(", {} unstable recalcs", cpu.sim.unstable_events)
        } else {
            String::new()
        }
    );
    for d in &args.dump {
        let (addr, len) = split_addr(d)?;
        let len = usize::from_str_radix(len, 16).map_err(|e| format!("'{d}': {e}"))?;
        for row in (0..len).step_by(16) {
            let a = addr as usize + row;
            let bytes: Vec<String> = (a..(a + 16).min(addr as usize + len))
                .map(|i| format!("{:02x}", cpu.memory[i & 0xffff]))
                .collect();
            println!("{:04x}: {}", a & 0xffff, bytes.join(" "));
        }
    }
    Ok(match args.expect_pc {
        Some(want) => stopped_at == Some(want),
        None => true,
    })
}
