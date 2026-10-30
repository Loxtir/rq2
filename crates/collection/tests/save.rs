//! Saving an edit back to per-request files.
//!
//! Saving re-renders the collection and writes only the difference, so these
//! tests are mostly about what does *not* happen: a secret is not copied into a
//! committed file, a rename does not lose the values in `.secrets/`, and a file
//! the tool did not write is not deleted.

use collection::model::{
    Auth, AuthSetting, Body, Collection, Item, KeyValue, RequestDef, Variable,
};
use collection::store::{self, Edit, RequestDraft};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("rq-save-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn variable(name: &str, value: &str, secret: bool) -> Variable {
    Variable {
        name: name.to_string(),
        value: Some(value.to_string()),
        secret,
    }
}

fn bearer() -> AuthSetting {
    AuthSetting::Method(Auth {
        kind: "bearer".to_string(),
        fields: [("token".to_string(), "{{token}}".to_string())]
            .into_iter()
            .collect(),
    })
}

fn collection(variables: Vec<Variable>) -> Collection {
    Collection {
        id: String::new(),
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        scripts: Default::default(),
        name: "Acme Orders".to_string(),
        description: Some("Order endpoints".to_string()),
        auth: bearer(),
        variables,
        items: vec![
            Item {
                id: String::new(),
                name: "Orders".to_string(),
                auth: Default::default(),
                variables: Vec::new(),
                scripts: Default::default(),
                request: None,
                items: vec![Item {
                    id: String::new(),
                    name: "List orders".to_string(),
                    auth: Default::default(),
                    variables: Vec::new(),
                    scripts: Default::default(),
                    request: Some(RequestDef {
                        method: "GET".to_string(),
                        url: "{{baseUrl}}/orders".to_string(),
                        headers: vec![KeyValue {
                            name: "Accept".to_string(),
                            value: "application/json".to_string(),
                            disabled: false,
                        }],
                        body: None,
                        auth: Default::default(),
                        description: Some("Paged".to_string()),
                        ..RequestDef::default()
                    }),
                    items: Vec::new(),
                }],
            },
            Item {
                id: String::new(),
                name: "Create order".to_string(),
                auth: Default::default(),
                variables: Vec::new(),
                scripts: Default::default(),
                request: Some(RequestDef {
                    method: "POST".to_string(),
                    url: "{{baseUrl}}/orders".to_string(),
                    headers: Vec::new(),
                    body: None,
                    auth: Default::default(),
                    description: None,
                    ..RequestDef::default()
                }),
                items: Vec::new(),
            },
        ],
    }
}

fn workspace(tag: &str, collection: &Collection) -> PathBuf {
    let root = temp_dir(tag);
    let rendered = store::render(collection, &[]);
    store::write(&root, &rendered).expect("write workspace");
    root
}

fn draft(name: &str, method: &str, url: &str) -> RequestDraft {
    RequestDraft {
        name: name.to_string(),
        method: method.to_string(),
        url: url.to_string(),
        params: Vec::new(),
        path_params: BTreeMap::new(),
        headers: Vec::new(),
        body: None,
        auth: Default::default(),
        variables: None,
        scripts: None,
        queries: None,
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn edits_a_request_and_leaves_its_auth_alone() {
    let root = workspace("edit", &collection(Vec::new()));
    let file = root.join("collections/acme-orders/01-orders/01-list-orders.yml");

    let mut edit = draft("List orders", "get", "{{baseUrl}}/v2/orders?limit=5");
    edit.headers = vec![KeyValue {
        name: "X-Trace".to_string(),
        value: "on".to_string(),
        disabled: false,
    }];
    edit.body = Some(Body {
        mode: "raw".to_string(),
        language: Some("json".to_string()),
        text: Some("{\"a\":1}".to_string()),
    });

    store::save(
        &root,
        "acme-orders",
        &Edit::Request {
            path: vec![0, 0],
            draft: edit,
        },
    )
    .expect("save");

    let written = read(&file);
    // A query typed into the URL box is normalised into the parameter list, so a
    // later toggle can switch one off without editing text.
    assert!(
        written.contains("url: '{{baseUrl}}/v2/orders'"),
        "{written}"
    );
    assert!(written.contains("name: limit"), "{written}");
    assert!(written.contains("value: '5'"), "{written}");
    assert!(written.contains("method: GET"), "{written}");
    assert!(written.contains("X-Trace"), "{written}");
    assert!(written.contains("{\"a\":1}"), "{written}");
    // The request keeps inheriting the collection's bearer auth.
    assert!(!written.contains("auth:"), "{written}");
}

#[test]
fn renaming_the_request_renames_the_file() {
    let root = workspace("rename", &collection(Vec::new()));
    let old = root.join("collections/acme-orders/01-orders/01-list-orders.yml");

    store::save(
        &root,
        "acme-orders",
        &Edit::Request {
            path: vec![0, 0],
            draft: draft("List open orders", "GET", "{{baseUrl}}/orders"),
        },
    )
    .expect("save");

    assert!(!old.exists(), "the old file should be gone");
    let new = root.join("collections/acme-orders/01-orders/01-list-open-orders.yml");
    assert!(read(&new).contains("name: List open orders"));
}

#[test]
fn a_secret_keeps_its_value_when_the_editor_omits_it() {
    let root = workspace(
        "secret",
        &collection(vec![
            variable("baseUrl", "https://acme.test", false),
            variable("token", "hunter2", true),
        ]),
    );

    // What the window sends: the secret's value never left Rust.
    let mut edited = vec![
        variable("baseUrl", "https://acme.test/v2", false),
        Variable {
            name: "token".to_string(),
            value: None,
            secret: true,
        },
    ];

    store::save(
        &root,
        "acme-orders",
        &Edit::Collection {
            scripts: Default::default(),
            name: "Acme Orders".to_string(),
            description: Some("Order endpoints".to_string()),
            variables: edited.clone(),
            auth: bearer(),
        },
    )
    .expect("save");

    let committed = read(&root.join("collections/acme-orders/collection.yml"));
    assert!(committed.contains("https://acme.test/v2"), "{committed}");
    assert!(committed.contains("secret: true"), "{committed}");
    assert!(!committed.contains("hunter2"), "{committed}");

    let secrets = read(&root.join(".secrets/acme-orders.env"));
    assert!(secrets.contains("hunter2"), "{secrets}");

    // And the value is still attached when the workspace is loaded again.
    let loaded = store::load(&root).expect("load");
    let token = loaded.collections[0]
        .variables
        .iter()
        .find(|variable| variable.name == "token")
        .expect("token");
    assert_eq!(token.value.as_deref(), Some("hunter2"));

    edited.clear();
}

#[test]
fn renaming_the_collection_moves_its_secrets_with_it() {
    let root = workspace(
        "collection-rename",
        &collection(vec![variable("token", "hunter2", true)]),
    );

    let outcome = store::save(
        &root,
        "acme-orders",
        &Edit::Collection {
            scripts: Default::default(),
            name: "Acme Commerce".to_string(),
            description: None,
            variables: vec![Variable {
                name: "token".to_string(),
                value: None,
                secret: true,
            }],
            auth: bearer(),
        },
    )
    .expect("save");

    assert_eq!(outcome.collection, "acme-commerce");
    assert!(!root.join("collections/acme-orders").exists());
    assert!(root
        .join("collections/acme-commerce/collection.yml")
        .exists());
    assert!(!root.join(".secrets/acme-orders.env").exists());

    let secrets = read(&root.join(".secrets/acme-commerce.env"));
    assert!(secrets.contains("hunter2"), "{secrets}");
}

#[test]
fn leaves_files_it_did_not_write() {
    let root = workspace("foreign", &collection(Vec::new()));
    let notes = root.join("collections/acme-orders/notes.md");
    std::fs::write(&notes, "hand-written").expect("notes");

    store::save(
        &root,
        "acme-orders",
        &Edit::Folder {
            scripts: Default::default(),
            path: vec![0],
            name: "Orders v2".to_string(),
            auth: AuthSetting::Inherit,
        },
    )
    .expect("save");

    assert_eq!(read(&notes), "hand-written");
    assert!(root
        .join("collections/acme-orders/01-orders-v2/01-list-orders.yml")
        .exists());
    assert!(!root.join("collections/acme-orders/01-orders").exists());
}

/// The window sends these exact shapes. Deserialising them here pins the contract
/// between `app.js` and `Edit`, which no click-through test can catch.
#[test]
fn deserialises_the_window_payloads() {
    let request: Edit = serde_json::from_str(
        r#"{
            "kind": "request",
            "path": [0, 0],
            "draft": {
                "name": "List orders",
                "method": "get",
                "url": "{{baseUrl}}/orders/:orderId",
                "params": [
                    { "name": "limit", "value": "5", "disabled": false },
                    { "name": "debug", "value": "1", "disabled": true }
                ],
                "path_params": { "orderId": "42" },
                "headers": [{ "name": "Accept", "value": "application/json", "disabled": false }],
                "body": { "mode": "raw", "language": "json", "text": "{\"a\":1}" },
                "auth": { "kind": "bearer", "token": "{{token}}" }
            }
        }"#,
    )
    .expect("request edit");

    let collection_edit: Edit = serde_json::from_str(
        r#"{
            "kind": "collection",
            "name": "Acme",
            "description": null,
            "variables": [
                { "name": "baseUrl", "value": "https://acme.test", "secret": false },
                { "name": "token", "value": null, "secret": true }
            ],
            "auth": "inherit"
        }"#,
    )
    .expect("collection edit");

    let folder: Edit = serde_json::from_str(
        r#"{
            "kind": "folder",
            "path": [0],
            "name": "Orders v2",
            "auth": { "kind": "basic", "username": "acme", "password": "{{password}}" }
        }"#,
    )
    .expect("folder edit");

    // Applying them is the half a shape check cannot prove.
    let root = workspace("contract", &collection(Vec::new()));
    store::save(&root, "acme-orders", &request).expect("save request");
    store::save(&root, "acme-orders", &collection_edit).expect("save collection");
    store::save(&root, "acme", &folder).expect("save folder");

    let written = read(&root.join("collections/acme/01-orders-v2/01-list-orders.yml"));
    assert!(written.contains("name: List orders"), "{written}");
    assert!(written.contains("\"a\":1"), "{written}");
    assert!(written.contains("orderId: '42'"), "{written}");
    assert!(written.contains("disabled: true"), "{written}");
    assert!(written.contains("kind: bearer"), "{written}");

    let folder_file = read(&root.join("collections/acme/01-orders-v2/folder.yml"));
    assert!(folder_file.contains("kind: basic"), "{folder_file}");
    assert!(folder_file.contains("username: acme"), "{folder_file}");

    // The collection edit really cleared its auth rather than ignoring it.
    assert!(!read(&root.join("collections/acme/collection.yml")).contains("auth:"));
}

#[test]
fn an_environment_variable_can_be_saved_on_its_own() {
    let root = temp_dir("environment");
    let mut collection = collection(Vec::new());
    collection.variables.clear();
    store::write(&root, &store::render(&collection, &[])).expect("write");

    // Environment files are not the collection's; write one through the same
    // machinery by rendering it with the collection, then edit just its value.
    let environment = collection::model::Environment {
        main_slot: None,
        platforms: Vec::new(),
        name: "dev".to_string(),
        insecure: false,
        variables: vec![variable("baseUrl", "https://dev.acme.test", false)],
    };
    let rendered = store::render(&collection, std::slice::from_ref(&environment));
    store::write(&root, &rendered).expect("write environment");

    store::save_environment(
        &root,
        "dev",
        vec![variable("baseUrl", "https://dev.acme.test/v2", false)],
    )
    .expect("save environment");

    assert!(read(&root.join("environments/dev.yml")).contains("https://dev.acme.test/v2"));
}

#[test]
fn environments_can_be_created_renamed_and_deleted_with_secrets() {
    let root = workspace("environment-lifecycle", &collection(Vec::new()));

    store::create_environment(
        &root,
        "QA cluster".to_string(),
        true,
        vec![variable("token", "qa-secret", true)],
    )
    .expect("create environment");
    assert!(root.join("environments/qa-cluster.yml").exists());
    assert!(read(&root.join(".secrets/qa-cluster.env")).contains("qa-secret"));

    store::update_environment(
        &root,
        "qa-cluster",
        "QA shared".to_string(),
        false,
        vec![Variable {
            name: "token".to_string(),
            value: None,
            secret: true,
        }],
    )
    .expect("rename environment");
    assert!(!root.join("environments/qa-cluster.yml").exists());
    assert!(root.join("environments/qa-shared.yml").exists());
    assert!(!root.join(".secrets/qa-cluster.env").exists());
    assert!(read(&root.join(".secrets/qa-shared.env")).contains("qa-secret"));
    assert!(!read(&root.join("environments/qa-shared.yml")).contains("insecure: true"));

    store::delete_environment(&root, "qa-shared").expect("delete environment");
    assert!(!root.join("environments/qa-shared.yml").exists());
    assert!(!root.join(".secrets/qa-shared.env").exists());
}

#[test]
fn refuses_an_empty_name_without_touching_the_disk() {
    let root = workspace("empty-name", &collection(Vec::new()));
    let before = read(&root.join("collections/acme-orders/collection.yml"));

    let error = store::save(
        &root,
        "acme-orders",
        &Edit::Collection {
            scripts: Default::default(),
            name: "   ".to_string(),
            description: None,
            variables: Vec::new(),
            auth: bearer(),
        },
    )
    .expect_err("an empty name is not a name");

    assert!(error.contains("name"), "{error}");
    assert_eq!(
        before,
        read(&root.join("collections/acme-orders/collection.yml"))
    );
}

#[test]
fn folder_and_request_variables_round_trip_and_keep_secrets_out() {
    let mut source = collection(vec![variable("baseUrl", "https://acme.test", false)]);
    source.items[0].variables = vec![variable("limit", "5", false)];
    source.items[0].items[0]
        .request
        .as_mut()
        .expect("request")
        .variables = vec![
        variable("postId", "1", false),
        variable("token", "hunter2", true),
    ];

    let root = workspace("scoped", &source);

    let folder = read(&root.join("collections/acme-orders/01-orders/folder.yml"));
    assert!(folder.contains("name: limit"), "{folder}");

    let file = root.join("collections/acme-orders/01-orders/01-list-orders.yml");
    let request = read(&file);
    assert!(request.contains("name: postId"), "{request}");
    assert!(request.contains("secret: true"), "{request}");
    assert!(!request.contains("hunter2"), "{request}");

    // A request secret lives in the collection's `.env` like any other.
    let secrets = read(&root.join(".secrets/acme-orders.env"));
    assert!(secrets.contains("hunter2"), "{secrets}");

    let loaded = store::load(&root).expect("load");
    let loaded_folder = &loaded.collections[0].items[0];
    assert_eq!(loaded_folder.variables[0].name, "limit");
    let loaded_request = loaded_folder.items[0].request.as_ref().expect("request");
    assert_eq!(loaded_request.variables[0].name, "postId");
    let token = loaded_request
        .variables
        .iter()
        .find(|variable| variable.name == "token")
        .expect("token");
    assert_eq!(token.value.as_deref(), Some("hunter2"));
}

#[test]
fn editing_a_request_variable_keeps_the_rest() {
    let mut source = collection(Vec::new());
    source.items[0].items[0]
        .request
        .as_mut()
        .expect("request")
        .variables = vec![
        variable("postId", "1", false),
        variable("token", "hunter2", true),
    ];

    let root = workspace("scope-edit", &source);

    store::save(
        &root,
        "acme-orders",
        &Edit::Variables {
            path: vec![0, 0],
            // The window sends the secret with no value; it must survive.
            variables: vec![
                variable("postId", "2", false),
                Variable {
                    name: "token".to_string(),
                    value: None,
                    secret: true,
                },
            ],
        },
    )
    .expect("save");

    let written = read(&root.join("collections/acme-orders/01-orders/01-list-orders.yml"));
    assert!(written.contains("value: '2'"), "{written}");
    assert!(written.contains("secret: true"), "{written}");
    assert!(!written.contains("hunter2"), "{written}");
    assert!(read(&root.join(".secrets/acme-orders.env")).contains("hunter2"));

    let loaded = store::load(&root).expect("load");
    let token = loaded.collections[0].items[0].items[0]
        .request
        .as_ref()
        .expect("request")
        .variables
        .iter()
        .find(|variable| variable.name == "token")
        .expect("token");
    assert_eq!(token.value.as_deref(), Some("hunter2"));
}

#[test]
fn globals_round_trip_and_keep_a_secret_out_of_the_commit() {
    let root = workspace("globals", &collection(Vec::new()));

    store::save_globals(
        &root,
        vec![
            variable("apiOrigin", "https://acme.test", false),
            variable("globalToken", "topsecret", true),
        ],
    )
    .expect("save globals");

    let committed = read(&root.join("globals.yml"));
    assert!(committed.contains("apiOrigin"), "{committed}");
    assert!(committed.contains("secret: true"), "{committed}");
    assert!(!committed.contains("topsecret"), "{committed}");

    let loaded = store::load(&root).expect("load");
    assert_eq!(loaded.globals.len(), 2);
    let token = loaded
        .globals
        .iter()
        .find(|variable| variable.name == "globalToken")
        .expect("token");
    assert_eq!(token.value.as_deref(), Some("topsecret"));

    // Editing one value and leaving the secret blank keeps its stored value.
    store::save_globals(
        &root,
        vec![
            variable("apiOrigin", "https://acme.test/v2", false),
            Variable {
                name: "globalToken".to_string(),
                value: None,
                secret: true,
            },
        ],
    )
    .expect("save globals again");

    assert!(read(&root.join("globals.yml")).contains("acme.test/v2"));
    assert!(read(&root.join(".secrets/globals.env")).contains("topsecret"));
}
