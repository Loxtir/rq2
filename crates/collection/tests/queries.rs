use collection::model::Variable;
use collection::store::{self, Edit, RequestDraft};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn workspace() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir()
        .join("opencode")
        .join(format!("rq-queries-{}-{nonce}", std::process::id()));
    let parsed = collection::postman::parse_collection(
        br#"{"info":{"name":"Queries"},"item":[
            {"name":"First","request":{"method":"GET","url":"https://api.test/first"}},
            {"name":"Second","request":{"method":"GET","url":"https://api.test/second"}}
        ]}"#,
        &[],
    )
    .unwrap();
    store::write(&root, &store::render(&parsed.collection, &[])).unwrap();
    root
}

fn draft(queries: Option<BTreeMap<String, String>>) -> RequestDraft {
    let mut draft: RequestDraft = serde_json::from_value(serde_json::json!({
        "name": "First", "method": "GET", "url": "https://api.test/first"
    }))
    .unwrap();
    draft.queries = queries;
    draft
}

#[test]
fn named_queries_round_trip_update_and_remove_without_affecting_other_requests() {
    let root = workspace();
    let file = root.join("collections/queries/01-first.yml");
    let other = root.join("collections/queries/02-second.yml");
    let untouched = std::fs::read(&other).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains("queries:"));
    let queries = BTreeMap::from([
        ("Brief".to_string(), "{ id title }".to_string()),
        (
            "Tree / résumé".to_string(),
            "{\n children** { id }\n}".to_string(),
        ),
    ]);
    let save = |draft| {
        store::save(
            &root,
            "queries",
            &Edit::Request {
                path: vec![0],
                draft,
            },
        )
    };
    save(draft(Some(queries.clone()))).unwrap();
    let loaded = store::load(&root).unwrap();
    assert_eq!(
        loaded.collections[0]
            .request_at(&[0])
            .unwrap()
            .request
            .queries,
        queries
    );
    assert!(loaded.collections[0]
        .request_at(&[1])
        .unwrap()
        .request
        .queries
        .is_empty());
    assert_eq!(std::fs::read(&other).unwrap(), untouched);

    // An older caller that omits the field must not erase saved queries.
    assert_eq!(save(draft(None)).unwrap().written, 0);
    let mut edited = queries;
    edited.insert("Brief".to_string(), "{ id }".to_string());
    save(draft(Some(edited.clone()))).unwrap();
    assert_eq!(
        store::load(&root).unwrap().collections[0]
            .request_at(&[0])
            .unwrap()
            .request
            .queries,
        edited
    );
    save(draft(Some(BTreeMap::new()))).unwrap();
    assert!(store::load(&root).unwrap().collections[0]
        .request_at(&[0])
        .unwrap()
        .request
        .queries
        .is_empty());
    assert!(!std::fs::read_to_string(&file).unwrap().contains("queries:"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queries_use_the_existing_secret_leak_guard() {
    let root = workspace();
    // The collection renderer scans its own secret values.
    store::save(
        &root,
        "queries",
        &Edit::Variables {
            path: vec![0],
            variables: vec![Variable {
                name: "token".to_string(),
                value: Some("query-secret-value".to_string()),
                secret: true,
            }],
        },
    )
    .unwrap();
    let file = root.join("collections/queries/01-first.yml");
    let before = std::fs::read(&file).unwrap();
    let result = store::save(
        &root,
        "queries",
        &Edit::Request {
            path: vec![0],
            draft: draft(Some(BTreeMap::from([(
                "Brief".to_string(),
                "{ query-secret-value }".to_string(),
            )]))),
        },
    );
    assert!(result.unwrap_err().contains("refusing to save"));
    assert_eq!(std::fs::read(file).unwrap(), before);
    std::fs::remove_dir_all(root).unwrap();
}
