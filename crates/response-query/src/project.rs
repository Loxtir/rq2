//! Applies a selection set to a response buffer.
//!
//! Unselected subtrees are skipped by scanning, never parsed. Verbatim copies
//! are byte slices of the original buffer with whitespace stripped.

use crate::diag::{Diag, Span};
use crate::scan::{copy_compact, decode_string, strip_bom, Scanner, MAX_DEPTH};
use crate::syntax::{Field, Selection, Slice};

/// Set while projecting one node of a recursive walk. It tells the projector
/// to leave the traversed edge out of the output, and records where that edge
/// was found so the walk can descend without scanning the node a second time.
#[derive(Default)]
struct Watch<'a> {
    edge: Option<&'a str>,
    hit: Option<(usize, usize)>,
    /// Unlike a flat walk, a tree walk emits the edge and repeats its selection.
    tree: Option<&'a Field>,
}

/// What to keep from each node of a list.
enum Mode<'a> {
    /// `[id]` — one bare scalar per node.
    Leaf(&'a str),
    /// `{ id name }` or `[{ id name }]`.
    Select(&'a Selection),
    /// No selection at all: copy the element through.
    Verbatim,
}

/// Projects `input` through `sel`, returning compact JSON.
pub fn project(input: &[u8], sel: &Selection) -> Result<Vec<u8>, Diag> {
    let input = strip_bom(input);
    let mut sc = Scanner::new(input);
    let mut out = Vec::with_capacity(input.len().min(64 * 1024));
    let mut watch = Watch::default();
    project_value(&mut sc, sel, &mut out, 0, &mut watch, &[])?;
    Ok(out)
}

/// Convenience wrapper that assumes the projection is valid UTF-8.
pub fn project_to_string(input: &[u8], sel: &Selection) -> Result<String, Diag> {
    let bytes = project(input, sel)?;
    String::from_utf8(bytes).map_err(|e| {
        Diag::new(
            format!("projection produced invalid utf-8: {e}"),
            Span::new(0, 0),
        )
    })
}

fn project_value(
    sc: &mut Scanner,
    sel: &Selection,
    out: &mut Vec<u8>,
    depth: usize,
    watch: &mut Watch,
    exclusions: &[&str],
) -> Result<(), Diag> {
    if depth > MAX_DEPTH {
        return Err(Diag::new("response nesting is too deep", sc.span_here()));
    }
    sc.skip_ws();
    match sc.peek() {
        Some(b'{') => project_object(sc, sel, out, depth, watch, exclusions),
        Some(b'[') => project_array(sc, sel, out, depth, watch.tree, exclusions),
        None => Err(Diag::new("unexpected end of input", sc.span_here())),
        // Scalars pass through unchanged so a selection over a mixed array works.
        _ => {
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            copy_compact(sc.src(), start, end, out);
            Ok(())
        }
    }
}

fn project_object(
    sc: &mut Scanner,
    sel: &Selection,
    out: &mut Vec<u8>,
    depth: usize,
    watch: &mut Watch,
    exclusions: &[&str],
) -> Result<(), Diag> {
    let mut inherited = exclusions.to_vec();
    for field in &sel.fields {
        if field.excluded && field.recursive && !inherited.contains(&field.name.as_str()) {
            inherited.push(&field.name);
        }
    }
    let exclusions = inherited.as_slice();
    sc.expect(b'{')?;
    out.push(b'{');
    let mut first = true;
    let mut matched = vec![false; sel.fields.len()];

    loop {
        sc.skip_ws();
        match sc.peek() {
            Some(b'}') => {
                sc.expect(b'}')?;
                break;
            }
            None => return Err(Diag::new("unclosed `{` in response", sc.span_here())),
            _ => {}
        }

        let key_raw = sc.scan_string_raw()?;
        sc.expect(b':')?;
        sc.skip_ws();

        let idx = match_key(sel, key_raw)?;
        if let Some(idx) = idx {
            matched[idx] = true;
        }
        let excluded = match watch.edge {
            Some(edge) => key_equals(key_raw, edge)?,
            None => false,
        };

        let hidden = key_matches_any(
            key_raw,
            exclusions.iter().copied().chain(
                sel.fields
                    .iter()
                    .filter(|field| field.excluded)
                    .map(|field| field.name.as_str()),
            ),
        )?;
        let tree = match watch.tree {
            Some(field) if key_equals(key_raw, &field.name)? => Some(field),
            _ => None,
        };

        if hidden {
            sc.scan_value_end()?;
        } else if excluded {
            // The edge being walked is never part of the output, even when the
            // selection is `*`. Otherwise every level would re-emit its own
            // subtree and the flat list would be quadratic.
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            if watch.hit.is_none() {
                watch.hit = Some((start, end));
            }
        } else if let Some(field) = tree {
            if !first {
                out.push(b',');
            }
            first = false;
            // An alias names the starting edge only; descendants keep their key.
            write_json_string(&decode_string(key_raw)?, out)?;
            out.push(b':');
            apply_field(sc, field, out, depth, exclusions)?;
        } else if let Some(idx) = idx {
            let field = &sel.fields[idx];
            if !first {
                out.push(b',');
            }
            first = false;
            let emitted = emit_name(field, key_raw)?;
            write_json_string(&emitted, out)?;
            out.push(b':');
            apply_field(sc, field, out, depth, exclusions)?;
        } else {
            sc.scan_value_end()?;
        }

        sc.skip_ws();
        match sc.peek() {
            Some(b',') => sc.expect(b',')?,
            Some(b'}') => {}
            None => return Err(Diag::new("unclosed `{` in response", sc.span_here())),
            _ => return Err(Diag::new("expected `,` or `}`", sc.span_here())),
        }
    }

    out.push(b'}');

    for (i, f) in sel.fields.iter().enumerate() {
        if f.required && !matched[i] {
            return Err(Diag::new(
                format!("required field `{}` is missing from the response", f.name),
                f.span,
            ));
        }
    }

    Ok(())
}

fn project_array(
    sc: &mut Scanner,
    sel: &Selection,
    out: &mut Vec<u8>,
    depth: usize,
    tree: Option<&Field>,
    exclusions: &[&str],
) -> Result<(), Diag> {
    sc.expect(b'[')?;
    out.push(b'[');
    let mut first = true;

    loop {
        sc.skip_ws();
        if sc.peek() == Some(b']') {
            sc.expect(b']')?;
            break;
        }
        if sc.peek().is_none() {
            return Err(Diag::new("unclosed `[` in response", sc.span_here()));
        }
        if !first {
            out.push(b',');
        }
        first = false;
        // The watch belongs to one object level; inside an array it is a new one.
        project_value(
            sc,
            sel,
            out,
            depth + 1,
            &mut Watch {
                tree,
                ..Watch::default()
            },
            exclusions,
        )?;
        sc.skip_ws();
        match sc.peek() {
            Some(b',') => sc.expect(b',')?,
            Some(b']') => {}
            None => return Err(Diag::new("unclosed `[` in response", sc.span_here())),
            _ => return Err(Diag::new("expected `,` or `]`", sc.span_here())),
        }
    }

    out.push(b']');
    Ok(())
}

fn apply_field(
    sc: &mut Scanner,
    field: &Field,
    out: &mut Vec<u8>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    sc.skip_ws();

    if field.recursive {
        return apply_recursive(sc, field, out, depth, exclusions);
    }
    if field.tree_recursive {
        let sub = field
            .selection
            .as_ref()
            .expect("tree recursion has a selection");
        return project_value(
            sc,
            sub,
            out,
            depth + 1,
            &mut Watch {
                tree: Some(field),
                ..Watch::default()
            },
            exclusions,
        );
    }

    let slice = field.slice();
    let collect = field.collect();

    if (slice.is_some() || collect.is_some()) && sc.peek() == Some(b'[') {
        return apply_list(sc, field, slice, collect, out, depth, exclusions);
    }

    match (sc.peek(), field.selection.as_ref()) {
        (Some(b'{'), Some(sub)) => {
            project_value(sc, sub, out, depth + 1, &mut Watch::default(), exclusions)
        }
        (Some(b'['), Some(sub)) => project_array(sc, sub, out, depth + 1, None, exclusions),
        _ => copy_filtered(sc, out, depth + 1, exclusions),
    }
}

/// Copies a whole value, filtering inherited recursive exclusions in place.
/// With no exclusions this retains the original scanner's fast verbatim path.
fn copy_filtered(
    sc: &mut Scanner,
    out: &mut Vec<u8>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    sc.skip_ws();
    if exclusions.is_empty() {
        let start = sc.pos();
        let end = sc.scan_value_end()?;
        copy_compact(sc.src(), start, end, out);
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err(Diag::new("response nesting is too deep", sc.span_here()));
    }
    let open = match sc.peek() {
        Some(b'{') => b'{',
        Some(b'[') => b'[',
        _ => {
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            copy_compact(sc.src(), start, end, out);
            return Ok(());
        }
    };
    let close = if open == b'{' { b'}' } else { b']' };
    sc.expect(open)?;
    out.push(open);
    let mut first = true;
    loop {
        sc.skip_ws();
        if sc.peek() == Some(close) {
            sc.expect(close)?;
            break;
        }
        let key = if open == b'{' {
            let key = sc.scan_string_raw()?;
            sc.expect(b':')?;
            Some(key)
        } else {
            None
        };
        let hidden = match key {
            Some(key) => key_matches_any(key, exclusions.iter().copied())?,
            None => false,
        };
        if hidden {
            sc.scan_value_end()?;
        } else {
            if !first {
                out.push(b',');
            }
            first = false;
            if let Some(key) = key {
                out.extend_from_slice(key);
                out.push(b':');
            }
            copy_filtered(sc, out, depth + 1, exclusions)?;
        }
        sc.skip_ws();
        if sc.peek() == Some(b',') {
            sc.expect(b',')?;
        } else if sc.peek() != Some(close) {
            return Err(Diag::new(
                "expected a comma or closing delimiter",
                sc.span_here(),
            ));
        }
    }
    out.push(close);
    Ok(())
}

/// `items[0:5]`, `items[id]`, `items[0:5][id]`. The slice narrows first, then
/// the collect runs once per surviving element.
fn apply_list(
    sc: &mut Scanner,
    field: &Field,
    slice: Option<Slice>,
    collect: Option<&Selection>,
    out: &mut Vec<u8>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    let elems = sc.split_array_elements()?;
    let len = elems.len() as i64;
    let (start, end) = resolve_range(slice, len);

    out.push(b'[');
    let mut first = true;
    for (s, e) in elems.iter().take(end).skip(start) {
        if !first {
            out.push(b',');
        }
        first = false;
        emit_one(
            sc.src(),
            *s,
            *e,
            field,
            collect,
            out,
            depth,
            &mut Watch::default(),
            exclusions,
        )?;
    }
    out.push(b']');
    Ok(())
}

/// `children...` — every node reachable through this same key, flattened into
/// one array in document order. Depth-first pre-order.
fn apply_recursive(
    sc: &mut Scanner,
    field: &Field,
    out: &mut Vec<u8>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    let mut nodes: Vec<Vec<u8>> = Vec::new();
    collect_edge(sc, field, &mut nodes, depth, exclusions)?;

    let (start, end) = resolve_range(field.slice(), nodes.len() as i64);

    out.push(b'[');
    let mut first = true;
    for node in nodes.into_iter().take(end).skip(start) {
        if !first {
            out.push(b',');
        }
        first = false;
        out.extend_from_slice(&node);
    }
    out.push(b']');
    Ok(())
}

/// Walks the value of one edge. The value is normally an array of nodes, but a
/// single object works too, which covers linked lists (`next...`).
fn collect_edge(
    sc: &mut Scanner,
    field: &Field,
    nodes: &mut Vec<Vec<u8>>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    if depth > MAX_DEPTH {
        return Err(Diag::new(
            format!(
                "`{}...` is deeper than the {MAX_DEPTH}-level nesting limit",
                field.name
            ),
            sc.span_here(),
        ));
    }

    sc.skip_ws();
    match sc.peek() {
        Some(b'[') => {
            for (start, end) in sc.split_array_elements()? {
                descend(sc.src(), start, end, field, nodes, depth + 1, exclusions)?;
            }
            Ok(())
        }
        Some(b'{') => {
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            descend(sc.src(), start, end, field, nodes, depth + 1, exclusions)
        }
        None => Err(Diag::new("unexpected end of input", sc.span_here())),
        // A leaf branch: the edge is null or a scalar, so the walk stops here.
        _ => {
            sc.scan_value_end()?;
            Ok(())
        }
    }
}

/// Emits one node, then continues into that node's own edge.
fn descend(
    src: &[u8],
    start: usize,
    end: usize,
    field: &Field,
    nodes: &mut Vec<Vec<u8>>,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    let mut watch = Watch {
        edge: Some(&field.name),
        hit: None,
        tree: None,
    };
    let mut buf = Vec::new();
    emit_one(
        src,
        start,
        end,
        field,
        field.collect(),
        &mut buf,
        depth,
        &mut watch,
        exclusions,
    )?;
    nodes.push(buf);

    if let Some((child_start, _)) = watch.hit {
        let mut child = Scanner::at(src, child_start);
        collect_edge(&mut child, field, nodes, depth, exclusions)?;
    }
    Ok(())
}

/// Emits one element or node according to the field's selection shape.
#[allow(clippy::too_many_arguments)]
fn emit_one(
    src: &[u8],
    start: usize,
    end: usize,
    field: &Field,
    collect: Option<&Selection>,
    out: &mut Vec<u8>,
    depth: usize,
    watch: &mut Watch,
    exclusions: &[&str],
) -> Result<(), Diag> {
    match mode(field, collect) {
        Mode::Leaf(leaf) => {
            let mut sc = Scanner::at(src, start);
            project_leaf(&mut sc, leaf, out, watch, depth + 1, exclusions)
        }
        Mode::Select(sel) => {
            let mut sc = Scanner::at(src, start);
            project_value(&mut sc, sel, out, depth + 1, watch, exclusions)
        }
        Mode::Verbatim => {
            if exclusions.is_empty() {
                copy_compact(src, start, end, out);
                Ok(())
            } else {
                let mut sc = Scanner::at(src, start);
                copy_filtered(&mut sc, out, depth + 1, exclusions)
            }
        }
    }
}

fn mode<'a>(field: &'a Field, collect: Option<&'a Selection>) -> Mode<'a> {
    match collect {
        Some(sel) => match sel.single_leaf() {
            Some(leaf) => Mode::Leaf(leaf),
            None => Mode::Select(sel),
        },
        None => match field.selection.as_ref() {
            Some(sel) => Mode::Select(sel),
            None => Mode::Verbatim,
        },
    }
}

/// Copies just `leaf`'s value out of an object, for `[id]`. A node without the
/// field contributes `null` so element positions stay stable.
fn project_leaf(
    sc: &mut Scanner,
    leaf: &str,
    out: &mut Vec<u8>,
    watch: &mut Watch,
    depth: usize,
    exclusions: &[&str],
) -> Result<(), Diag> {
    sc.skip_ws();
    if sc.peek() != Some(b'{') {
        out.extend_from_slice(b"null");
        return Ok(());
    }

    sc.expect(b'{')?;
    let mut found = false;

    loop {
        sc.skip_ws();
        match sc.peek() {
            Some(b'}') => {
                sc.expect(b'}')?;
                break;
            }
            None => return Err(Diag::new("unclosed `{` in response", sc.span_here())),
            _ => {}
        }

        let key_raw = sc.scan_string_raw()?;
        sc.expect(b':')?;
        sc.skip_ws();

        let excluded = match watch.edge {
            Some(edge) => key_equals(key_raw, edge)?,
            None => false,
        };

        if key_matches_any(key_raw, exclusions.iter().copied())? {
            sc.scan_value_end()?;
        } else if excluded {
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            if watch.hit.is_none() {
                watch.hit = Some((start, end));
            }
        } else if !found && key_equals(key_raw, leaf)? {
            found = true;
            copy_filtered(sc, out, depth + 1, exclusions)?;
        } else {
            sc.scan_value_end()?;
        }

        sc.skip_ws();
        match sc.peek() {
            Some(b',') => sc.expect(b',')?,
            Some(b'}') => {}
            None => return Err(Diag::new("unclosed `{` in response", sc.span_here())),
            _ => return Err(Diag::new("expected `,` or `}`", sc.span_here())),
        }
    }

    if !found {
        out.extend_from_slice(b"null");
    }
    Ok(())
}

fn resolve_range(slice: Option<Slice>, len: i64) -> (usize, usize) {
    let start = resolve_index(slice.and_then(|s| s.start), len, 0).clamp(0, len) as usize;
    let end = resolve_index(slice.and_then(|s| s.end), len, len).clamp(0, len) as usize;
    (start, end.max(start))
}

fn resolve_index(value: Option<i64>, len: i64, default: i64) -> i64 {
    match value {
        None => default,
        Some(n) if n < 0 => (len + n).max(0),
        Some(n) => n,
    }
}

fn match_key(sel: &Selection, key_raw: &[u8]) -> Result<Option<usize>, Diag> {
    if key_raw.len() < 2 {
        return Ok(None);
    }
    let inner = &key_raw[1..key_raw.len() - 1];
    if inner.contains(&b'\\') {
        let decoded = decode_string(key_raw)?;
        Ok(sel.find(&decoded))
    } else {
        match std::str::from_utf8(inner) {
            Ok(s) => Ok(sel.find(s)),
            Err(_) => Ok(None),
        }
    }
}

fn key_equals(key_raw: &[u8], name: &str) -> Result<bool, Diag> {
    if key_raw.len() < 2 {
        return Ok(false);
    }
    let inner = &key_raw[1..key_raw.len() - 1];
    if inner.contains(&b'\\') {
        return Ok(decode_string(key_raw)? == name);
    }
    Ok(std::str::from_utf8(inner)
        .map(|s| s == name)
        .unwrap_or(false))
}

fn key_matches_any<'a>(
    key_raw: &[u8],
    names: impl IntoIterator<Item = &'a str>,
) -> Result<bool, Diag> {
    for name in names {
        if key_equals(key_raw, name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn emit_name(field: &Field, key_raw: &[u8]) -> Result<String, Diag> {
    if field.is_wildcard() {
        decode_string(key_raw)
    } else {
        Ok(field.alias.clone().unwrap_or_else(|| field.name.clone()))
    }
}

fn write_json_string(s: &str, out: &mut Vec<u8>) -> Result<(), Diag> {
    serde_json::to_writer(&mut *out, s)
        .map_err(|e| Diag::new(format!("failed to write key: {e}"), Span::new(0, 0)))
}
