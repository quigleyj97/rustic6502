# Netlist data

`6502.netlist` is the NMOS 6502 (rev D) netlist from the
[Visual6502 project](https://github.com/trebonian/visual6502), converted from
`segdefs.js`, `transdefs.js` and `nodenames.js` with:

```sh
rustic6502 convert path/to/visual6502 -o netlists/6502.netlist
```

## The netlist format

`.netlist` files use a small, line-oriented text format. It holds only what
the switch-level simulator needs: nodes, pull-ups, transistors and node names.
It leaves out the segment geometry in the Visual6502 data, so files are much
smaller and load faster than the JavaScript sources. `Netlist::from_native`
in `src/netlist.rs` reads the format and `Netlist::to_native` writes it.

### Example

```text
rustic6502-netlist 1
chip 6502
nodes 1725
vcc 657
vss 558
pullup 0 3 4 5 6 8 10 11 14 16 17 19 20 21 22 23 25 26 27 29
pullup 31 33 34 35 36 38 39 46 53 58 60 61 62 63 65 67 70 71 72 75
t t0 357 558 217
t t1 1608 657 349
n #(A+B)0 143
n ab0 268
n clk0 1171
```

### General rules

- The **first line** must start with `rustic6502-netlist`, followed by a
  format version. The current version is `1`. The version number is not
  checked yet, but write `1`. Nothing can come before this line, not even a
  comment or a blank line.
- Every later line is either blank, a comment, or a **record**: a keyword
  followed by fields separated by whitespace (any number of spaces or tabs).
  Leading and trailing whitespace is ignored.
- **Comments** take up a whole line that starts with `#`. Trailing comments
  are not supported, because node names may contain `#` (for example
  `#(A+B)0`).
- **Node numbers** are non-negative decimal integers (`u32`). They index a
  flat node table and do not need to be contiguous.
- **Names** (of nodes and transistors) are any run of characters other than
  whitespace. They are case-sensitive.
- Records may come in any order. All names and aliases are resolved after
  the whole file has been read.
- An unknown keyword is an error. Errors report the 1-based line number.

### Records

| Keyword  | Fields                           | Required | Repeatable |
| -------- | -------------------------------- | -------- | ---------- |
| `chip`   | `<name...>`                      | no       | last wins  |
| `nodes`  | `<count>`                        | no       | last wins  |
| `vcc`    | `<node-or-name>`                 | yes      | last wins  |
| `vss`    | `<node-or-name>`                 | yes      | last wins  |
| `clock-ratio` | `<n>`                       | no       | last wins  |
| `pullup` | `<node> <node> ...`              | no       | yes        |
| `t`      | `<name> <gate> <c1> <c2> [weak]` | no       | yes        |
| `n`      | `<name> <node>`                  | no       | yes        |
| `alias`  | `<new-name> <existing-name>`     | no       | yes        |

#### `chip <name...>`

A free-form label for the chip, such as `6502`. All the remaining fields are
joined with single spaces, so `chip Ricoh 2A03` is allowed. It defaults to
`unknown`. The value is informational only; `rustic6502 info` prints it.

#### `nodes <count>`

The size of the node table: one more than the highest node number. This
record is optional. The loader works out the size from the highest node
number used anywhere in the file, and uses whichever of the two values is
larger. A larger declared count gives you unused nodes at the end.

#### `vcc <node-or-name>` / `vss <node-or-name>`

The power and ground rails. Both are required and must be different nodes.
The field can be a node number or a node name (including an alias) defined
elsewhere in the file. If the field parses as a number, it is used as a node
number, even when a node has that name.

The simulator holds these nodes at fixed levels. Transistors connecting a
node to a rail act as pull-ups or pull-downs.

#### `clock-ratio <n>`

How many times the simulator toggles the clock input pin per CPU
half-cycle. A positive integer that defaults to `1`, which suits the 6502,
whose clock pad is the CPU clock. Chips with an on-chip clock divider need a
larger value. The NTSC 2A03 divides its ~21.48 MHz master clock by 12, so
one CPU half-cycle is 6 master cycles (12 toggles) and its netlist uses
`clock-ratio 12`. `to_native` writes this record only when the value is not
1.

#### `pullup <node> <node> ...`

Lists nodes that have a depletion-mode pull-up (a resistor to VCC). A node
with a pull-up reads high unless something drives it low. You can spread the
list over any number of `pullup` lines. `to_native` writes 20 per line.
Listing a node twice has no extra effect. Nodes not listed have no pull-up.

#### `t <name> <gate> <c1> <c2> [weak]`

One NMOS enhancement transistor:

- `name` is an identifier for the transistor, such as `t1234`. It only
  appears in error messages and need not be unique.
- `gate` is the node that controls the transistor. The channel conducts when
  this node is high.
- `c1` and `c2` are the two ends of the channel. Their order does not matter.

If the fifth field is exactly `weak`, the transistor is marked weak. Any
other fields after the fourth are ignored. Some Visual6502-derived data sets,
such as the 6800, flag weak transistors. By default `rustic6502 convert`
drops them and never writes them out (pass `--keep-weak` to keep them). The
simulator does not handle weak transistors differently yet, so a `weak`
transistor in a `.netlist` file is simulated like any other.

#### `n <name> <node>`

Gives a node a name. A node can have any number of names. If the same name
appears more than once, the last `n` line wins. Node names are how the
simulator finds the chip's pins and internal registers (see
[Names used by the simulator](#names-used-by-the-simulator)).

#### `alias <new-name> <existing-name>`

Gives `new-name` the same node as `existing-name`. Aliases are applied after
all `n` lines, in the order they appear, so one alias can refer to an earlier
alias. An alias to an unknown name is an error.

Aliases let you use a netlist whose pad names differ from what the simulator
expects, without renumbering anything. For example:

```text
alias rdy RDY
alias ab0 A0
```

### Validation

After parsing, the loader checks that:

- every node used by a transistor or a name is less than the node count, and
- `vcc` and `vss` are different nodes.

For a `.netlist` file the range check always passes, because the node count
is widened to cover every node the file uses.

### Names used by the simulator

`Cpu6502` (in `src/cpu.rs`) looks up these node names and fails to start if
any of the required ones are missing:

| Signal      | Name(s)                                    | Required |
| ----------- | ------------------------------------------ | -------- |
| CPU clock   | first of `clk0`, `phi0`, `clk`             | yes      |
| Clock input | `clk_in`; if missing, the CPU clock node   | no       |
| Reset       | first of `res`, `reset`                    | yes      |
| Pins        | `rw`, `sync`, `rdy`, `irq`, `nmi`, `so`    | yes      |
| Address bus | `ab0` … `ab15`                             | yes      |
| Data bus    | `db0` … `db7`                              | yes      |
| Registers   | `a0`–`a7`, `x0`–`x7`, `y0`–`y7`, `s0`–`s7` | no       |
|             | `pcl0`–`pcl7`, `pch0`–`pch7`, `ir0`–`ir7`  | no       |
| Flags       | `p0` … `p7` (each individually)            | no       |

The simulator toggles the clock input `clock-ratio` times per half-step.
After that it services the bus according to the CPU clock node: when that
node is low it drives the data bus for a read, and when it is high it
commits a pending write. On the 6502 both are the `clk0` pad. On the 2A03
`clk_in` is the external clock pin and `clk0` is the on-chip divider's
output.

The register names are used only for tracing. If any of `a`, `x`, `y`, `s`,
`pcl`, `pch` or `ir` is incomplete, register tracing is turned off; the
simulation still runs. A missing `pN` flag is shown as unknown.

### Converting from Visual6502 data

`rustic6502 convert <dir>` reads `segdefs.js`, `transdefs.js` and
`nodenames.js` from `<dir>` and writes a `.netlist` file:

- **Pull-ups** come from the `'+'` / `'-'` field of each node's *first*
  segment in `segdefs.js`, as in Visual6502's `wires.js`. The pull-up flag is
  always cleared on the rails.
- **Transistors** come from `transdefs.js` fields 0–3 (name, gate, c1, c2)
  and field 6 (weak). The coordinate fields are dropped.
- **Names** come from `nodenames.js`. Entries with a negative node number are
  skipped.
- **Rails**: VCC is the node named by `--vcc` (default `vcc`). Ground is the node named by
  `--vss`, or else `vss`, or else `gnd`.
- `--chip` sets the `chip` record (default `6502`), `--keep-weak` keeps
  weak transistors, and `--clock-ratio` sets the `clock-ratio` record.

The `--netlist` option of `run` and `info` also accepts a Visual6502 data
directory directly, in which case it is converted in memory at load time.
Such a directory has no `clock-ratio` record, so pass `run --clock-ratio`
(which overrides the netlist's value) for chips that need one.
