//! Builds a schema from responses that were actually observed.
//!
//! This is what makes the APIs without an OpenAPI spec first-class: after one
//! successful call the endpoint has types, and after a few calls it has
//! optionality too.

use crate::diag::Diag;
use crate::scan::{decode_string, strip_bom, Scanner, MAX_DEPTH};
use crate::schema::{merge, ScalarKind, Schema, SchemaField, SchemaKind, Source};
use std::collections::BTreeMap;

pub fn infer(samples: &[&[u8]]) -> Result<Schema, Diag> {
    let mut acc: Option<Schema> = None;

    for sample in samples {
        let mut sc = Scanner::new(strip_bom(sample));
        let one = infer_value(&mut sc, 0)?;
        acc = Some(match acc {
            None => one,
            Some(prev) => merge(prev, one),
        });
    }

    Ok(acc.unwrap_or_else(Schema::any))
}

fn infer_value(sc: &mut Scanner, depth: usize) -> Result<Schema, Diag> {
    if depth > MAX_DEPTH {
        return Err(Diag::new("response nesting is too deep", sc.span_here()));
    }

    sc.skip_ws();
    match sc.peek() {
        None => Err(Diag::new("unexpected end of input", sc.span_here())),

        Some(b'{') => {
            sc.expect(b'{')?;
            let mut fields: BTreeMap<String, SchemaField> = BTreeMap::new();
            loop {
                sc.skip_ws();
                if sc.peek() == Some(b'}') {
                    sc.expect(b'}')?;
                    break;
                }
                if sc.peek().is_none() {
                    return Err(Diag::new("unclosed `{`", sc.span_here()));
                }
                let key = decode_string(sc.scan_string_raw()?)?;
                sc.expect(b':')?;
                let value = infer_value(sc, depth + 1)?;
                fields.insert(
                    key,
                    SchemaField {
                        schema: value,
                        required: true,
                        desc: None,
                        source: Source::Observed,
                    },
                );
                sc.skip_ws();
                match sc.peek() {
                    Some(b',') => sc.expect(b',')?,
                    Some(b'}') => {}
                    None => return Err(Diag::new("unclosed `{`", sc.span_here())),
                    _ => return Err(Diag::new("expected `,` or `}`", sc.span_here())),
                }
            }
            Ok(Schema {
                kind: SchemaKind::Object(fields),
                nullable: false,
                open: false,
                format: None,
            })
        }

        Some(b'[') => {
            let elems = sc.split_array_elements()?;
            let mut items: Option<Schema> = None;
            for (start, _end) in &elems {
                let mut inner = Scanner::at(sc.src(), *start);
                let one = infer_value(&mut inner, depth + 1)?;
                items = Some(match items {
                    None => one,
                    Some(prev) => merge(prev, one),
                });
            }
            // An empty array tells us nothing about the element type.
            Ok(Schema::array(items.unwrap_or_else(Schema::any)))
        }

        Some(b'"') => {
            sc.scan_string_raw()?;
            Ok(Schema::scalar(ScalarKind::String))
        }

        Some(b't') | Some(b'f') => {
            sc.scan_value_end()?;
            Ok(Schema::scalar(ScalarKind::Boolean))
        }

        Some(b'n') => {
            sc.scan_value_end()?;
            Ok(Schema {
                kind: SchemaKind::Any,
                nullable: true,
                open: true,
                format: None,
            })
        }

        Some(_) => {
            let start = sc.pos();
            let end = sc.scan_value_end()?;
            let raw = sc.slice(start, end);
            let float = raw.iter().any(|b| matches!(b, b'.' | b'e' | b'E'));
            Ok(Schema::scalar(if float {
                ScalarKind::Number
            } else {
                ScalarKind::Integer
            }))
        }
    }
}
