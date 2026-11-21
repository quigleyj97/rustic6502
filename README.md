# rustic6502

A transistor-level 6502 (and family) netlist simulator written in Rust. The
netlists are converted from the netlist files Chipsim uses in
[Visual6502](https://github.com/trebonian/visual6502), and are simulated
similarly.

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
