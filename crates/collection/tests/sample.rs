//! The sample workspace, used as documentation and as a live demo.
//!
//! It is the only place where the whole scope chain appears at once: a global
//! origin, a collection URL composed from it, a folder limit, a request's own
//! variable feeding a path parameter, and an environment that overrides the
//! global. Pinning it here keeps the YAML in `examples/` honest.

use collection::send::variables;
use collection::store;
use std::path::Path;

fn sample() -> store::Workspace {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/jsonplaceholder");
    store::load(&root).expect("the sample workspace loads")
}

#[test]
fn the_sample_composes_globals_and_collection_values() {
    let workspace = sample();
    let collection = workspace
        .collection("JSONPlaceholder")
        .expect("the JSONPlaceholder collection");

    let resolved = variables(collection, None, &workspace.globals, &[0, 0]);
    assert_eq!(
        resolved.get("baseUrl").map(String::as_str),
        Some("https://jsonplaceholder.typicode.com")
    );
}

#[test]
fn the_sample_reads_a_folder_and_a_request_scope() {
    let workspace = sample();
    let collection = workspace
        .collection("JSONPlaceholder")
        .expect("the JSONPlaceholder collection");

    // Posts folder: `limit` for the list request.
    let list = variables(collection, None, &workspace.globals, &[0, 0]);
    assert_eq!(list.get("limit").map(String::as_str), Some("20"));

    // Get post: its own `postNumber` feeds the `:postId` path parameter.
    let get = variables(collection, None, &workspace.globals, &[0, 1]);
    assert_eq!(get.get("postNumber").map(String::as_str), Some("1"));
    assert_eq!(
        get.get("limit").map(String::as_str),
        Some("20"),
        "a request still sees the folder above it"
    );
}

#[test]
fn the_sample_environment_overrides_the_global_origin() {
    let workspace = sample();
    let collection = workspace
        .collection("JSONPlaceholder")
        .expect("the JSONPlaceholder collection");
    let local = workspace
        .environments
        .iter()
        .find(|environment| environment.name == "local")
        .expect("the local environment");

    let resolved = variables(collection, Some(local), &workspace.globals, &[0, 0]);
    assert_eq!(
        resolved.get("baseUrl").map(String::as_str),
        Some("http://127.0.0.1:8788")
    );
}
