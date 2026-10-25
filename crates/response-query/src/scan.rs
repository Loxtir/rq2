//! Byte-level JSON scanning.
//!
//! Nothing here builds a DOM. Everything returns byte offsets into the original
//! buffer so callers can slice, copy, or skip without allocating.

use crate::diag::{Diag, Span};

pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Copy)]
pub struct Scanner<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Scanner { buf, pos: 0 }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Scanner { buf, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn src(&self) -> &'a [u8] {
        self.buf
    }

    pub fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub fn span_here(&self) -> Span {
        Span::point(self.pos)
    }

    pub fn slice(&self, start: usize, end: usize) -> &'a [u8] {
        let start = start.min(self.buf.len());
        let end = end.min(self.buf.len()).max(start);
        &self.buf[start..end]
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    pub fn expect(&mut self, c: u8) -> Result<(), Diag> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(Diag::new(
                format!("expected `{}`", c as char),
                self.span_here(),
            ))
        }
    }

    /// Scans a string literal starting at the current position.
    /// Returns the raw bytes *including* the surrounding quotes.
    pub fn scan_string_raw(&mut self) -> Result<&'a [u8], Diag> {
        let start = self.pos;
        if self.peek() != Some(b'"') {
            return Err(Diag::new("expected a string", self.span_here()));
        }
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err(Diag::new("unterminated string", Span::new(start, self.pos))),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(&self.buf[start..self.pos]);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    if self.peek().is_none() {
                        return Err(Diag::new(
                            "unterminated escape sequence",
                            Span::new(start, self.pos),
                        ));
                    }
                    self.pos += 1;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Scans one value of any type and returns the offset just past it.
    /// The value is not materialized.
    pub fn scan_value_end(&mut self) -> Result<usize, Diag> {
        self.skip_ws();
        let start = self.pos;
        match self.peek() {
            None => Err(Diag::new("unexpected end of input", self.span_here())),
            Some(b'"') => {
                self.scan_string_raw()?;
                Ok(self.pos)
            }
            Some(b'{') | Some(b'[') => {
                let mut depth = 0i32;
                loop {
                    match self.peek() {
                        None => {
                            return Err(Diag::new(
                                "unterminated object or array",
                                Span::new(start, self.pos),
                            ))
                        }
                        Some(b'"') => {
                            self.scan_string_raw()?;
                        }
                        Some(b'{') | Some(b'[') => {
                            depth += 1;
                            self.pos += 1;
                        }
                        Some(b'}') | Some(b']') => {
                            depth -= 1;
                            self.pos += 1;
                            if depth == 0 {
                                return Ok(self.pos);
                            }
                        }
                        Some(_) => self.pos += 1,
                    }
                }
            }
            Some(_) => {
                while let Some(b) = self.peek() {
                    match b {
                        b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r' => break,
                        _ => self.pos += 1,
                    }
                }
                if self.pos == start {
                    return Err(Diag::new("expected a value", self.span_here()));
                }
                Ok(self.pos)
            }
        }
    }

    /// Splits an array into element byte ranges. Leaves the cursor after `]`.
    pub fn split_array_elements(&mut self) -> Result<Vec<(usize, usize)>, Diag> {
        self.expect(b'[')?;
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.expect(b']')?;
                break;
            }
            if self.peek().is_none() {
                return Err(Diag::new("unterminated array", self.span_here()));
            }
            let start = self.pos;
            let end = self.scan_value_end()?;
            out.push((start, end));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.expect(b',')?,
                Some(b']') => {
                    self.expect(b']')?;
                    break;
                }
                _ => return Err(Diag::new("expected `,` or `]`", self.span_here())),
            }
        }
        Ok(out)
    }
}

/// Strips a leading UTF-8 BOM. Windows tools write these constantly.
pub fn strip_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}

/// Decodes a raw JSON string literal (quotes included) into a `String`.
pub fn decode_string(raw: &[u8]) -> Result<String, Diag> {
    serde_json::from_slice(raw)
        .map_err(|e| Diag::new(format!("invalid string literal: {e}"), Span::new(0, 0)))
}

/// Copies `src[start..end]` into `out`, dropping insignificant whitespace.
/// String contents are preserved byte for byte.
pub fn copy_compact(src: &[u8], start: usize, end: usize, out: &mut Vec<u8>) {
    let start = start.min(src.len());
    let end = end.min(src.len()).max(start);
    let mut i = start;
    let mut in_string = false;
    while i < end {
        let b = src[i];
        if in_string {
            out.push(b);
            if b == b'\\' {
                i += 1;
                if i < end {
                    out.push(src[i]);
                    i += 1;
                }
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
        } else {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => i += 1,
                b'"' => {
                    in_string = true;
                    out.push(b);
                    i += 1;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        }
    }
}
