# rustic6502

[![CI](https://github.com/quigleyj97/rustic6502/actions/workflows/ci.yml/badge.svg)](https://github.com/quigleyj97/rustic6502/actions/workflows/ci.yml)
[![Docs](https://github.com/quigleyj97/rustic6502/actions/workflows/docs.yml/badge.svg)](https://quigleyj97.github.io/rustic6502/)

A transistor-level 6502 (and family) netlist simulator written in Rust. The
netlists are converted from the netlist files Chipsim uses in
[Visual6502](https://github.com/trebonian/visual6502), and are simulated
similarly.

Docs: [quigleyj97.github.io/rustic6502](https://quigleyj97.github.io/rustic6502/rustic6502/index.html)

## Building

To build this simulator, run `cargo build` for a debug build or
`cargo build --release` for a performance-optimized version. You can run the
simulator with a simple test program like this:

```sh
cargo build --release
# LDA #$42; LDX #$05; STX $11; ADC #$10; STA $10; JMP *
./target/release/rustic6502 run --poke 0000:a942a2058e1100691085104c0b00 \
    --stop-on-trap --trace-instructions --dump 0010:2
```

The full address bus, from $0000-$FFFF, is covered by a virtual RAM block.
Exortibantly luxurious by the standards of the time, but these days you can
run the simulator on consumer devices with 750 thousand times more memory :)

### Tracing nodes

You can record any of the netlist's nodes every half-cycle, for example to
check an HDL implementation of the chip against this simulation, node by
node. To list the node names (`name node` per line; `--by-node` groups the
aliases of each node on one line; patterns may use `*` and `?`):

```sh
./target/release/rustic6502 nodes            # every named node
./target/release/rustic6502 nodes 'ab*' 'db?' rw
./target/release/rustic6502 nodes --by-node
```

Then pass `--trace-node` to `run`. It takes names, wildcard patterns (`*`
alone is every named node) or raw node numbers (traced as `n<number>`). You
can repeat it or separate entries with commas:

```sh
./target/release/rustic6502 run --poke 0000:a942a2058e1100691085104c0b00 \
    --stop-on-trap --trace-node 'clk0,rw,sync,ab*,db*' --trace-out run.vcd
```

`--trace-format vcd` (the default) writes a Value Change Dump that GTKWave
and most HDL simulators can read. One time unit is one half-cycle, all
signals are in a module named after the chip, and names that aren't plain
Verilog identifiers (like `#(A+B)0`) are written as escaped identifiers
(`\#(A+B)0`). `--trace-format table` writes a `# half_cycle name...`
header, then one line per half-cycle with the half-cycle number and a `0`
or `1` for each node, which is easy to `$fscanf` from a testbench. The first
sample (half-cycle 0) is the state right after reset. Each sample is taken
after the clock edge and the bus have settled.

From Rust, use `Probe::resolve` and `NodeTracer` (in `src/trace.rs`), and
call `NodeTracer::sample` after each `Cpu6502::half_step`.

### Tests

This includes a few unit tests, which can be run with:

```sh
cargo test
```

Klaus Dormann's functional tests are included as a git submodule, and can be run
with:

```sh
git submodule update --init
cargo test --release --test klaus -- --ignored
```

### Undocumented opcodes

Most undocumented opcodes for the NMOS 6502 are implemented and working, in a
way that matches the original silicon. In fact, rustic6502 can handle opcodes
that visual6502 and (ironically) perfect6502 cannot:

- `0x0B - ANC`
- `0x2B - ANC`
- `0x4B - ALR`
- `0x6B - ARR`
- `0xBB - LAS`

This is because those simulators do not correctly model the NMOS behavior when
multiple transistors are driving the same silicon. rustic6502 does when set to
NMOS simulation mode, and thus handles these opcodes correctly.

2 opcodes do not correctly simulate, however:

- `0x8B - XAA` (aka `ANE`)
- `0xAB - LXA`

These rely on open bus behavior that can vary by manufacturer, stepping, device,
or even device configuration (for instance, NES carts can induce different open
bus behavior than external debuggers).

They remain unsupported- `rustic6502` currently passes ~45% of `XAA` tests and
`~18%` of `LXA` tests from Tom Harte's SingleStepTests repo (specifically, the
NMOS 6502 with BCD). It's unclear exactly which chip was used for these tests,
or the test setup, so I can't reliably reference them here. I should do real
hardware tests myself on this, but that's a problem for another day.

### Benchmarking

`examples/bench.rs` mirrors perfect6502's `make benchmark`: it boots
Commodore 64 BASIC V2 and stops the clock at the first `READY.` prompt. You'll
need the CBM BASIC binary, easily obtainable from the perfect6502 repo, to run
it.

On an M4 Pro, rustic6502 runs just over twice as fast (~0.21s) compared to
perfect6502 (~0.51s), from my own testing.

```sh
cargo run --release --example bench -- [path/to/cbmbasic.bin]
```

### Adding new netlists

A conversion utility is included in the built binary to permit creating and
loading new netlists. Run it with:

```sh
./targets/release/rustic6502 convert dir/to/local/checkout -o output.netlist
```

You can then load and consume it with:

```sh
./targets/release/rustic6502 run --netlist output.netlist
```

## License

I genuinely am not sure which license applies to the netlist data. In the
visual6502 project, most files have the MIT license as their header but the
segment definitions claim a CC-BY-NC-SA-3.0 license. I'm guessing that applies
to the transistor shapes, since that's a visual asset and not simply code, and
issue #37 in visual6502 claims MIT licensing (with the exception of GPL-licensed
vendored code), so that's what I've gone with.
