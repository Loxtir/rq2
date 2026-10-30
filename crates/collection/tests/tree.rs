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

fn ids(root: &Path) -> Vec<(String, String)> {
    fn walk(items: &[collection::model::Item], out: &mut Vec<(String, String)>) {
        for item in items {
            out.push((item.name.clone(), item.id.clone()));
            walk(&item.items, out);
        }
    }
    let mut out = Vec::new();
    walk(&store::load(root).unwrap().collections[0].items, &mut out);
    out
}

#[test]
fn ids_are_written_kept_through_edits_and_fresh_for_copies() {
    let root = sample("ids");
    let before = ids(&root);
    assert!(before.iter().all(|(_, id)| id.len() == 8), "{before:?}");
    let collection_id = store::load(&root).unwrap().collections[0].id.clone();
    assert!(!collection_id.is_empty());

    // Rename, move and reorder: same ids.
    store::rename_item(&root, "demo", &[0], "a renamed").unwrap();
    store::move_item(&root, "demo", &[2], &[], 0).unwrap();
    store::rename_collection(&root, "demo", "Demo Two").unwrap();
    let workspace = store::load(&root).unwrap();
    assert_eq!(workspace.collections[0].id, collection_id);
    let id_of = |name: &str| before.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let collection = workspace.collection("demo-two").unwrap();
    assert_eq!(collection.path_of(&id_of("b")), Some(vec![2]));
    assert_eq!(collection.path_of(&id_of("c")), Some(vec![0, 0]));
    assert_eq!(collection.id_at(&[1]), Some(id_of("a").as_str()));

    // A copy gets new ids all the way down.
    let copy = store::duplicate_item(&root, "demo-two", &[0]).unwrap();
    let workspace = store::load(&root).unwrap();
    let collection = workspace.collection("demo-two").unwrap();
    let all = ids(&root);
    let unique: std::collections::BTreeSet<_> = all.iter().map(|(_, id)| id).collect();
    assert_eq!(unique.len(), all.len(), "{all:?}");
    assert!(collection.id_at(&copy).is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ensure_ids_fills_missing_and_duplicate_ids_on_disk() {
    let root = sample("ensure");
    let dir = root.join("collections/demo");
    let a = std::fs::read_to_string(dir.join("01-a.yml")).unwrap();
    // A file from before ids, and a hand-made copy sharing a's id.
    let without: String = a
        .lines()
        .filter(|line| !line.starts_with("id:"))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(dir.join("01-a.yml"), &without).unwrap();
    std::fs::write(dir.join("02-b.yml"), a.replace("name: a", "name: b")).unwrap();
    std::fs::write(dir.join("09-z.yml"), a.replace("name: a", "name: z")).unwrap();

    // a lost its id; b copies a's (b keeps it, being first now); z too.
    assert_eq!(store::ensure_ids(&root).unwrap(), 2);
    let all = ids(&root);
    let unique: std::collections::BTreeSet<_> = all.iter().map(|(_, id)| id.clone()).collect();
    assert!(all.iter().all(|(_, id)| !id.is_empty()), "{all:?}");
    assert_eq!(unique.len(), all.len(), "{all:?}");
    assert_eq!(
        names(&root),
        ["a", "b", "F", "F/c", "z"],
        "no file is renamed or duplicated"
    );
    assert!(dir.join("09-z.yml").is_file());
    assert_eq!(store::ensure_ids(&root).unwrap(), 0, "stable once written");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_scripts_are_js_files_referenced_by_id() {
    let root = sample("shared");
    let id = store::save_shared_script(&root, None, "Milestones", "pm.variables.set('m', '1');\n")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("scripts/milestones.js")).unwrap(),
        format!("// @id {id}\npm.variables.set('m', '1');\n")
    );
    assert!(
        store::save_shared_script(&root, None, "milestones", "").is_err(),
        "taken"
    );

    let meta = root.join("collections/demo/collection.yml");
    let text = std::fs::read_to_string(&meta).unwrap();
    std::fs::write(
        &meta,
        format!("{text}scripts:\n  include:\n    pre_request: [{id}]\n"),
    )
    .unwrap();
    let before = std::fs::read_to_string(&meta).unwrap();

    // Renaming moves the file and leaves every reference alone.
    assert_eq!(
        store::save_shared_script(&root, Some(&id), "Sprints", "// v2\n").unwrap(),
        id
    );
    let workspace = store::load(&root).unwrap();
    assert_eq!(workspace.scripts[&id].name, "sprints");
    assert_eq!(workspace.scripts[&id].code, "// v2\n");
    assert_eq!(
        workspace
            .collection("demo")
            .unwrap()
            .scripts
            .include
            .pre_request,
        std::slice::from_ref(&id)
    );
    assert!(workspace.collection("demo").unwrap().includes(&id));
    assert_eq!(std::fs::read_to_string(&meta).unwrap(), before);
    assert!(!root.join("scripts/milestones.js").exists());

    // Hand-written files work as long as they carry an id.
    std::fs::write(
        root.join("scripts/by-hand.js"),
        "// @id hand_1\r\nconsole.log(1);\r\n",
    )
    .unwrap();
    assert_eq!(
        store::load(&root).unwrap().scripts["hand_1"].name,
        "by-hand"
    );
    std::fs::write(root.join("scripts/no-id.js"), "console.log(1);\n").unwrap();
    assert!(store::load(&root).err().unwrap().contains("@id"));
    std::fs::write(root.join("scripts/no-id.js"), "// @id hand_1\n").unwrap();
    assert!(store::load(&root).err().unwrap().contains("unique"));
    std::fs::remove_file(root.join("scripts/no-id.js")).unwrap();
    std::fs::remove_file(root.join("scripts/by-hand.js")).unwrap();

    store::delete_shared_script(&root, &id).unwrap();
    assert!(store::load(&root).unwrap().scripts.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
