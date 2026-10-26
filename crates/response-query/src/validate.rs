//! Checks a selection set against a schema and explains what is wrong.

use crate::diag::Diag;
use crate::schema::{Schema, SchemaKind};
use crate::syntax::Selection;

pub fn validate(sel: &Selection, schema: &Schema) -> Vec<Diag> {
    let mut out = Vec::new();
    walk(sel, schema, &mut out);
    out
}

fn walk(sel: &Selection, schema: &Schema, out: &mut Vec<Diag>) {
    let node = unwrap_arrays(schema);
    let SchemaKind::Object(fields) = &node.kind else {
        return;
    };

    for field in &sel.fields {
        if field.is_wildcard() || field.excluded {
            continue;
        }

        match fields.get(&field.name) {
            None => {
                if node.open {
                    continue;
                }
                let mut message = format!("`{}` is not a field here", field.name);
                if let Some(hint) = nested_hint(&field.name) {
                    message.push_str(&format!(
                        " — names are matched literally, so nest it: `{hint}`"
                    ));
                } else if let Some(candidate) = suggest(&field.name, fields.keys()) {
                    message.push_str(&format!(" — did you mean `{candidate}`?"));
                }
                out.push(Diag::new(message, field.span));
            }
            Some(schema_field) => {
                let target = unwrap_arrays(&schema_field.schema);

                if (field.recursive || field.tree_recursive)
                    && !matches!(target.kind, SchemaKind::Object(_) | SchemaKind::Any)
                {
                    out.push(Diag::new(
                        format!(
                            "`{}` is {}, so `{}` cannot walk through it",
                            field.name,
                            target.note_pointer(),
                            if field.tree_recursive { "**" } else { "..." }
                        ),
                        field.span,
                    ));
                    continue;
                }

                if let Some(collect) = field.collect() {
                    if !matches!(
                        &schema_field.schema.kind,
                        SchemaKind::Array(_) | SchemaKind::Any
                    ) {
                        out.push(Diag::new(
                            format!(
                                "`{}` is {}, not a list, so `[...]` has nothing to iterate",
                                field.name,
                                target.note_pointer()
                            ),
                            field.span,
                        ));
                    }
                    walk(collect, &schema_field.schema, out);
                    continue;
                }

                if let Some(sub) = &field.selection {
                    match &target.kind {
                        SchemaKind::Object(_) | SchemaKind::Any => {
                            walk(sub, &schema_field.schema, out);
                        }
                        _ => out.push(Diag::new(
                            format!(
                                "`{}` is a {}, so it cannot take a selection set",
                                field.name,
                                target.note_pointer()
                            ),
                            field.span,
                        )),
                    }
                }
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

/// A dot is part of a field name, so `a.b` is looked up literally. Almost
/// nobody means that; show the nesting they probably wanted.
fn nested_hint(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }

    let mut hint = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hint.push_str(" { ");
        }
        hint.push_str(part);
    }
    for _ in 1..parts.len() {
        hint.push_str(" }");
    }
    Some(hint)
}

fn suggest<'a, I>(name: &str, candidates: I) -> Option<String>
where
    I: Iterator<Item = &'a String>,
{
    let limit = if name.len() <= 4 { 1 } else { 2 };
    let mut best: Option<(usize, &str)> = None;

    for candidate in candidates {
        let distance = levenshtein(name, candidate);
        if distance <= limit && best.map(|(d, _)| distance < d).unwrap_or(true) {
            best = Some((distance, candidate.as_str()));
        }
    }

    best.map(|(_, candidate)| candidate.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    prev[b.len()]
}
