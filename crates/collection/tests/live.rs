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
