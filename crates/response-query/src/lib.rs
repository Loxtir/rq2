//! Projection language for JSON API responses.
//!
//! A query is a *view* over a stored response, not a destructive filter:
//!
//! ```text
//! {
//!   id!
//!   profile { displayName avatar }
//!   recentOrders[:5] { id: orderId total }
//!   metadata
//! }
//! ```
//!
//! - a field with no selection set returns its value verbatim
//! - `*` matches every key not listed explicitly
//! - `field!` fails loudly when the response omits it
//! - `field == "value"` and `field != "value"` conditionally emit a field
//! - `field in ["a", "b"]` and `field not in [...]` test scalar membership
//! - `items[? name == "Mike"] { name age }` filters objects before projection
//! - `[a:b]` slices an array, negative indices count from the end
//! - `[id]` collects one value per element into a flat list
//! - `children...{ id }` walks a repeated key to any depth
//! - `children**{ id }` repeats a projection while retaining the nested tree
//! - `{ * !secret }` omits a key here; `{ * !secret... }` omits it at any depth

pub mod diag;
pub mod infer;
pub mod literal;
pub mod openapi;
pub mod project;
pub mod scaffold;
pub mod scan;
pub mod schema;
pub mod syntax;
pub mod validate;

pub use diag::{Diag, Span};
pub use infer::infer;
pub use project::{project, project_to_string};
pub use scaffold::scaffold;
pub use schema::{
    fields_at, merge, to_json_schema, ScalarKind, Schema, SchemaField, SchemaKind, Source,
};
pub use syntax::{parse, Condition, Field, Selection, Slice, Step};
pub use validate::validate;
