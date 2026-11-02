//! Lean module headers (`module`, `prelude`, imports), parsed exactly as Lean's own
//! fast header parser does (`Lean.parseImports'`, `src/Lean/Elab/ParseImportsFast.lean`
//! in Lean v4.35.0), including its quirks: keywords match as prefixes, a file without
//! `prelude` imports `Init` twice (once as a meta import), and in a module an import is
//! private unless marked `public`. `worker/` can check this parser against Lean's.

/// One import of a module header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The imported module's name components.
    pub module: Vec<String>,
    /// `import all`
    pub all: bool,
    /// Whether the import is re-exported (`public`, or any import outside the module system).
    pub exported: bool,
    /// `meta import`
    pub meta: bool,
}

/// A parsed module header.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Header {
    /// Whether the file starts with `module` (participates in the module system).
    pub is_module: bool,
    /// The imports, in order, including the implicit `Init` imports of a non-`prelude` file.
    pub imports: Vec<Import>,
}

/// A parse error: byte offset and message (Lean's wording).
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}

struct P<'a> {
    s: &'a str,
    pos: usize,
    imports: Vec<Import>,
    is_module: bool,
    meta: bool,
    exported: bool,
    all: bool,
    bad_modifier: bool,
    error: Option<String>,
}

fn is_letter_like(c: char) -> bool {
    let v = c as u32;
    (0x3b1..=0x3c9).contains(&v) && v != 0x3bb
        || (0x391..=0x3a9).contains(&v) && v != 0x3a0 && v != 0x3a3
        || (0x3ca..=0x3fb).contains(&v)
        || (0x1f00..=0x1ffe).contains(&v)
        || (0x2100..=0x214f).contains(&v)
        || (0x1d49c..=0x1d59f).contains(&v)
        || (0x00c0..=0x00ff).contains(&v) && v != 0x00d7 && v != 0x00f7
        || (0x0100..=0x017f).contains(&v)
}

fn is_sub_script_alnum(c: char) -> bool {
    let v = c as u32;
    (0x2080..=0x2089).contains(&v) || (0x2090..=0x209c).contains(&v) || (0x1d62..=0x1d6a).contains(&v) || v == 0x2c7c
}

fn is_id_first(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || is_letter_like(c)
}

fn is_id_rest_fast(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || (c != '.' && c != '\n' && c != ' '
            && (c == '_' || c == '\'' || c == '!' || c == '?' || is_letter_like(c) || is_sub_script_alnum(c)))
}

impl<'a> P<'a> {
    fn peek_at(&self, pos: usize) -> Option<char> {
        self.s[pos..].chars().next()
    }
    fn peek(&self) -> Option<char> {
        self.peek_at(self.pos)
    }
    fn next_pos(&self, pos: usize) -> usize {
        pos + self.peek_at(pos).map_or(1, |c| c.len_utf8())
    }
    fn err(&mut self, msg: &str) {
        self.error = Some(msg.to_string());
    }

    fn whitespace(&mut self) {
        loop {
            if self.error.is_some() {
                return;
            }
            let Some(c) = self.peek() else { return };
            if c == '\t' {
                self.err("tabs are not allowed; please configure your editor to expand them");
                return;
            } else if c == ' ' || c == '\r' || c == '\n' {
                self.pos += 1;
            } else if c == '-' {
                let i = self.pos + 1;
                if self.peek_at(i) == Some('-') {
                    // line comment: up to (not including) the newline
                    let rest = &self.s[i + 1..];
                    self.pos = i + 1 + rest.find('\n').unwrap_or(rest.len());
                } else {
                    return;
                }
            } else if c == '/' {
                let i = self.pos + 1;
                if self.peek_at(i) == Some('-') {
                    let j = i + 1;
                    match self.peek_at(j) {
                        Some('-') | Some('!') => return, // doc comments are tokens
                        // Lean also skips the character after `/-` before scanning
                        // for the end (so `/-/- -/` is a single, unnested comment).
                        Some(_) => {
                            self.pos = self.next_pos(j);
                            self.finish_comment_block(1);
                        }
                        None => {
                            self.pos = j;
                            self.finish_comment_block(1);
                        }
                    }
                } else {
                    return;
                }
            } else {
                return;
            }
        }
    }

    fn finish_comment_block(&mut self, mut nesting: usize) {
        loop {
            let Some(c) = self.peek() else {
                self.err("unterminated comment");
                return;
            };
            let i = self.next_pos(self.pos);
            if c == '-' {
                match self.peek_at(i) {
                    None => {
                        self.err("unterminated comment");
                        return;
                    }
                    Some('/') => {
                        self.pos = i + 1;
                        if nesting == 1 {
                            return;
                        }
                        nesting -= 1;
                    }
                    Some(_) => self.pos = i,
                }
            } else if c == '/' {
                match self.peek_at(i) {
                    None => {
                        self.err("unterminated comment");
                        return;
                    }
                    Some('-') => {
                        self.pos = i + 1;
                        nesting += 1;
                    }
                    Some(_) => self.pos = i,
                }
            } else {
                self.pos = i;
            }
        }
    }

    /// Lean's `keywordCore`: a prefix match (no word boundary), then whitespace.
    fn keyword(&mut self, k: &str) -> bool {
        if self.s[self.pos..].starts_with(k) {
            self.pos += k.len();
            self.whitespace();
            true
        } else {
            false
        }
    }

    fn is_id_cont(&self) -> bool {
        if self.peek() != Some('.') {
            return false;
        }
        match self.peek_at(self.pos + 1) {
            Some(c) => is_id_first(c) || c == '«',
            None => false,
        }
    }

    fn module_ident(&mut self) {
        let mut comps: Vec<String> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                self.err("unexpected end of input");
                return;
            };
            if c == '«' {
                let start = self.pos + c.len_utf8();
                match self.s[start..].find('»') {
                    None => {
                        self.pos = self.s.len();
                        self.err("unterminated identifier escape");
                        return;
                    }
                    Some(off) => {
                        comps.push(self.s[start..start + off].to_string());
                        self.pos = start + off + '»'.len_utf8();
                    }
                }
            } else if is_id_first(c) {
                let start = self.pos;
                let mut i = self.pos + c.len_utf8();
                while let Some(d) = self.peek_at(i) {
                    if !is_id_rest_fast(d) {
                        break;
                    }
                    i += d.len_utf8();
                }
                comps.push(self.s[start..i].to_string());
                self.pos = i;
            } else {
                self.err("expected identifier");
                return;
            }
            if self.is_id_cont() {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.imports.push(Import { module: comps, all: self.all, exported: self.exported, meta: self.meta });
        self.whitespace();
        self.meta = false;
        self.all = false;
        self.exported = !self.is_module;
    }

    fn many_imports(&mut self) {
        loop {
            let start = self.pos;
            // atomic(`public`? `meta`? `import`)
            if self.keyword("public") {
                if self.is_module { self.exported = true } else { self.bad_modifier = true }
            }
            if self.keyword("meta") {
                if self.is_module { self.meta = true } else { self.bad_modifier = true }
            }
            if self.error.is_none() && !self.keyword("import") {
                self.err("`import` expected");
            }
            if self.error.is_some() {
                self.pos = start;
            } else {
                if self.keyword("all") {
                    if self.is_module { self.all = true } else { self.bad_modifier = true }
                }
                if self.error.is_none() {
                    self.module_ident();
                }
            }
            if self.error.is_some() {
                if self.pos == start {
                    self.error = None;
                    self.bad_modifier = false;
                }
                return;
            } else if self.bad_modifier {
                self.pos = start;
                self.bad_modifier = false;
                self.err("cannot use 'public', 'meta', or 'all' without 'module'");
                return;
            }
        }
    }
}

fn init_import(meta: bool) -> Import {
    Import { module: vec!["Init".to_string()], all: false, exported: true, meta }
}

/// Parse the header of a Lean source file.
pub fn parse(input: &str) -> Result<Header, ParseError> {
    let mut p = P {
        s: input,
        pos: 0,
        imports: Vec::new(),
        is_module: false,
        meta: false,
        exported: false,
        all: false,
        bad_modifier: false,
        error: None,
    };
    p.whitespace();
    if p.error.is_none() {
        p.is_module = p.keyword("module");
        p.exported = !p.is_module;
    }
    if p.error.is_none() && !p.keyword("prelude") {
        p.imports.push(init_import(false));
        p.imports.push(init_import(true));
    }
    if p.error.is_none() {
        p.many_imports();
    }
    match p.error {
        Some(msg) => Err(ParseError { pos: p.pos, msg }),
        None => Ok(Header { is_module: p.is_module, imports: p.imports }),
    }
}

/// Lean's rendering of a name: components joined by `.`, escaped with `«»` when needed.
pub fn name_to_string(comps: &[String]) -> String {
    comps
        .iter()
        .map(|c| {
            let mut chars = c.chars();
            let ok = match chars.next() {
                Some(f) => is_id_first(f) && chars.all(is_id_rest_fast),
                None => false,
            };
            if ok { c.clone() } else { format!("«{c}»") }
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(h: &Header) -> Vec<(String, bool, bool, bool)> {
        h.imports.iter().map(|i| (name_to_string(&i.module), i.exported, i.meta, i.all)).collect()
    }

    #[test]
    fn plain_file() {
        let h = parse("import Foo.Bar\nimport Baz\n\ndef x := 1\n").unwrap();
        assert!(!h.is_module);
        assert_eq!(
            names(&h),
            vec![
                ("Init".into(), true, false, false),
                ("Init".into(), true, true, false),
                ("Foo.Bar".into(), true, false, false),
                ("Baz".into(), true, false, false)
            ]
        );
    }

    #[test]
    fn module_file() {
        let src = "module\n\npublic import A.B\nimport C\nmeta import D\npublic meta import all E\n\npublic section\n";
        let h = parse(src).unwrap();
        assert!(h.is_module);
        assert_eq!(
            names(&h)[2..],
            [
                ("A.B".into(), true, false, false),
                ("C".into(), false, false, false),
                ("D".into(), false, true, false),
                ("E".into(), true, true, true)
            ]
        );
    }

    #[test]
    fn prelude_and_comments() {
        let src = "/- block /- nested -/ -/\n-- line\nmodule\nprelude\npublic import Init.Core -- trailing\n/-! doc -/\n";
        let h = parse(src).unwrap();
        assert!(h.is_module);
        assert_eq!(names(&h), vec![("Init.Core".into(), true, false, false)]);
    }

    #[test]
    fn escapes_and_modifiers_without_module() {
        let h = parse("import «weird name».X\n").unwrap();
        assert_eq!(h.imports[2].module, vec!["weird name".to_string(), "X".to_string()]);
        let e = parse("public import X\n").unwrap_err();
        assert_eq!(e.msg, "cannot use 'public', 'meta', or 'all' without 'module'");
        assert!(parse("import\n").is_err());
        assert!(parse("\timport X").is_err());
    }
}
