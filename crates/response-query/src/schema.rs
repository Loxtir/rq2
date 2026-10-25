//! The internal schema type that every provider feeds.
//!
//! OpenAPI, inferred-from-samples, and "we know nothing" all produce this, so
//! the validator, autocomplete and field picker only ever see one shape.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Spec,
    Observed,
    Both,
}

impl Source {
    pub fn merge(self, other: Source) -> Source {
        if self == other {
            self
        } else {
            Source::Both
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    String,
    Integer,
    Number,
    Boolean,
}

impl ScalarKind {
    pub fn json_name(self) -> &'static str {
        match self {
            ScalarKind::String => "string",
            ScalarKind::Integer => "integer",
            ScalarKind::Number => "number",
            ScalarKind::Boolean => "boolean",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub kind: SchemaKind,
    pub nullable: bool,
    /// `additionalProperties` / unknown extra keys are allowed.
    pub open: bool,
    /// Wire-level hint that the projection language does not care about but
    /// humans do: `int64`, `timestamp`, `enum`, `bytes`, `map`, ...
    pub format: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SchemaKind {
    Object(BTreeMap<String, SchemaField>),
    Array(Box<Schema>),
    Scalar(ScalarKind),
    Any,
}

#[derive(Debug, Clone)]
pub struct SchemaField {
    pub schema: Schema,
    pub required: bool,
    pub desc: Option<String>,
    pub source: Source,
}

impl Schema {
    pub fn any() -> Self {
        Schema {
            kind: SchemaKind::Any,
            nullable: false,
            open: true,
            format: None,
        }
    }

    pub fn scalar(kind: ScalarKind) -> Self {
        Schema {
            kind: SchemaKind::Scalar(kind),
            nullable: false,
            open: false,
            format: None,
        }
    }

    pub fn object() -> Self {
        Schema {
            kind: SchemaKind::Object(BTreeMap::new()),
            nullable: false,
            open: false,
            format: None,
        }
    }

    pub fn array(items: Schema) -> Self {
        Schema {
            kind: SchemaKind::Array(Box::new(items)),
            nullable: false,
            open: false,
            format: None,
        }
    }

    /// Tags a schema with a wire format hint, e.g. a string that is really an
    /// `int64` because proto3 JSON renders 64-bit ints as strings.
    pub fn with_format(mut self, format: &str) -> Self {
        self.format = Some(format.to_string());
        self
    }

    pub fn is_any(&self) -> bool {
        matches!(&self.kind, SchemaKind::Any)
    }

    pub fn note_pointer(&self) -> &'static str {
        match &self.kind {
            SchemaKind::Any => "untyped",
            SchemaKind::Scalar(_) => "scalar",
            SchemaKind::Array(_) => "array",
            SchemaKind::Object(_) => "object",
        }
    }
}

/// Unions two observations of the same position.
pub fn merge(a: Schema, b: Schema) -> Schema {
    let nullable = a.nullable || b.nullable;
    let open = a.open || b.open;
    // Conflicting format hints are dropped rather than guessed at.
    let format = if a.format == b.format {
        a.format.clone()
    } else {
        None
    };

    let kind = match (a.kind, b.kind) {
        (SchemaKind::Any, k) => k,
        (k, SchemaKind::Any) => k,
        (SchemaKind::Object(mut fa), SchemaKind::Object(fb)) => {
            // A field that any single sample omitted is not guaranteed.
            let seen: std::collections::BTreeSet<String> = fb.keys().cloned().collect();

            for (key, incoming) in fb {
                match fa.get_mut(&key) {
                    Some(existing) => {
                        existing.required = existing.required && incoming.required;
                        existing.schema = merge(existing.schema.clone(), incoming.schema);
                        existing.source = existing.source.merge(incoming.source);
                        if existing.desc.is_none() {
                            existing.desc = incoming.desc;
                        }
                    }
                    None => {
                        let mut field = incoming;
                        field.required = false;
                        fa.insert(key, field);
                    }
                }
            }

            for (key, field) in fa.iter_mut() {
                if !seen.contains(key) {
                    field.required = false;
                }
            }

            SchemaKind::Object(fa)
        }
        (SchemaKind::Array(x), SchemaKind::Array(y)) => SchemaKind::Array(Box::new(merge(*x, *y))),
        (SchemaKind::Scalar(x), SchemaKind::Scalar(y)) if x == y => SchemaKind::Scalar(x),
        (SchemaKind::Scalar(_), SchemaKind::Scalar(_)) => SchemaKind::Any,
        _ => SchemaKind::Any,
    };

    Schema {
        kind,
        nullable,
        open,
        format,
    }
}

/// Walks a path of object keys, transparently descending through arrays.
pub fn fields_at<'s>(schema: &'s Schema, path: &[&str]) -> Option<&'s Schema> {
    let mut cur = schema;
    for seg in path {
        let mut node = cur;
        while let SchemaKind::Array(items) = &node.kind {
            node = items.as_ref();
        }
        match &node.kind {
            SchemaKind::Object(fields) => {
                cur = &fields.get(*seg)?.schema;
            }
            _ => return None,
        }
    }
    Some(cur)
}

/// Renders a schema as a JSON Schema document, so inferred shapes can be
/// handed to a team as a starting point for a missing OpenAPI spec.
pub fn to_json_schema(schema: &Schema) -> Value {
    let mut obj = Map::new();

    if schema.nullable {
        obj.insert("nullable".to_string(), json!(true));
    }
    if let Some(format) = &schema.format {
        obj.insert("format".to_string(), json!(format));
    }

    match &schema.kind {
        SchemaKind::Any => {
            obj.insert(
                "description".to_string(),
                json!("observed as untyped; refine by hand"),
            );
        }
        SchemaKind::Scalar(kind) => {
            obj.insert("type".to_string(), json!(kind.json_name()));
        }
        SchemaKind::Array(items) => {
            obj.insert("type".to_string(), json!("array"));
            obj.insert("items".to_string(), to_json_schema(items));
        }
        SchemaKind::Object(fields) => {
            obj.insert("type".to_string(), json!("object"));
            let mut props = Map::new();
            let mut required = Vec::new();
            for (name, field) in fields {
                props.insert(name.clone(), to_json_schema(&field.schema));
                if field.required {
                    required.push(json!(name));
                }
            }
            obj.insert("properties".to_string(), Value::Object(props));
            if !required.is_empty() {
                obj.insert("required".to_string(), Value::Array(required));
            }
            if schema.open {
                obj.insert("additionalProperties".to_string(), json!(true));
            }
        }
    }

    Value::Object(obj)
}
