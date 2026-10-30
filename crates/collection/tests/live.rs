//! Per-user current values sit on top of the files and never change them.

use collection::live::{Entry, LiveValues, Scope};
use collection::model::Variable;
use collection::store;
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rq-live-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

#[test]
fn a_current_value_overlays_the_saved_one_without_touching_the_files() {
    let dir = temp_dir("overlay");
    let root = dir.join("workspace");
    let collection =
        serde_norway::from_str("name: Demo\nvariables:\n - {name: id, value: saved}\n").unwrap();
    store::write(&root, &store::render(&collection, &[])).unwrap();
    let values = LiveValues::new(dir.join("config").join("variables.json"));

    let edit = Entry {
        scope: Scope::Collection {
            collection: "demo".into(),
        },
        variable: Variable {
            name: "id".into(),
            value: Some("local".into()),
            secret: false,
        },
    };
    values.write(&root, vec![edit.clone()], false).unwrap();

    let live = values.load(&root).unwrap();
    assert_eq!(
        live.collections[0].variables[0].value.as_deref(),
        Some("local")
    );
    let saved = store::load(&root).unwrap();
    assert_eq!(
        saved.collections[0].variables[0].value.as_deref(),
        Some("saved")
    );
    assert!(values.entries(&dir.join("other")).unwrap().is_empty());

    values.write(&root, vec![edit], true).unwrap();
    assert!(values.entries(&root).unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

fn variable(name: &str, value: &str) -> Variable {
    Variable {
        name: name.into(),
        value: Some(value.into()),
        secret: false,
    }
}

/// Requests `a` and `b` in collection Demo, with ids.
fn two_requests(dir: &std::path::Path) -> PathBuf {
    let root = dir.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    store::create_collection(&root, "Demo").unwrap();
    for name in ["a", "b"] {
        store::add_item(
            &root,
            "demo",
            &[],
            name,
            Some(collection::model::RequestDef {
                method: "GET".into(),
                url: format!("https://{name}"),
                variables: vec![variable("v", "saved")],
                ..Default::default()
            }),
        )
        .unwrap();
    }
    root
}

fn value_of(values: &LiveValues, root: &std::path::Path, request: &str) -> Option<String> {
    let workspace = values.load(root).unwrap();
    let item = workspace.collections[0]
        .items
        .iter()
        .find(|item| item.name == request)
        .unwrap();
    item.request.as_ref().unwrap().variables[0].value.clone()
}

#[test]
fn a_current_value_follows_its_request_through_moves_and_renames() {
    let dir = temp_dir("follow");
    let root = two_requests(&dir);
    let values = LiveValues::new(dir.join("config").join("variables.json"));
    values
        .write(
            &root,
            vec![Entry {
                scope: Scope::Item {
                    collection: "demo".into(),
                    path: vec![0],
                },
                variable: variable("v", "local-a"),
            }],
            false,
        )
        .unwrap();

    // `a` moves below `b`, then everything is renamed.
    store::move_item(&root, "demo", &[0], &[], 1).unwrap();
    store::rename_item(&root, "demo", &[1], "a2").unwrap();
    let slug = store::rename_collection(&root, "demo", "Renamed").unwrap();
    assert_eq!(slug, "renamed");
    assert_eq!(value_of(&values, &root, "a2").as_deref(), Some("local-a"));
    assert_eq!(value_of(&values, &root, "b").as_deref(), Some("saved"));
    let text = std::fs::read_to_string(dir.join("config/variables.json")).unwrap();
    assert!(!text.contains("\"path\""), "{text}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn position_based_entries_from_before_ids_are_read_and_rewritten_by_id() {
    let dir = temp_dir("legacy");
    let root = two_requests(&dir);
    let config = dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    let key = root.canonicalize().unwrap().to_string_lossy().into_owned();
    let key = if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    };
    let legacy = serde_json::json!({"workspaces": {key: [
        {"scope": {"kind": "item", "collection": "demo", "path": [1]}, "variable": {"name": "v", "value": "local-b"}},
        {"scope": {"kind": "collection", "collection": "gone"}, "variable": {"name": "x", "value": "kept"}},
    ]}});
    std::fs::write(config.join("variables.json"), legacy.to_string()).unwrap();
    let values = LiveValues::new(config.join("variables.json"));
    assert_eq!(value_of(&values, &root, "b").as_deref(), Some("local-b"));

    // Any write migrates: `b` is then found by id even after moving.
    values.write(&root, Vec::new(), false).unwrap();
    store::move_item(&root, "demo", &[1], &[], 0).unwrap();
    assert_eq!(value_of(&values, &root, "b").as_deref(), Some("local-b"));
    let text = std::fs::read_to_string(config.join("variables.json")).unwrap();
    assert!(
        text.contains("\"gone\""),
        "an entry for a missing collection is kept: {text}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
