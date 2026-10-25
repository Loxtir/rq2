//! Generates a starter query from a schema.
//!
//! This is the inverse of projection, and it is the cheapest way to check that
//! a schema shape actually maps onto the query language: scaffold, parse, then
//! validate the result. If that round-trip fails, the schema model has a hole.

use crate::schema::{Schema, SchemaField, SchemaKind};
use std::collections::BTreeMap;
use std::fmt::Write;

const INDENT: &str = "  ";

pub fn scaffold(schema: &Schema, max_depth: usize) -> String {
    let mut out = String::from("{\n");
    let node = unwrap_arrays(schema);
    if let SchemaKind::Object(fields) = &node.kind {
        emit_fields(fields, max_depth, 1, &mut out);
    }
    out.push_str("}\n");
    out
}

fn emit_fields(
    fields: &BTreeMap<String, SchemaField>,
    max_depth: usize,
    level: usize,
    out: &mut String,
) {
    let pad = INDENT.repeat(level);

    for (name, field) in fields {
        // A name the parser cannot express as a bare identifier is skipped
        // rather than emitted as something that will not re-parse.
        if !is_expressible(name) {
            continue;
        }

        let target = unwrap_arrays(&field.schema);
        match &target.kind {
            // Stop nesting at the depth limit, and emit the subtree verbatim
            // instead. A generated query should never be enormous.
            SchemaKind::Object(inner) if level < max_depth && !inner.is_empty() => {
                let _ = writeln!(out, "{pad}{name} {{");
                emit_fields(inner, max_depth, level + 1, out);
                let _ = writeln!(out, "{pad}}}");
            }
            _ => {
                let _ = writeln!(out, "{pad}{name}");
            }
        }
    }
}

fn unwrap_arrays(schema: &Schema) -> &Schema {
    let mut cur = schema;
    while let SchemaKind::Array(items) = &cur.kind {
        cur = items.as_ref();
    }
    cur
}

fn is_expressible(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|c| {
            matches!(c, '{' | '}' | ':' | '[' | ']' | '!' | '*' | ',' | '#') || c.is_whitespace()
        })
}
