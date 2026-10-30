//! Finding a collection or environment by what a user or the window passes.

use collection::model::{Collection, Environment};
use collection::store::Workspace;

fn workspace() -> Workspace {
    let collection = |name: &str| -> Collection {
        serde_json::from_value(serde_json::json!({ "name": name })).expect("collection")
    };
    let environment = |name: &str| -> Environment {
        serde_json::from_value(serde_json::json!({ "name": name })).expect("environment")
    };
    Workspace {
        collections: vec![collection("Orders API"), collection("Users")],
        environments: vec![environment("Local Dev"), environment("local-dev")],
        globals: Vec::new(),
        scripts: Default::default(),
    }
}

#[test]
fn a_request_is_found_exactly_first_then_by_a_unique_substring() {
    let collection: Collection = serde_json::from_value(serde_json::json!({
        "name": "Orders",
        "items": [
            { "name": "List orders", "request": { "method": "GET", "url": "/orders" } },
            { "name": "List", "request": { "method": "GET", "url": "/" } },
            { "name": "Get order", "request": { "method": "GET", "url": "/orders/1" } },
        ]
    }))
    .expect("collection");

    assert_eq!(collection.find_request("list").unwrap().name, "List");
    assert_eq!(collection.find_request("GET").unwrap().path, vec![2]);
    let ambiguous = collection.find_request("order").err().unwrap();
    assert!(ambiguous.contains("matches 2 requests"), "{ambiguous}");
    let missing = collection.find_request("delete").err().unwrap();
    assert!(
        missing.contains("Available: List orders, List, Get order"),
        "{missing}"
    );
}

#[test]
fn a_collection_is_found_by_slug_or_by_name() {
    let workspace = workspace();
    assert_eq!(
        workspace.collection("orders-api").unwrap().name,
        "Orders API"
    );
    assert_eq!(
        workspace.collection("Orders API").unwrap().name,
        "Orders API"
    );
    assert!(workspace.collection("orders").is_none());
}

#[test]
fn an_exact_environment_name_wins_over_a_slug_match() {
    let workspace = workspace();
    assert_eq!(
        workspace.environment("local-dev").unwrap().name,
        "local-dev"
    );
    assert_eq!(
        workspace.environment("Local Dev").unwrap().name,
        "Local Dev"
    );
    assert_eq!(
        workspace.environment("LOCAL DEV").unwrap().name,
        "Local Dev"
    );
    assert!(workspace.environment("prod").is_none());
}
