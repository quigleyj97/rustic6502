//! Record the values of chosen netlist nodes over time.
//!
//! This is for comparing another implementation of the chip (an HDL model,
//! say) against the transistor-level simulation, node by node. Pick the
//! nodes with [`Probe::resolve`], then call [`NodeTracer::sample`] after
//! every half-cycle. Two output formats are supported:
//!
//! * [`TraceFormat::Vcd`]: a standard Value Change Dump, readable by
//!   GTKWave, Verilator/Icarus testbenches and most waveform tools. One time
//!   unit is one half-cycle. Names that aren't plain Verilog identifiers
//!   (such as `#(A+B)0`) are written as escaped identifiers (`\#(A+B)0`).
//! * [`TraceFormat::Table`]: one line per sample: the half-cycle number and
//!   then a `0`/`1` column per probe, after a `#` header line naming the
//!   columns. Easy to read from a testbench or diff line by line.

use std::io::{self, Write};

use crate::netlist::{Netlist, NodeId};
use crate::sim::Sim;

/// One traced signal: a name and the node it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub name: String,
    pub node: NodeId,
}

impl Probe {
    /// Resolve probe specifications into probes, in the order given.
    ///
    /// Each spec is one of:
    /// * a node name, such as `ab0` (aliases work too);
    /// * a wildcard pattern (`*`, `?`), such as `ab*`, matching every name
    ///   it fits, in name order. `*` alone traces every named node;
    /// * a node number, such as `1171`, traced under the name `n1171`.
    ///
    /// Specs can also be lists separated by commas: `ab*,db*,rw`. A name
    /// picked twice is traced once. A spec that matches nothing is an error.
    pub fn resolve<S: AsRef<str>>(nl: &Netlist, specs: &[S]) -> Result<Vec<Probe>, String> {
        let mut out: Vec<Probe> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut push = |name: &str, node: NodeId, out: &mut Vec<Probe>| {
            if seen.insert(name.to_string()) {
                out.push(Probe {
                    name: name.to_string(),
                    node,
                });
            }
        };
        for spec in specs.iter().flat_map(|s| s.as_ref().split(',')) {
            let spec = spec.trim();
            if spec.is_empty() {
                continue;
            }
            if let Ok(n) = spec.parse::<NodeId>() {
                if n >= nl.node_count {
                    return Err(format!(
                        "node {n} out of range (netlist has {} nodes)",
                        nl.node_count
                    ));
                }
                push(&format!("n{n}"), n, &mut out);
                continue;
            }
            let mut any = false;
            for (name, node) in nl.match_names(spec) {
                push(name, node, &mut out);
                any = true;
            }
            if !any {
                return Err(format!("no node name matches '{spec}'"));
            }
        }
        Ok(out)
    }
}

/// Output format for [`NodeTracer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TraceFormat {
    /// Value Change Dump.
    #[default]
    Vcd,
    /// Whitespace-separated columns, one line per sample.
    Table,
}

impl std::str::FromStr for TraceFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "vcd" => Ok(Self::Vcd),
            "table" | "txt" | "text" => Ok(Self::Table),
            _ => Err(format!(
                "unknown trace format '{s}' (expected vcd or table)"
            )),
        }
    }
}

/// Writes the values of a set of [`Probe`]s each time [`sample`] is called.
///
/// [`sample`]: NodeTracer::sample
pub struct NodeTracer<W: Write> {
    out: W,
    format: TraceFormat,
    probes: Vec<Probe>,
    /// VCD identifier code for each probe. Probes on the same node share a
    /// code, so aliases always show identical waveforms.
    codes: Vec<String>,
    /// Whether each probe is the first one using its code; only those are
    /// written in the VCD value section.
    primary: Vec<bool>,
    /// Value at the last sample, per probe (`None` before the first).
    last: Option<Vec<bool>>,
    now: Vec<bool>,
}

impl<W: Write> NodeTracer<W> {
    /// Start a trace. For VCD this writes the header straight away; `scope`
    /// names the module the signals are placed in (the chip name is a good
    /// choice).
    pub fn new(
        mut out: W,
        format: TraceFormat,
        probes: Vec<Probe>,
        scope: &str,
    ) -> io::Result<Self> {
        let mut codes = Vec::with_capacity(probes.len());
        let mut primary = Vec::with_capacity(probes.len());
        let mut by_node = std::collections::HashMap::new();
        for p in &probes {
            let next = by_node.len();
            primary.push(!by_node.contains_key(&p.node));
            let code = by_node.entry(p.node).or_insert_with(|| vcd_code(next));
            codes.push(code.clone());
        }
        match format {
            TraceFormat::Vcd => {
                writeln!(
                    out,
                    "$version rustic6502 {} $end",
                    env!("CARGO_PKG_VERSION")
                )?;
                writeln!(out, "$comment one time unit = one half-cycle of clk0 $end")?;
                writeln!(out, "$timescale 1ns $end")?;
                writeln!(out, "$scope module {} $end", vcd_ident(scope))?;
                for (p, code) in probes.iter().zip(&codes) {
                    writeln!(out, "$var wire 1 {code} {} $end", vcd_ident(&p.name))?;
                }
                writeln!(out, "$upscope $end")?;
                writeln!(out, "$enddefinitions $end")?;
            }
            TraceFormat::Table => {
                write!(out, "# half_cycle")?;
                for p in &probes {
                    write!(out, " {}", p.name)?;
                }
                writeln!(out)?;
            }
        }
        Ok(Self {
            out,
            format,
            probes,
            codes,
            primary,
            last: None,
            now: Vec::new(),
        })
    }

    pub fn probes(&self) -> &[Probe] {
        &self.probes
    }

    /// Record the current value of every probe at time `half_cycle`. VCD
    /// output only lists the signals that changed since the last sample (all
    /// of them the first time); the table always gets a full line.
    pub fn sample(&mut self, sim: &Sim, half_cycle: u64) -> io::Result<()> {
        let mut now = std::mem::take(&mut self.now);
        now.clear();
        now.extend(self.probes.iter().map(|p| sim.is_high(p.node)));
        match self.format {
            TraceFormat::Vcd => {
                let first = self.last.is_none();
                let mut stamped = false;
                for (i, &v) in now.iter().enumerate() {
                    let changed = self.last.as_ref().is_none_or(|l| l[i] != v);
                    if !changed || !self.primary[i] {
                        continue;
                    }
                    if !stamped {
                        writeln!(self.out, "#{half_cycle}")?;
                        if first {
                            writeln!(self.out, "$dumpvars")?;
                        }
                        stamped = true;
                    }
                    writeln!(self.out, "{}{}", v as u8, self.codes[i])?;
                }
                if first && stamped {
                    writeln!(self.out, "$end")?;
                }
            }
            TraceFormat::Table => {
                write!(self.out, "{half_cycle}")?;
                for &v in &now {
                    write!(self.out, " {}", v as u8)?;
                }
                writeln!(self.out)?;
            }
        }
        self.now = self.last.replace(now).unwrap_or_default();
        Ok(())
    }

    /// Flush and return the writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

/// The `n`th VCD identifier code: base-94 over the printable ASCII range.
fn vcd_code(mut n: usize) -> String {
    let mut s = String::new();
    loop {
        s.push((b'!' + (n % 94) as u8) as char);
        n /= 94;
        if n == 0 {
            return s;
        }
        n -= 1;
    }
}

/// `name` as a VCD reference: unchanged if it is a plain identifier,
/// otherwise a Verilog escaped identifier (`\` prefix).
fn vcd_ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        name.to_string()
    } else {
        format!("\\{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inverter() -> Netlist {
        Netlist::from_native(
            "rustic6502-netlist 1\nvcc 1\nvss 0\npullup 3\nt t0 2 3 0\n\
             n in 2\nn out 3\nalias nout out\nn #weird 3\n",
        )
        .unwrap()
    }

    #[test]
    fn resolves_specs() {
        let nl = inverter();
        let p = Probe::resolve(&nl, &["in,out", "o*", "3"]).unwrap();
        let names: Vec<_> = p.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["in", "out", "n3"]);
        assert!(Probe::resolve(&nl, &["nope"]).is_err());
        assert!(Probe::resolve(&nl, &["99"]).is_err());
    }

    #[test]
    fn codes_are_unique() {
        let codes: std::collections::HashSet<_> = (0..20000).map(vcd_code).collect();
        assert_eq!(codes.len(), 20000);
        assert_eq!(vcd_code(0), "!");
        assert_eq!(vcd_code(93), "~");
        assert_eq!(vcd_code(94), "!!");
    }

    #[test]
    fn vcd_and_table_output() {
        let nl = inverter();
        let probes = Probe::resolve(&nl, &["in", "out", "nout", "#weird"]).unwrap();
        let mut sim = Sim::new(&nl);
        sim.set_low(2);
        sim.recalc_all();

        let mut vcd = NodeTracer::new(Vec::new(), TraceFormat::Vcd, probes.clone(), "x").unwrap();
        let mut tab = NodeTracer::new(Vec::new(), TraceFormat::Table, probes, "x").unwrap();
        for t in 0..3 {
            vcd.sample(&sim, t).unwrap();
            tab.sample(&sim, t).unwrap();
            if t == 0 {
                sim.set_high(2);
            }
        }
        let vcd = String::from_utf8(vcd.finish().unwrap()).unwrap();
        let tab = String::from_utf8(tab.finish().unwrap()).unwrap();

        assert!(vcd.contains("$var wire 1 ! in $end"));
        assert!(vcd.contains("$var wire 1 \" out $end"));
        assert!(vcd.contains("$var wire 1 \" nout $end"));
        assert!(vcd.contains("$var wire 1 \" \\#weird $end"));
        let body = vcd.split("$enddefinitions $end\n").nth(1).unwrap();
        // Aliases share a code, so each node's change is written once, and
        // the unchanged sample at t=2 writes nothing.
        assert_eq!(body, "#0\n$dumpvars\n0!\n1\"\n$end\n#1\n1!\n0\"\n");

        assert_eq!(
            tab,
            "# half_cycle in out nout #weird\n0 0 1 1 1\n1 1 0 0 0\n2 1 0 0 0\n"
        );
    }
}
