//! Minimal OpenAPI 3 reader: enough to answer "what shape does this operation
//! return", which is all the query language needs.
//!
//! Supported: local `$ref`, `allOf` (merge), `oneOf`/`anyOf` (union, with the
//! caveat that two branches declaring the same key degenerate to a looser
//! type), `additionalProperties`, `nullable`, and OpenAPI 3.1 `type: [...]`.
//! Remote `$ref`s are not fetched; they degrade to `Any`.

use crate::diag::{Diag, Span};
use crate::schema::{merge, ScalarKind, Schema, SchemaField, SchemaKind, Source};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

const MAX_REF_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub summary: String,
}

impl Operation {
    pub fn display_id(&self) -> &str {
        if self.id.is_empty() {
            "-"
        } else {
            &self.id
        }
    }
}

#[derive(Debug)]
pub struct Spec {
    doc: Value,
}

impl Spec {
    /// Accepts JSON or YAML. JSON is tried first because it is faster and
    /// unambiguous; YAML is the common format for hand-written specs.
    pub fn parse(bytes: &[u8]) -> Result<Self, Diag> {
        let bytes = crate::scan::strip_bom(bytes);

        let doc: Value = match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(json_error) => serde_norway::from_slice(bytes).map_err(|yaml_error| {
                Diag::new(
                    format!("spec is neither JSON ({json_error}) nor YAML ({yaml_error})"),
                    Span::new(0, 1),
                )
            })?,
        };

        if doc.get("openapi").is_none() && doc.get("swagger").is_none() {
            return Err(Diag::new(
                "missing `openapi` field; is this an OpenAPI document?",
                Span::new(0, 1),
            ));
        }

        Ok(Spec { doc })
    }

    pub fn operations(&self) -> Vec<Operation> {
        let mut out = Vec::new();
        let Some(paths) = self.doc.get("paths").and_then(|v| v.as_object()) else {
            return out;
        };
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                continue;
            };
            for method in HTTP_METHODS {
                let Some(op) = item.get(method).and_then(|v| v.as_object()) else {
                    continue;
                };
                out.push(Operation {
                    id: op
                        .get("operationId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    method: method.to_uppercase(),
                    path: path.clone(),
                    summary: op
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
        out
    }

    /// Response schema for an operation, preferring `200`, then any 2xx.
    pub fn response_schema(&self, operation_id: &str) -> Result<Option<Schema>, Diag> {
        let Some((path, method)) = self.find_operation(operation_id) else {
            return Ok(None);
        };

        let pointer = format!("/paths/{}/{}", escape_pointer(path), method);
        let Some(op) = self.doc.pointer(&pointer) else {
            return Ok(None);
        };

        let Some(responses) = op.get("responses").and_then(|v| v.as_object()) else {
            return Ok(None);
        };

        let mut chosen: Option<&Value> = responses.get("200");
        if chosen.is_none() {
            for (code, resp) in responses {
                if code.starts_with('2') {
                    chosen = Some(resp);
                    break;
                }
            }
        }
        let Some(response) = chosen else {
            return Ok(None);
        };

        let Some(content) = response.get("content").and_then(|v| v.as_object()) else {
            return Ok(None);
        };

        let media = content
            .get("application/json")
            .or_else(|| content.get("application/*+json"))
            .or_else(|| {
                content
                    .iter()
                    .find(|(key, _)| key.contains("json"))
                    .map(|(_, value)| value)
            });

        let Some(media) = media else {
            return Ok(None);
        };
        let Some(node) = media.get("schema") else {
            return Ok(None);
        };

        Ok(Some(self.resolve(node, 0)?))
    }

    fn find_operation(&self, id: &str) -> Option<(&str, &str)> {
        let paths = self.doc.get("paths")?.as_object()?;
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                continue;
            };
            for method in HTTP_METHODS {
                let Some(op) = item.get(method) else {
                    continue;
                };
                if op.get("operationId").and_then(|v| v.as_str()) == Some(id) {
                    return Some((path.as_str(), method));
                }
            }
        }
        None
    }

    fn resolve(&self, node: &Value, depth: usize) -> Result<Schema, Diag> {
        let mut stack = Vec::new();
        self.resolve_at(node, depth, &mut stack, true)
    }

    /// `follow_refs` is turned off one level below a cycle. The result still has
    /// the object's field names, so `children...` can be validated and completed
    /// at one level of depth, but the expansion cannot run away.
    fn resolve_at(
        &self,
        node: &Value,
        depth: usize,
        stack: &mut Vec<String>,
        follow_refs: bool,
    ) -> Result<Schema, Diag> {
        if depth > MAX_REF_DEPTH {
            return Ok(Schema::any());
        }

        let Some(obj) = node.as_object() else {
            return Ok(Schema::any());
        };

        if let Some(reference) = obj.get("$ref").and_then(|v| v.as_str()) {
            if !follow_refs {
                return Ok(Schema::any());
            }
            if let Some(pointer) = reference.strip_prefix('#') {
                if let Some(target) = self.doc.pointer(pointer) {
                    // A ref already on the expansion stack is recursive. Expanding
                    // a model with both `parent` and `children` eagerly is
                    // exponential, so expand it once more without following refs.
                    if stack.iter().any(|seen| seen.as_str() == pointer) {
                        return self.resolve_at(target, depth + 1, stack, false);
                    }
                    stack.push(pointer.to_string());
                    let resolved = self.resolve_at(target, depth + 1, stack, true);
                    stack.pop();
                    return resolved;
                }
            }
            return Ok(Schema::any());
        }

        let mut nullable = obj
            .get("nullable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut type_name: Option<&str> = None;
        match obj.get("type") {
            Some(Value::String(s)) => type_name = Some(s.as_str()),
            Some(Value::Array(types)) => {
                for entry in types {
                    if let Some(name) = entry.as_str() {
                        if name == "null" {
                            nullable = true;
                        } else if type_name.is_none() {
                            type_name = Some(name);
                        }
                    }
                }
            }
            _ => {}
        }

        if let Some(all_of) = obj.get("allOf").and_then(|v| v.as_array()) {
            let mut acc: Option<Schema> = None;
            for item in all_of {
                let one = self.resolve_at(item, depth + 1, stack, follow_refs)?;
                acc = Some(match acc {
                    None => one,
                    Some(prev) => merge(prev, one),
                });
            }
            let mut base = acc.unwrap_or_else(Schema::any);
            if obj.contains_key("properties") {
                base = merge(base, self.object_props(obj, depth, stack, follow_refs)?);
            }
            base.nullable = base.nullable || nullable;
            return Ok(base);
        }

        if let Some(union) = obj
            .get("oneOf")
            .or_else(|| obj.get("anyOf"))
            .and_then(|v| v.as_array())
        {
            let mut acc: Option<Schema> = None;
            for item in union {
                let one = self.resolve_at(item, depth + 1, stack, follow_refs)?;
                acc = Some(match acc {
                    None => one,
                    Some(prev) => merge(prev, one),
                });
            }
            let mut base = acc.unwrap_or_else(Schema::any);
            base.nullable = base.nullable || nullable;
            return Ok(base);
        }

        let mut schema = match type_name {
            Some("object") => self.object_props(obj, depth, stack, follow_refs)?,
            Some("array") => {
                let items = match obj.get("items") {
                    Some(items) => self.resolve_at(items, depth + 1, stack, follow_refs)?,
                    None => Schema::any(),
                };
                Schema::array(items)
            }
            Some("string") => Schema::scalar(ScalarKind::String),
            Some("integer") => Schema::scalar(ScalarKind::Integer),
            Some("number") => Schema::scalar(ScalarKind::Number),
            Some("boolean") => Schema::scalar(ScalarKind::Boolean),
            Some(_) => Schema::any(),
            None => {
                if obj.contains_key("properties") {
                    self.object_props(obj, depth, stack, follow_refs)?
                } else {
                    Schema::any()
                }
            }
        };

        schema.nullable = schema.nullable || nullable;
        Ok(schema)
    }

    fn object_props(
        &self,
        obj: &Map<String, Value>,
        depth: usize,
        stack: &mut Vec<String>,
        follow_refs: bool,
    ) -> Result<Schema, Diag> {
        let required: Vec<&str> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut fields: BTreeMap<String, SchemaField> = BTreeMap::new();
        if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
            for (name, prop) in props {
                let schema = self.resolve_at(prop, depth + 1, stack, follow_refs)?;
                let desc = prop
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                fields.insert(
                    name.clone(),
                    SchemaField {
                        schema,
                        required: required.contains(&name.as_str()),
                        desc,
                        source: Source::Spec,
                    },
                );
            }
        }

        let open = match obj.get("additionalProperties") {
            Some(Value::Bool(flag)) => *flag,
            Some(Value::Object(_)) => true,
            _ => false,
        };

        Ok(Schema {
            kind: SchemaKind::Object(fields),
            nullable: false,
            open,
            format: None,
        })
    }
}

fn escape_pointer(path: &str) -> String {
    path.replace('~', "~0").replace('/', "~1")
}
