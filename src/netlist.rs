//! Netlist representation and loaders.
//!
//! A [`Netlist`] is a list of nodes (with pull-up information), a list of
//! transistors (gate / channel connections) and a table of node names. Two
//! on-disk formats are supported:
//!
//! * **Visual6502 JS** – a directory containing `segdefs.js`, `transdefs.js`
//!   and `nodenames.js` exactly as published by the visual6502.org project
//!   (and by derived projects such as Visual 2A03, the 6800 and Z80 data
//!   sets, ...). Load with [`Netlist::from_visual6502_dir`].
//! * **Compact native format** – a small line-oriented text file produced by
//!   [`Netlist::to_native`] (or `rustic6502 convert`). Load with
//!   [`Netlist::from_native`].
//!
//! Any chip whose netlist uses the standard 6502 pad names (`clk0`, `res`,
//! `rw`, `sync`, `ab0..15`, `db0..7`, ...) can be driven by
//! [`crate::cpu::Cpu6502`]. Variants with differing pad names can map them
//! with `alias` lines in the native format (see the format description in
//! `README.md`).

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Index of a node in the netlist.
pub type NodeId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transistor {
    pub name: String,
    pub gate: NodeId,
    pub c1: NodeId,
    pub c2: NodeId,
    /// "Weak" transistors (flagged in some data sets, e.g. the 6800) are
    /// usually ignored by the simulator.
    pub weak: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Netlist {
    /// Free-form chip name (e.g. "6502").
    pub chip: String,
    /// One past the highest node number in use.
    pub node_count: u32,
    /// `pullup[n]` is true when node `n` has a depletion-mode pull-up.
    pub pullup: Vec<bool>,
    pub transistors: Vec<Transistor>,
    /// Name -> node table. Several names may map to the same node.
    pub names: BTreeMap<String, NodeId>,
    pub vcc: NodeId,
    pub vss: NodeId,
    /// Toggles of the clock input pin per CPU half-cycle: 1 for a chip
    /// clocked directly (the 6502), 12 for the NTSC 2A03, whose on-chip
    /// divider makes the CPU clock from a ~21.48 MHz master clock.
    pub clock_ratio: u32,
}

#[derive(Debug)]
pub struct NetlistError(pub String);

impl std::fmt::Display for NetlistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for NetlistError {}

fn err<T>(msg: impl Into<String>) -> Result<T, NetlistError> {
    Err(NetlistError(msg.into()))
}

/// Options controlling conversion from Visual6502 data.
#[derive(Debug, Clone)]
pub struct Visual6502Options {
    pub chip: String,
    /// Name of the power node in `nodenames.js` (`vcc` for every known chip).
    pub vcc_name: String,
    /// Name of the ground node (`vss` for the 6502, `gnd` for the 6800).
    /// When `None`, `vss` and then `gnd` are tried.
    pub vss_name: Option<String>,
    /// Drop transistors flagged as weak (as the 6800 support code does).
    pub ignore_weak: bool,
    /// Recorded as [`Netlist::clock_ratio`].
    pub clock_ratio: u32,
}

impl Default for Visual6502Options {
    fn default() -> Self {
        Self {
            chip: "6502".into(),
            vcc_name: "vcc".into(),
            vss_name: None,
            ignore_weak: true,
            clock_ratio: 1,
        }
    }
}

impl Netlist {
    /// Load from a path: a directory is treated as Visual6502 JS data, a file
    /// as the native format.
    pub fn load(path: &str) -> Result<Self, NetlistError> {
        let p = Path::new(path);
        if p.is_dir() {
            let mut opts = Visual6502Options::default();
            if let Some(name) = p
                .canonicalize()
                .ok()
                .and_then(|c| c.file_name().map(|f| f.to_string_lossy().into_owned()))
            {
                opts.chip = name.strip_prefix("chip-").unwrap_or(&name).to_string();
            }
            Self::from_visual6502_dir(p, &opts)
        } else {
            let text =
                fs::read_to_string(p).map_err(|e| NetlistError(format!("{}: {e}", p.display())))?;
            Self::from_native(&text)
        }
    }

    pub fn node(&self, name: &str) -> Option<NodeId> {
        self.names.get(name).copied()
    }

    /// Every `(name, node)` pair whose name matches `pattern`, in name
    /// order. `*` matches any run of characters and `?` any single
    /// character; a pattern without wildcards matches only that exact name.
    pub fn match_names<'a>(&'a self, pattern: &'a str) -> impl Iterator<Item = (&'a str, NodeId)> {
        self.names
            .iter()
            .filter(move |(k, _)| glob_match(pattern, k))
            .map(|(k, &n)| (k.as_str(), n))
    }

    /// Named nodes, each with all of its names (in name order).
    pub fn names_by_node(&self) -> BTreeMap<NodeId, Vec<&str>> {
        let mut out: BTreeMap<NodeId, Vec<&str>> = BTreeMap::new();
        for (k, &n) in &self.names {
            out.entry(n).or_default().push(k);
        }
        out
    }

    /// Load a Visual6502-style data directory.
    pub fn from_visual6502_dir(dir: &Path, opts: &Visual6502Options) -> Result<Self, NetlistError> {
        let read = |f: &str| {
            fs::read_to_string(dir.join(f))
                .map_err(|e| NetlistError(format!("{}: {e}", dir.join(f).display())))
        };
        Self::from_visual6502(
            &read("segdefs.js")?,
            &read("transdefs.js")?,
            &read("nodenames.js")?,
            opts,
        )
    }

    /// Build a netlist from the contents of `segdefs.js`, `transdefs.js` and
    /// `nodenames.js`.
    pub fn from_visual6502(
        segdefs: &str,
        transdefs: &str,
        nodenames: &str,
        opts: &Visual6502Options,
    ) -> Result<Self, NetlistError> {
        let segs = js::parse_var(segdefs, "segdefs")?;
        let trans = js::parse_var(transdefs, "transdefs")?;
        let names = js::parse_var(nodenames, "nodenames")?;

        let mut name_map = BTreeMap::new();
        let js::Value::Object(entries) = names else {
            return err("nodenames is not an object");
        };
        for (k, v) in entries {
            if let Some(n) = v.as_num()
                && n >= 0.0
            {
                name_map.insert(k, n as NodeId);
            }
        }

        // Node pull-ups come from the first segment of each node, matching
        // wires.js:setupNodes().
        let mut pull: HashMap<NodeId, bool> = HashMap::new();
        let js::Value::Array(segs) = segs else {
            return err("segdefs is not an array");
        };
        for seg in segs {
            let js::Value::Array(f) = seg else {
                return err("segdefs entry is not an array");
            };
            let n = f
                .first()
                .and_then(|v| v.as_num())
                .ok_or(NetlistError("bad segdef".into()))?;
            let up = matches!(f.get(1), Some(js::Value::Str(s)) if s == "+");
            pull.entry(n as NodeId).or_insert(up);
        }

        let js::Value::Array(trans) = trans else {
            return err("transdefs is not an array");
        };
        let mut transistors = Vec::with_capacity(trans.len());
        for t in trans {
            let js::Value::Array(f) = t else {
                return err("transdefs entry is not an array");
            };
            let name = match f.first() {
                Some(js::Value::Str(s)) => s.clone(),
                _ => return err("transistor without name"),
            };
            let num = |i: usize| -> Result<NodeId, NetlistError> {
                f.get(i)
                    .and_then(|v| v.as_num())
                    .map(|n| n as NodeId)
                    .ok_or_else(|| NetlistError(format!("transistor {name}: bad field {i}")))
            };
            let weak = f.get(6).map(|v| v.truthy()).unwrap_or(false);
            if weak && opts.ignore_weak {
                continue;
            }
            transistors.push(Transistor {
                gate: num(1)?,
                c1: num(2)?,
                c2: num(3)?,
                weak,
                name,
            });
        }

        let vcc = *name_map
            .get(&opts.vcc_name)
            .ok_or_else(|| NetlistError(format!("no node named {}", opts.vcc_name)))?;
        let vss = match &opts.vss_name {
            Some(n) => name_map.get(n).copied(),
            None => name_map.get("vss").or(name_map.get("gnd")).copied(),
        }
        .ok_or(NetlistError("no ground node (vss/gnd)".into()))?;

        let mut max = vcc.max(vss);
        for &n in pull.keys() {
            max = max.max(n);
        }
        for t in &transistors {
            max = max.max(t.gate).max(t.c1).max(t.c2);
        }
        let node_count = max + 1;
        let mut pullup = vec![false; node_count as usize];
        for (n, up) in pull {
            pullup[n as usize] = up;
        }
        pullup[vcc as usize] = false;
        pullup[vss as usize] = false;

        let nl = Netlist {
            chip: opts.chip.clone(),
            node_count,
            pullup,
            transistors,
            names: name_map,
            vcc,
            vss,
            clock_ratio: opts.clock_ratio,
        };
        nl.validate()?;
        Ok(nl)
    }

    fn validate(&self) -> Result<(), NetlistError> {
        for t in &self.transistors {
            for n in [t.gate, t.c1, t.c2] {
                if n >= self.node_count {
                    return err(format!(
                        "transistor {} references node {n} out of range",
                        t.name
                    ));
                }
            }
        }
        for (k, &n) in &self.names {
            if n >= self.node_count {
                return err(format!("name {k} references node {n} out of range"));
            }
        }
        if self.vcc == self.vss {
            return err("vcc and vss are the same node");
        }
        if self.clock_ratio == 0 {
            return err("clock-ratio must be at least 1");
        }
        Ok(())
    }

    /// Serialise to the native text format.
    pub fn to_native(&self) -> String {
        let mut s = String::new();
        s.push_str("rustic6502-netlist 1\n");
        let _ = writeln!(s, "chip {}", self.chip);
        let _ = writeln!(s, "nodes {}", self.node_count);
        let _ = writeln!(s, "vcc {}", self.vcc);
        let _ = writeln!(s, "vss {}", self.vss);
        if self.clock_ratio != 1 {
            let _ = writeln!(s, "clock-ratio {}", self.clock_ratio);
        }
        let ups: Vec<String> = (0..self.node_count)
            .filter(|&n| self.pullup[n as usize])
            .map(|n| n.to_string())
            .collect();
        for chunk in ups.chunks(20) {
            let _ = writeln!(s, "pullup {}", chunk.join(" "));
        }
        for t in &self.transistors {
            let _ = writeln!(
                s,
                "t {} {} {} {}{}",
                t.name,
                t.gate,
                t.c1,
                t.c2,
                if t.weak { " weak" } else { "" }
            );
        }
        for (k, n) in &self.names {
            let _ = writeln!(s, "n {k} {n}");
        }
        s
    }

    /// Parse the native text format.
    ///
    /// ```text
    /// rustic6502-netlist 1
    /// chip <name>
    /// nodes <count>              (optional; inferred otherwise)
    /// vcc <node> | vcc <name>
    /// vss <node> | vss <name>
    /// clock-ratio <n>            (optional; default 1)
    /// pullup <node> <node> ...
    /// t <name> <gate> <c1> <c2> [weak]
    /// n <name> <node>
    /// alias <new-name> <existing-name>
    /// # comment (whole lines only)
    /// ```
    pub fn from_native(text: &str) -> Result<Self, NetlistError> {
        let mut lines = text.lines().enumerate();
        match lines.next() {
            Some((_, l)) if l.trim().starts_with("rustic6502-netlist") => {}
            _ => return err("missing 'rustic6502-netlist' header"),
        }
        let mut chip = String::from("unknown");
        let mut declared_nodes: Option<u32> = None;
        let mut clock_ratio = 1;
        let mut vcc_s = None;
        let mut vss_s = None;
        let mut ups = Vec::new();
        let mut transistors = Vec::new();
        let mut names = BTreeMap::new();
        let mut aliases = Vec::new();
        for (ln, line) in lines {
            // Only whole-line comments: node names such as `#(A+B)0` contain '#'.
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let ctx = |m: &str| NetlistError(format!("line {}: {m}", ln + 1));
            let mut it = line.split_whitespace();
            let kw = it.next().unwrap();
            let rest: Vec<&str> = it.collect();
            let num = |s: &str| {
                s.parse::<NodeId>()
                    .map_err(|_| ctx(&format!("bad node number '{s}'")))
            };
            match kw {
                "chip" => chip = rest.join(" "),
                "nodes" => {
                    declared_nodes = Some(num(rest.first().ok_or_else(|| ctx("missing count"))?)?)
                }
                "clock-ratio" => {
                    let r = rest.first().ok_or_else(|| ctx("missing ratio"))?;
                    clock_ratio = r
                        .parse::<u32>()
                        .ok()
                        .filter(|&r| r > 0)
                        .ok_or_else(|| ctx(&format!("bad clock ratio '{r}'")))?;
                }
                "vcc" => vcc_s = rest.first().map(|s| s.to_string()),
                "vss" => vss_s = rest.first().map(|s| s.to_string()),
                "pullup" => {
                    for r in rest {
                        ups.push(num(r)?);
                    }
                }
                "t" => {
                    if rest.len() < 4 {
                        return Err(ctx("transistor needs name gate c1 c2"));
                    }
                    transistors.push(Transistor {
                        name: rest[0].to_string(),
                        gate: num(rest[1])?,
                        c1: num(rest[2])?,
                        c2: num(rest[3])?,
                        weak: rest.get(4) == Some(&"weak"),
                    });
                }
                "n" => {
                    if rest.len() != 2 {
                        return Err(ctx("name line needs <name> <node>"));
                    }
                    names.insert(rest[0].to_string(), num(rest[1])?);
                }
                "alias" => {
                    if rest.len() != 2 {
                        return Err(ctx("alias needs <new> <existing>"));
                    }
                    aliases.push((rest[0].to_string(), rest[1].to_string(), ln + 1));
                }
                other => return Err(ctx(&format!("unknown keyword '{other}'"))),
            }
        }
        for (new, old, ln) in aliases {
            let n = *names
                .get(&old)
                .ok_or_else(|| NetlistError(format!("line {ln}: unknown name '{old}'")))?;
            names.insert(new, n);
        }
        let resolve = |s: Option<String>, what: &str| -> Result<NodeId, NetlistError> {
            let s = s.ok_or_else(|| NetlistError(format!("missing '{what}' line")))?;
            s.parse::<NodeId>()
                .ok()
                .or_else(|| names.get(&s).copied())
                .ok_or_else(|| NetlistError(format!("{what}: unknown node '{s}'")))
        };
        let vcc = resolve(vcc_s, "vcc")?;
        let vss = resolve(vss_s, "vss")?;
        let mut max = vcc.max(vss);
        for &u in &ups {
            max = max.max(u);
        }
        for t in &transistors {
            max = max.max(t.gate).max(t.c1).max(t.c2);
        }
        for &n in names.values() {
            max = max.max(n);
        }
        let node_count = declared_nodes.unwrap_or(0).max(max + 1);
        let mut pullup = vec![false; node_count as usize];
        for u in ups {
            pullup[u as usize] = true;
        }
        let nl = Netlist {
            chip,
            node_count,
            pullup,
            transistors,
            names,
            vcc,
            vss,
            clock_ratio,
        };
        nl.validate()?;
        Ok(nl)
    }
}

/// Shell-style wildcard match: `*` is any run of characters (including
/// none), `?` is exactly one character. Everything else is literal, so node
/// names such as `#(A+B)0` can be matched directly.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Position of the last `*` seen and the text position it was tried at.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || (p[pi] != '*' && p[pi] == t[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// A minimal parser for the subset of JavaScript used by the Visual6502 data
/// files: `var name = <literal>;` where literals are numbers, strings,
/// booleans, arrays and objects with identifier/string keys.
mod js {
    use super::NetlistError;

    #[derive(Debug, Clone)]
    pub enum Value {
        Num(f64),
        Str(String),
        Bool(bool),
        Null,
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    impl Value {
        pub fn as_num(&self) -> Option<f64> {
            match self {
                Value::Num(n) => Some(*n),
                _ => None,
            }
        }
        pub fn truthy(&self) -> bool {
            match self {
                Value::Num(n) => *n != 0.0,
                Value::Str(s) => !s.is_empty(),
                Value::Bool(b) => *b,
                Value::Null => false,
                _ => true,
            }
        }
    }

    /// Find `var <name> = <literal>` (or `<name> = <literal>`) and parse it.
    pub fn parse_var(src: &str, name: &str) -> Result<Value, NetlistError> {
        let src = strip_comments(src);
        let b = src.as_bytes();
        let mut search = 0;
        while let Some(off) = src[search..].find(name) {
            let start = search + off;
            let end = start + name.len();
            search = end;
            let before_ok = start == 0 || !is_ident(b[start - 1]);
            let after_ok = end >= b.len() || !is_ident(b[end]);
            if !(before_ok && after_ok) {
                continue;
            }
            let mut p = Parser { b, i: end };
            p.ws();
            if p.peek() != Some(b'=') || p.b.get(p.i + 1) == Some(&b'=') {
                continue;
            }
            p.i += 1;
            return p.value();
        }
        Err(NetlistError(format!("no assignment to '{name}' found")))
    }

    fn is_ident(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
    }

    fn strip_comments(src: &str) -> String {
        let b = src.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\'' | b'"' => {
                    let q = b[i];
                    out.push(b[i]);
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == b'\\' && i + 1 < b.len() {
                            out.push(b[i]);
                            i += 1;
                        }
                        out.push(b[i]);
                        i += 1;
                    }
                    if i < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                    out.push(b' ');
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8(out).unwrap_or_default()
    }

    struct Parser<'a> {
        b: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.b.get(self.i).copied()
        }
        fn ws(&mut self) {
            while matches!(self.peek(), Some(c) if c.is_ascii_whitespace()) {
                self.i += 1;
            }
        }
        fn fail<T>(&self, m: &str) -> Result<T, NetlistError> {
            Err(NetlistError(format!(
                "JS parse error at byte {}: {m}",
                self.i
            )))
        }
        fn value(&mut self) -> Result<Value, NetlistError> {
            self.ws();
            match self.peek() {
                Some(b'[') => {
                    self.i += 1;
                    let mut v = Vec::new();
                    loop {
                        self.ws();
                        match self.peek() {
                            Some(b']') => {
                                self.i += 1;
                                return Ok(Value::Array(v));
                            }
                            Some(b',') => {
                                self.i += 1;
                            }
                            None => return self.fail("unterminated array"),
                            _ => v.push(self.value()?),
                        }
                    }
                }
                Some(b'{') => {
                    self.i += 1;
                    let mut v = Vec::new();
                    loop {
                        self.ws();
                        match self.peek() {
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(Value::Object(v));
                            }
                            Some(b',') => {
                                self.i += 1;
                            }
                            None => return self.fail("unterminated object"),
                            _ => {
                                let key = match self.peek() {
                                    Some(b'\'' | b'"') => match self.value()? {
                                        Value::Str(s) => s,
                                        _ => unreachable!(),
                                    },
                                    _ => self.ident()?,
                                };
                                self.ws();
                                if self.peek() != Some(b':') {
                                    return self.fail("expected ':'");
                                }
                                self.i += 1;
                                let val = self.value()?;
                                v.push((key, val));
                            }
                        }
                    }
                }
                Some(q @ (b'\'' | b'"')) => {
                    self.i += 1;
                    let mut s = Vec::new();
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        if c == q {
                            return Ok(Value::Str(String::from_utf8_lossy(&s).into_owned()));
                        }
                        if c == b'\\' {
                            if let Some(n) = self.peek() {
                                s.push(n);
                                self.i += 1;
                            }
                        } else {
                            s.push(c);
                        }
                    }
                    self.fail("unterminated string")
                }
                Some(c) if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => {
                    let st = self.i;
                    self.i += 1;
                    while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'.') {
                        self.i += 1;
                    }
                    let t = std::str::from_utf8(&self.b[st..self.i]).unwrap();
                    let (neg, body) = match t.as_bytes()[0] {
                        b'-' => (true, &t[1..]),
                        b'+' => (false, &t[1..]),
                        _ => (false, t),
                    };
                    let n = if let Some(h) = body.strip_prefix("0x").or(body.strip_prefix("0X")) {
                        i64::from_str_radix(h, 16).map(|v| v as f64).ok()
                    } else {
                        body.parse::<f64>().ok()
                    };
                    match n {
                        Some(n) => Ok(Value::Num(if neg { -n } else { n })),
                        None => self.fail(&format!("bad number '{t}'")),
                    }
                }
                Some(_) => {
                    let id = self.ident()?;
                    match id.as_str() {
                        "true" => Ok(Value::Bool(true)),
                        "false" => Ok(Value::Bool(false)),
                        "null" | "undefined" => Ok(Value::Null),
                        _ => self.fail(&format!("unsupported identifier '{id}'")),
                    }
                }
                None => self.fail("unexpected end of input"),
            }
        }
        fn ident(&mut self) -> Result<String, NetlistError> {
            let st = self.i;
            while matches!(self.peek(), Some(c) if is_ident(c)) {
                self.i += 1;
            }
            if st == self.i {
                return self.fail("expected identifier");
            }
            Ok(String::from_utf8_lossy(&self.b[st..self.i]).into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_js_subset() {
        let seg = "/* c */ var segdefs = [\n[0,'+',1,2,3],\n[1,'-',1,2],\n[0,'-',1],\n];";
        let tr =
            "var transdefs = [\n['t0', 1, 0, 2, [1,2],[3]],\n['t1', 0, 1, 3, [1],[2], true],\n]";
        let nn = "var nodenames ={\nvcc: 2, // pwr\nvss: 3,\nfoo: 1,\nbad: -1,\n}";
        let nl = Netlist::from_visual6502(seg, tr, nn, &Visual6502Options::default()).unwrap();
        assert_eq!(nl.node_count, 4);
        assert!(nl.pullup[0]);
        assert!(!nl.pullup[1]);
        assert_eq!(nl.transistors.len(), 1, "weak transistor dropped");
        assert_eq!(nl.node("foo"), Some(1));
        assert_eq!(nl.node("bad"), None);
        let back = Netlist::from_native(&nl.to_native()).unwrap();
        assert_eq!(back, nl);
    }

    #[test]
    fn globs() {
        assert!(glob_match("ab*", "ab12"));
        assert!(glob_match("ab*", "ab"));
        assert!(!glob_match("ab*", "xab"));
        assert!(glob_match("a?", "a7"));
        assert!(!glob_match("a?", "a"));
        assert!(glob_match("*", ""));
        assert!(glob_match("#(A+B)0", "#(A+B)0"));
        assert!(glob_match("*c*k*", "clock"));
        assert!(!glob_match("*c*k", "clocks"));
    }

    #[test]
    fn native_aliases() {
        let t =
            "rustic6502-netlist 1\nchip x\nvcc 1\nvss 0\nt t0 2 3 0\nn reset 2\nalias res reset\n";
        let nl = Netlist::from_native(t).unwrap();
        assert_eq!(nl.node("res"), Some(2));
    }

    #[test]
    fn native_clock_ratio() {
        let base = "rustic6502-netlist 1\nvcc 1\nvss 0\nt t0 2 3 0\n";
        assert_eq!(Netlist::from_native(base).unwrap().clock_ratio, 1);
        let nl = Netlist::from_native(&format!("{base}clock-ratio 12\n")).unwrap();
        assert_eq!(nl.clock_ratio, 12);
        assert_eq!(Netlist::from_native(&nl.to_native()).unwrap(), nl);
        assert!(Netlist::from_native(&format!("{base}clock-ratio 0\n")).is_err());
        assert!(Netlist::from_native(&format!("{base}clock-ratio x\n")).is_err());
    }
}
