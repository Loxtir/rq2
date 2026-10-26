//! The query language: a GraphQL-shaped selection set.
//!
//! ```text
//! selection := '{' field* '}'
//! field     := '!' name '...'? | alias? name suffix* selection?
//! alias     := IDENT ':'
//! name      := IDENT | '*'
//! suffix    := '...'                    walk this same key transitively
//!            | '**'                     project a repeated key, keeping its tree
//!            | '[' INT? ':' INT? ']'     slice an array
//!            | '[' collect-body ']'      one value per element, flattened
//!            | '!'
//! collect-body := selection | field+
//! ```
//!
//! Commas are optional separators, `#` starts a comment.

use crate::diag::{Diag, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub alias: Option<String>,
    /// `children...` — apply this same key transitively, producing one flat
    /// list of every node reachable through it.
    pub recursive: bool,
    /// `children** { id }` — repeat the selection while retaining nested edges.
    pub tree_recursive: bool,
    /// `!name` omits this key; `!name...` also omits it throughout this subtree.
    pub excluded: bool,
    /// Brackets in source order. At most one slice and one collect.
    pub steps: Vec<Step>,
    /// `field!` — fail the projection if the response omits it.
    pub required: bool,
    pub selection: Option<Selection>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slice {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Slice(Slice),
    /// `[id]` or `[{ id name }]` — project each element and flatten the
    /// results into a single array.
    Collect(Selection),
}

impl Selection {
    /// Explicit field names win over `*`, which acts as a fallback.
    pub fn find(&self, key: &str) -> Option<usize> {
        let mut wildcard = None;
        for (i, f) in self.fields.iter().enumerate() {
            if f.excluded {
                continue;
            }
            if f.is_wildcard() {
                if wildcard.is_none() {
                    wildcard = Some(i);
                }
            } else if f.name == key {
                return Some(i);
            }
        }
        wildcard
    }

    /// The name of the single plain field, when this selection is something
    /// like `[id]`. Collecting one scalar yields bare values instead of
    /// one-key objects.
    pub fn single_leaf(&self) -> Option<&str> {
        let [field] = self.fields.as_slice() else {
            return None;
        };
        if field.is_wildcard()
            || field.alias.is_some()
            || field.recursive
            || field.tree_recursive
            || field.excluded
            || field.required
            || !field.steps.is_empty()
            || field.selection.is_some()
        {
            return None;
        }
        Some(&field.name)
    }
}

impl Field {
    pub fn is_wildcard(&self) -> bool {
        self.name == "*"
    }

    pub fn collect(&self) -> Option<&Selection> {
        self.steps.iter().find_map(|step| match step {
            Step::Collect(sel) => Some(sel),
            Step::Slice(_) => None,
        })
    }

    pub fn slice(&self) -> Option<Slice> {
        self.steps.iter().find_map(|step| match step {
            Step::Slice(slice) => Some(*slice),
            Step::Collect(_) => None,
        })
    }
}

pub fn parse(src: &str) -> Result<Selection, Diag> {
    let mut p = Parser::new(src);
    p.skip_trivia();
    if p.peek() != Some(b'{') {
        return Err(Diag::new("a query must start with `{`", Span::point(p.pos)));
    }
    let sel = p.selection()?;
    p.skip_trivia();
    if p.pos < p.b.len() {
        return Err(Diag::new(
            "unexpected input after the selection set",
            Span::new(p.pos, p.b.len()),
        ));
    }
    Ok(sel)
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

fn is_trivia(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b',')
}

fn is_delimiter(b: u8) -> bool {
    is_trivia(b) || matches!(b, b'{' | b'}' | b':' | b'[' | b']' | b'!' | b'*' | b'#')
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src,
            b: src.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn span_from(&self, start: usize) -> Span {
        if self.pos > start {
            Span::new(start, self.pos)
        } else {
            Span::point(start)
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b) if is_trivia(b) => self.pos += 1,
                Some(b'#') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), Diag> {
        self.skip_trivia();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Diag::new(
                format!("expected `{}`", c as char),
                Span::point(self.pos),
            ))
        }
    }

    /// Names may contain anything except structural delimiters, which means
    /// `content-type`, `@id`, `x-amz-date` and unicode keys all work. A `...`
    /// run terminates the name so it can be read as the recursive suffix.
    fn ident(&mut self) -> Result<(String, usize), Diag> {
        self.skip_trivia();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b < 0x80 && is_delimiter(b) {
                break;
            }
            if b == b'.' && self.b[self.pos..].starts_with(b"...") {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Diag::new("expected a field name", Span::point(start)));
        }
        Ok((self.src[start..self.pos].to_string(), start))
    }

    fn name(&mut self) -> Result<String, Diag> {
        self.skip_trivia();
        if self.peek() == Some(b'*') {
            self.pos += 1;
            return Ok("*".to_string());
        }
        Ok(self.ident()?.0)
    }

    fn selection(&mut self) -> Result<Selection, Diag> {
        self.skip_trivia();
        let open = self.pos;
        self.expect(b'{')?;
        let mut fields = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    return Err(Diag::new("unclosed `{`", Span::new(open, self.pos)));
                }
                _ => {}
            }
            fields.push(self.field()?);
        }
        Ok(Selection { fields })
    }

    fn field(&mut self) -> Result<Field, Diag> {
        self.skip_trivia();
        let start = self.pos;

        let excluded = self.peek() == Some(b'!');
        if excluded {
            self.pos += 1;
        }

        let mut alias = None;
        let name;

        if self.peek().is_some() && self.peek() != Some(b'*') {
            let save = self.pos;
            let word = self.ident()?.0;
            self.skip_trivia();
            if self.peek() == Some(b':') {
                self.pos += 1;
                alias = Some(word);
                name = self.name()?;
            } else {
                self.pos = save;
                name = self.name()?;
            }
        } else {
            name = self.name()?;
        }

        let mut recursive = false;
        let mut tree_recursive = false;
        let mut required = false;
        let mut steps: Vec<Step> = Vec::new();

        loop {
            self.skip_trivia();
            let flat = self.b[self.pos..].starts_with(b"...");
            let tree = self.b[self.pos..].starts_with(b"**");
            if flat || tree {
                if recursive || tree_recursive {
                    return Err(Diag::new(
                        "choose one recursion suffix per field: `...` or `**`",
                        Span::point(self.pos),
                    ));
                }
                if name == "*" {
                    return Err(Diag::new(
                        "recursion needs a field name to walk, not `*`",
                        Span::point(self.pos),
                    ));
                }
                recursive = flat;
                tree_recursive = tree;
                self.pos += if flat { 3 } else { 2 };
                continue;
            }
            match self.peek() {
                Some(b'[') => {
                    steps.push(self.bracket()?);
                    continue;
                }
                Some(b'!') => {
                    // `id!` is required; `id !secret` starts another field.
                    if self.b.get(self.pos + 1).is_some_and(|b| !is_delimiter(*b)) {
                        break;
                    }
                    self.pos += 1;
                    required = true;
                    continue;
                }
                _ => break,
            }
        }

        let mut seen_collect = false;
        for step in &steps {
            match step {
                Step::Collect(_) => seen_collect = true,
                Step::Slice(_) if seen_collect => {
                    return Err(Diag::new(
                        "a slice must come before a collect: `items[0:5][id]`",
                        self.span_from(start),
                    ));
                }
                Step::Slice(_) => {}
            }
        }
        if steps
            .iter()
            .filter(|s| matches!(s, Step::Collect(_)))
            .count()
            > 1
        {
            return Err(Diag::new(
                "only one collect `[...]` is allowed per field",
                self.span_from(start),
            ));
        }

        self.skip_trivia();
        let selection = if self.peek() == Some(b'{') {
            Some(self.selection()?)
        } else {
            None
        };

        if excluded
            && (name == "*"
                || alias.is_some()
                || required
                || tree_recursive
                || !steps.is_empty()
                || selection.is_some())
        {
            return Err(Diag::new(
                "an exclusion must be `!field` or `!field...`, without an alias, selection, or other suffixes",
                self.span_from(start),
            ));
        }
        if tree_recursive && (selection.is_none() || !steps.is_empty()) {
            return Err(Diag::new(
                format!("`{name}**` needs a selection set, without slices or collects: `{name}** {{ field }}`"),
                self.span_from(start),
            ));
        }
        if recursive && !excluded && selection.is_none() && !seen_collect {
            return Err(Diag::new(
                format!(
                    "`{name}...` needs to say what to keep: `{name}...{{ field }}` returns projected nodes, \
                     `{name}...[field]` returns collected values"
                ),
                self.span_from(start),
            ));
        }
        if selection.is_some() && seen_collect {
            return Err(Diag::new(
                "a collect `[...]` already selects the fields, so a `{...}` here is ambiguous",
                self.span_from(start),
            ));
        }

        Ok(Field {
            name,
            alias,
            recursive,
            tree_recursive,
            excluded,
            steps,
            required,
            selection,
            span: self.span_from(start),
        })
    }

    fn bracket(&mut self) -> Result<Step, Diag> {
        let open = self.pos;
        if self.slice_ahead() {
            self.expect(b'[')?;
            let start = self.opt_int()?;
            self.expect(b':')?;
            let end = self.opt_int()?;
            self.expect(b']')?;
            return Ok(Step::Slice(Slice { start, end }));
        }

        self.expect(b'[')?;
        self.skip_trivia();
        let sel = if self.peek() == Some(b'{') {
            self.selection()?
        } else {
            let mut fields = Vec::new();
            loop {
                self.skip_trivia();
                match self.peek() {
                    Some(b']') => break,
                    None => return Err(Diag::new("unclosed `[`", Span::new(open, self.pos))),
                    _ => fields.push(self.field()?),
                }
            }
            if fields.is_empty() {
                return Err(Diag::new(
                    "`[]` needs at least one field to collect",
                    Span::new(open, self.pos),
                ));
            }
            Selection { fields }
        };
        self.expect(b']')?;
        Ok(Step::Collect(sel))
    }

    /// Distinguishes `[a:b]` from `[id]` without backtracking: a colon at
    /// bracket depth zero means a slice.
    fn slice_ahead(&self) -> bool {
        let mut depth = 0i32;
        let mut i = self.pos + 1;
        while i < self.b.len() {
            match self.b[i] {
                b'{' | b'[' => depth += 1,
                b']' if depth == 0 => return false,
                b'}' | b']' => depth -= 1,
                b':' if depth == 0 => return true,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn opt_int(&mut self) -> Result<Option<i64>, Diag> {
        self.skip_trivia();
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == digits {
            self.pos = start;
            return Ok(None);
        }
        let text = &self.src[start..self.pos];
        match text.parse::<i64>() {
            Ok(v) => Ok(Some(v)),
            Err(_) => Err(Diag::new("invalid slice index", Span::new(start, self.pos))),
        }
    }
}
