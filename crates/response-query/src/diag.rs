//! Diagnostics with byte spans that render as line:col against the source text.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span {
            start,
            end: end.max(start),
        }
    }

    pub fn point(pos: usize) -> Self {
        Span::new(pos, pos + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub message: String,
    pub span: Span,
}

impl Diag {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diag {
            message: message.into(),
            span,
        }
    }

    pub fn at(message: impl Into<String>, pos: usize) -> Self {
        Diag::new(message, Span::point(pos))
    }

    /// 1-based line and column of this diagnostic's start offset.
    pub fn line_col(&self, source: &str) -> (usize, usize) {
        line_col(source, self.span.start)
    }

    /// `line:col: message` for showing next to editor gutters.
    pub fn render(&self, source: &str) -> String {
        let (line, col) = self.line_col(source);
        format!("{line}:{col}: {}", self.message)
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.span.start)
    }
}

impl std::error::Error for Diag {}

pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
