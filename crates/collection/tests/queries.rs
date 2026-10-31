use collection::model::Variable;
use collection::store::{self, Edit};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

fn queries(root: &Path, index: usize) -> BTreeMap<String, String> {
    store::load(root).unwrap().collections[0]
        .request_at(&[index])
        .unwrap()
        .request
        .queries
        .clone()
}

fn first_id(root: &Path) -> String {
    store::load(root).unwrap().collections[0].items[0]
        .id
        .clone()
}

#[test]
fn each_query_is_its_own_file_and_never_touches_request_files() {
    let root = workspace();
    let first = root.join("collections/queries/01-first.yml");
    let second = root.join("collections/queries/02-second.yml");
    let (first_before, second_before) = (
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
    );
    let id = first_id(&root);

    store::save_query(&root, "queries", &[0], "Brief", Some("{ id title }")).unwrap();
    store::save_query(
        &root,
        "queries",
        &[0],
        "Tree / résumé",
        Some("{\n children** { id }\n}"),
    )
    .unwrap();
    assert_eq!(std::fs::read(&first).unwrap(), first_before);
    assert_eq!(std::fs::read(&second).unwrap(), second_before);
    let dir = root.join("collections/queries/queries").join(&id);
    assert_eq!(
        std::fs::read_to_string(dir.join("Brief.query")).unwrap(),
        "{ id title }"
    );
    assert!(dir.join("Tree %2F résumé.query").is_file());
    assert_eq!(
        queries(&root, 0),
        BTreeMap::from([
            ("Brief".to_string(), "{ id title }".to_string()),
            (
                "Tree / résumé".to_string(),
                "{\n children** { id }\n}".to_string()
            ),
        ])
    );
    assert!(queries(&root, 1).is_empty());

    store::save_query(&root, "queries", &[0], "Brief", None).unwrap();
    assert!(!dir.join("Brief.query").exists());
    assert_eq!(queries(&root, 0).len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn request_saves_renames_and_moves_keep_query_files_in_place() {
    let root = workspace();
    store::save_query(&root, "queries", &[0], "Brief", Some("{ id }")).unwrap();
    let file = root
        .join("collections/queries/queries")
        .join(first_id(&root))
        .join("Brief.query");
    let mut draft: store::RequestDraft = serde_json::from_value(serde_json::json!({
        "name": "Renamed", "method": "POST", "url": "https://api.test/first"
    }))
    .unwrap();
    draft.method = "POST".into();
    store::save(
        &root,
        "queries",
        &Edit::Request {
            path: vec![0],
            draft,
        },
    )
    .unwrap();
    store::move_item(&root, "queries", &[0], &[], 2).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ id }");
    assert_eq!(queries(&root, 1)["Brief"], "{ id }");

    store::delete_item(&root, "queries", &[1]).unwrap();
    assert!(!file.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_query_maps_move_into_files() {
    let root = workspace();
    let request = root.join("collections/queries/01-first.yml");
    let text = std::fs::read_to_string(&request).unwrap();
    std::fs::write(&request, format!("{text}queries:\n  Brief: '{{ id }}'\n")).unwrap();
    assert_eq!(queries(&root, 0)["Brief"], "{ id }");

    assert_eq!(store::migrate_queries(&root).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&request).unwrap(), text);
    assert_eq!(queries(&root, 0)["Brief"], "{ id }");
    assert_eq!(store::migrate_queries(&root).unwrap(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn queries_use_the_existing_secret_leak_guard() {
    let root = workspace();
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
    let result = store::save_query(
        &root,
        "queries",
        &[0],
        "Brief",
        Some("{ query-secret-value }"),
    );
    assert!(result.unwrap_err().contains("refusing to save"));
    assert!(!root.join("collections/queries/queries").exists());
    std::fs::remove_dir_all(root).unwrap();
}
