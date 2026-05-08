//! Pull `///` doc comments out of Move source files and index them by item.
//!
//! Not a full parser. Walks each `.move` file line-by-line, accumulates `///`
//! blocks, and pairs them with the declaration that follows. Tracks just
//! enough nesting (struct + enum + named-variant bodies) to reach fields and
//! variants. Function bodies are skipped wholesale via brace counting.
//!
//! Good enough for fixture-style packages. If a real package trips this up
//! we'd swap in `move-compiler`'s parser, which already attaches `DocComment`
//! to AST nodes.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::Result;

#[derive(Debug, Default)]
pub struct DocMap {
    /// Module-level: `module → doc`.
    modules: BTreeMap<String, String>,
    /// Items (structs, enums, fns, consts): `(module, name) → doc`.
    items: BTreeMap<(String, String), String>,
    /// Struct fields and enum variants: `(module, parent, member) → doc`.
    members: BTreeMap<(String, String, String), String>,
    /// Variant named fields: `(module, type, variant, field) → doc`.
    variant_fields: BTreeMap<(String, String, String, String), String>,
}

impl DocMap {
    pub fn module(&self, m: &str) -> Option<&str> {
        self.modules.get(m).map(String::as_str)
    }

    pub fn item(&self, m: &str, name: &str) -> Option<&str> {
        self.items
            .get(&(m.to_string(), name.to_string()))
            .map(String::as_str)
    }

    pub fn member(&self, m: &str, parent: &str, member: &str) -> Option<&str> {
        self.members
            .get(&(m.to_string(), parent.to_string(), member.to_string()))
            .map(String::as_str)
    }

    pub fn variant_field(&self, m: &str, ty: &str, variant: &str, field: &str) -> Option<&str> {
        self.variant_fields
            .get(&(
                m.to_string(),
                ty.to_string(),
                variant.to_string(),
                field.to_string(),
            ))
            .map(String::as_str)
    }
}

/// Read every `.move` file under `sources_dir` and build a [`DocMap`].
/// Missing or non-directory paths produce an empty map (codegen still works,
/// just without docs).
pub fn collect(sources_dir: &Path) -> Result<DocMap> {
    let mut map = DocMap::default();
    if !sources_dir.is_dir() {
        return Ok(map);
    }
    let mut paths: Vec<_> = fs::read_dir(sources_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("move"))
        .collect();
    paths.sort();
    for p in paths {
        let src = fs::read_to_string(&p)?;
        Scanner::new(&src, &mut map).run();
    }
    Ok(map)
}

#[derive(Debug, PartialEq, Eq)]
enum VariantShape {
    Unit,
    Positional,
    Named,
}

struct Scanner<'a> {
    lines: Vec<&'a str>,
    idx: usize,
    map: &'a mut DocMap,
    module: Option<String>,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str, map: &'a mut DocMap) -> Self {
        Self {
            lines: src.lines().collect(),
            idx: 0,
            map,
            module: None,
        }
    }

    fn run(&mut self) {
        self.parse_top_level();
    }

    fn eof(&self) -> bool {
        self.idx >= self.lines.len()
    }

    fn peek(&self) -> &'a str {
        self.lines.get(self.idx).copied().unwrap_or("")
    }

    /// Consume blank lines, `///` doc lines, plain `//` comments, and
    /// attribute lines (`#[...]`). Doc lines accumulate; blank or plain
    /// comments clear the buffer; attribute lines pass through transparently.
    /// Returns the joined doc string (or `None` if empty) and leaves the
    /// scanner pointed at the first non-trivia line.
    fn collect_lead(&mut self) -> Option<String> {
        let mut docs: Vec<String> = Vec::new();
        while !self.eof() {
            let trimmed = self.peek().trim_start();
            if trimmed.is_empty() {
                self.idx += 1;
                docs.clear();
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("///") {
                if rest.starts_with('/') {
                    self.idx += 1;
                    docs.clear();
                    continue;
                }
                // Keep the leading space the user typed (if any) so the
                // rendered `#[doc = " Foo"]` round-trips to `/// Foo` and
                // not `///Foo`.
                docs.push(rest.trim_end().to_string());
                self.idx += 1;
                continue;
            }
            if trimmed.starts_with("//") {
                self.idx += 1;
                docs.clear();
                continue;
            }
            if trimmed.starts_with("#[") {
                self.idx += 1;
                continue;
            }
            break;
        }
        if docs.is_empty() {
            None
        } else {
            Some(docs.join("\n"))
        }
    }

    fn parse_top_level(&mut self) {
        while !self.eof() {
            let docs = self.collect_lead();
            if self.eof() {
                return;
            }
            let line = self.peek();
            let trimmed = line.trim_start();

            if let Some(name) = parse_module_decl(trimmed) {
                self.module = Some(name.clone());
                if let Some(d) = docs {
                    self.map.modules.insert(name, d);
                }
                self.idx += 1;
                continue;
            }

            if let Some(name) = parse_fun_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs {
                        self.map.items.insert((m.clone(), name), d);
                    }
                }
                // Don't advance past the decl line first — its `{` is the
                // brace that opens the body, so `skip_balanced_body` needs
                // to count it.
                self.skip_balanced_body();
                continue;
            }

            if let Some(name) = parse_const_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs {
                        self.map.items.insert((m.clone(), name), d);
                    }
                }
                self.idx += 1;
                continue;
            }

            if let Some(name) = parse_struct_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs.clone() {
                        self.map.items.insert((m.clone(), name.clone()), d);
                    }
                }
                let opens = brace_delta(line) > 0;
                self.idx += 1;
                if opens {
                    self.parse_struct_body(&name);
                }
                continue;
            }

            if let Some(name) = parse_enum_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs.clone() {
                        self.map.items.insert((m.clone(), name.clone()), d);
                    }
                }
                let opens = brace_delta(line) > 0;
                self.idx += 1;
                if opens {
                    self.parse_enum_body(&name);
                }
                continue;
            }

            // Unknown line; advance.
            self.idx += 1;
        }
    }

    fn parse_struct_body(&mut self, struct_name: &str) {
        let mut depth: i32 = 1;
        while !self.eof() && depth > 0 {
            let docs = self.collect_lead();
            if self.eof() {
                return;
            }
            let line = self.peek();
            let trimmed = line.trim_start();
            if trimmed.starts_with('}') {
                depth += brace_delta(line);
                self.idx += 1;
                continue;
            }
            if let Some(field) = parse_field_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs {
                        self.map
                            .members
                            .insert((m.clone(), struct_name.to_string(), field), d);
                    }
                }
            }
            depth += brace_delta(line);
            self.idx += 1;
        }
    }

    fn parse_enum_body(&mut self, enum_name: &str) {
        let mut depth: i32 = 1;
        while !self.eof() && depth > 0 {
            let docs = self.collect_lead();
            if self.eof() {
                return;
            }
            let line = self.peek();
            let trimmed = line.trim_start();
            if trimmed.starts_with('}') && depth == 1 {
                depth += brace_delta(line);
                self.idx += 1;
                continue;
            }
            if let Some((variant, shape)) = parse_variant_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs.clone() {
                        self.map
                            .members
                            .insert((m.clone(), enum_name.to_string(), variant.clone()), d);
                    }
                }
                if shape == VariantShape::Named {
                    depth += brace_delta(line);
                    self.idx += 1;
                    self.parse_variant_named(enum_name, &variant, &mut depth);
                    continue;
                }
            }
            depth += brace_delta(line);
            self.idx += 1;
        }
    }

    fn parse_variant_named(&mut self, enum_name: &str, variant: &str, depth: &mut i32) {
        let target = *depth - 1;
        while !self.eof() && *depth > target {
            let docs = self.collect_lead();
            if self.eof() {
                return;
            }
            let line = self.peek();
            let trimmed = line.trim_start();
            if trimmed.starts_with('}') {
                *depth += brace_delta(line);
                self.idx += 1;
                continue;
            }
            if let Some(field) = parse_field_decl(trimmed) {
                if let Some(m) = &self.module {
                    if let Some(d) = docs {
                        self.map.variant_fields.insert(
                            (m.clone(), enum_name.to_string(), variant.to_string(), field),
                            d,
                        );
                    }
                }
            }
            *depth += brace_delta(line);
            self.idx += 1;
        }
    }

    /// Skip lines until brace count balances. Used after a function-decl
    /// line to consume the body.
    fn skip_balanced_body(&mut self) {
        let mut depth: i32 = 0;
        let mut started = false;
        while !self.eof() {
            let line = self.lines[self.idx];
            self.idx += 1;
            let delta = brace_delta(line);
            if delta > 0 {
                started = true;
            }
            depth += delta;
            if started && depth <= 0 {
                return;
            }
        }
    }
}

/// `{` minus `}` count on a line. Naive — doesn't strip strings or comments,
/// but Move source rarely has braces inside string literals.
fn brace_delta(line: &str) -> i32 {
    let opens = line.bytes().filter(|b| *b == b'{').count() as i32;
    let closes = line.bytes().filter(|b| *b == b'}').count() as i32;
    opens - closes
}

fn parse_module_decl(s: &str) -> Option<String> {
    let s = s.strip_prefix("module ")?.trim_start();
    let (_addr, rest) = split_ident(s)?;
    let rest = rest.strip_prefix("::")?;
    let (name, _) = split_ident(rest)?;
    Some(name.to_string())
}

fn parse_struct_decl(s: &str) -> Option<String> {
    let mut s = s;
    if let Some(r) = s.strip_prefix("public ") {
        s = r.trim_start();
    }
    let s = s.strip_prefix("struct ")?.trim_start();
    let (name, _) = split_ident(s)?;
    Some(name.to_string())
}

fn parse_enum_decl(s: &str) -> Option<String> {
    let mut s = s;
    if let Some(r) = s.strip_prefix("public ") {
        s = r.trim_start();
    }
    let s = s.strip_prefix("enum ")?.trim_start();
    let (name, _) = split_ident(s)?;
    Some(name.to_string())
}

fn parse_fun_decl(s: &str) -> Option<String> {
    let mut s = s;
    loop {
        if let Some(r) = s.strip_prefix("public(package) ") {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = s.strip_prefix("public(friend) ") {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = s.strip_prefix("public ") {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = s.strip_prefix("entry ") {
            s = r.trim_start();
            continue;
        }
        break;
    }
    let s = s.strip_prefix("fun ")?.trim_start();
    let (name, _) = split_ident(s)?;
    Some(name.to_string())
}

fn parse_const_decl(s: &str) -> Option<String> {
    let s = s.strip_prefix("const ")?.trim_start();
    let (name, rest) = split_ident(s)?;
    if rest.trim_start().starts_with(':') {
        Some(name.to_string())
    } else {
        None
    }
}

fn parse_field_decl(s: &str) -> Option<String> {
    let (name, rest) = split_ident(s)?;
    if rest.trim_start().starts_with(':') {
        Some(name.to_string())
    } else {
        None
    }
}

fn parse_variant_decl(s: &str) -> Option<(String, VariantShape)> {
    let (name, rest) = split_ident(s)?;
    let rest = rest.trim_start();
    let shape = if rest.starts_with('(') {
        VariantShape::Positional
    } else if rest.starts_with('{') {
        VariantShape::Named
    } else if rest.is_empty() || rest.starts_with(',') {
        VariantShape::Unit
    } else {
        return None;
    };
    Some((name.to_string(), shape))
}

fn split_ident(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| !is_ident(c)).unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some((&s[..end], &s[end..]))
    }
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
