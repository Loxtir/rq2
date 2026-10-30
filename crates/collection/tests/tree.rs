//! Rename, delete, duplicate and move on disk.

use collection::model::{RequestDef, Variable};
use collection::store;
use std::path::{Path, PathBuf};

fn temp_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("rq-tree-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn request(url: &str) -> Option<RequestDef> {
    Some(RequestDef {
        method: "GET".into(),
        url: url.into(),
        ..Default::default()
    })
}

/// `a`, `b`, then folder `F` holding `c`.
fn sample(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    store::create_collection(&root, "Demo").unwrap();
    store::add_item(&root, "demo", &[], "a", request("https://a")).unwrap();
    store::add_item(&root, "demo", &[], "b", request("https://b")).unwrap();
    let folder = store::add_item(&root, "demo", &[], "F", None).unwrap();
    store::add_item(&root, "demo", &folder, "c", request("https://c")).unwrap();
    root
}

fn names(root: &Path) -> Vec<String> {
    fn walk(items: &[collection::model::Item], prefix: &str, out: &mut Vec<String>) {
        for item in items {
            out.push(format!("{prefix}{}", item.name));
            walk(&item.items, &format!("{prefix}{}/", item.name), out);
        }
    }
    let workspace = store::load(root).unwrap();
    let mut out = Vec::new();
    walk(&workspace.collection("demo").unwrap().items, "", &mut out);
    out
}

fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    out
}

#[test]
fn move_reorders_and_nests_by_renaming_files() {
    let root = sample("move");
    // Put `a` after `b`: once `a` is out, `b` is at 0, so index 1.
    assert_eq!(store::move_item(&root, "demo", &[0], &[], 1).unwrap(), [1]);
    assert_eq!(names(&root), ["b", "a", "F", "F/c"]);

    // `b` into the folder, after `c`; the folder is at 1 once `b` is out.
    assert_eq!(
        store::move_item(&root, "demo", &[0], &[1], 9).unwrap(),
        [1, 1]
    );
    assert_eq!(names(&root), ["a", "F", "F/c", "F/b"]);

    assert!(
        store::move_item(&root, "demo", &[1], &[1], 0).is_err(),
        "not into itself"
    );
    let all = files(&root.join("collections"));
    assert_eq!(
        all.iter().filter(|file| file.ends_with("-b.yml")).count(),
        1,
        "moved, not copied: {all:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rename_duplicate_and_delete_keep_the_tree_and_files_in_step() {
    let root = sample("edit");
    store::rename_item(&root, "demo", &[2], "Folder").unwrap();
    assert_eq!(store::duplicate_item(&root, "demo", &[2]).unwrap(), [3]);
    assert_eq!(store::duplicate_item(&root, "demo", &[2]).unwrap(), [3]);
    assert_eq!(
        names(&root),
        [
            "a",
            "b",
            "Folder",
            "Folder/c",
            "Folder copy 2",
            "Folder copy 2/c",
            "Folder copy",
            "Folder copy/c"
        ]
    );

    store::delete_item(&root, "demo", &[2]).unwrap();
    store::delete_item(&root, "demo", &[0]).unwrap();
    assert_eq!(
        names(&root),
        [
            "b",
            "Folder copy 2",
            "Folder copy 2/c",
            "Folder copy",
            "Folder copy/c"
        ]
    );
    let all = files(&root.join("collections"));
    assert!(
        !all.iter().any(|file| file.contains("-folder/")),
        "the folder's directory is gone: {all:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_duplicated_secret_keeps_its_value_and_a_deleted_collection_takes_its_secrets() {
    let root = sample("secret");
    let mut secret = request("https://s").unwrap();
    secret.variables.push(Variable {
        name: "token".into(),
        value: Some("s3cret".into()),
        secret: true,
    });
    let path = store::add_item(&root, "demo", &[], "s", Some(secret)).unwrap();
    let copy = store::duplicate_item(&root, "demo", &path).unwrap();
    let workspace = store::load(&root).unwrap();
    let copied = workspace
        .collection("demo")
        .unwrap()
        .request_at(&copy)
        .unwrap();
    assert_eq!(copied.request.variables[0].value.as_deref(), Some("s3cret"));

    assert_eq!(
        store::rename_collection(&root, "demo", "Renamed").unwrap(),
        "renamed"
    );
    assert!(root.join(".secrets/renamed.env").is_file());
    assert!(!root.join(".secrets/demo.env").exists());

    store::delete_collection(&root, "renamed").unwrap();
    assert!(!root.join("collections/renamed").exists());
    assert!(!root.join(".secrets/renamed.env").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_scripts_save_rename_with_their_includes_and_delete() {
    let root = sample("shared");
    let script = |text: &str| collection::model::Scripts {
        pre_request: text.into(),
        ..Default::default()
    };
    assert_eq!(
        store::save_shared_script(
            &root,
            None,
            "Milestones",
            script("pm.variables.set('m', '1');")
        )
        .unwrap(),
        "milestones"
    );
    assert!(
        store::save_shared_script(&root, None, "milestones", script("")).is_err(),
        "taken"
    );

    let meta = root.join("collections/demo/collection.yml");
    let text = std::fs::read_to_string(&meta).unwrap();
    std::fs::write(&meta, format!("{text}scripts:\n  include: [milestones]\n")).unwrap();

    store::save_shared_script(&root, Some("milestones"), "Sprints", script("// v2")).unwrap();
    let workspace = store::load(&root).unwrap();
    assert_eq!(workspace.scripts.keys().collect::<Vec<_>>(), ["sprints"]);
    assert_eq!(workspace.scripts["sprints"].pre_request, "// v2");
    assert_eq!(
        workspace.collection("demo").unwrap().scripts.include,
        ["sprints"]
    );
    assert!(!root.join("scripts/milestones.yml").exists());

    store::delete_shared_script(&root, "sprints").unwrap();
    assert!(store::load(&root).unwrap().scripts.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
