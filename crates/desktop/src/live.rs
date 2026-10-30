//! The desktop's handle on per-user current values; the logic lives in
//! `collection::live`. The config path is set once at startup.
use collection::live::LiveValues;
pub(crate) use collection::live::{target, variables_mut, Entry, Scope};
use collection::store;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static CONFIG: OnceLock<LiveValues> = OnceLock::new();

pub(crate) fn initialize(path: PathBuf) {
    let _ = CONFIG.set(LiveValues::new(path));
}

pub(crate) fn entries(root: &Path) -> Result<Vec<Entry>, String> {
    match CONFIG.get() {
        Some(values) => values.entries(root),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn write(root: &Path, edits: Vec<Entry>, reset: bool) -> Result<(), String> {
    CONFIG
        .get()
        .ok_or("local variable config is unavailable")?
        .write(root, edits, reset)
}

pub(crate) fn load(root: &Path) -> Result<store::Workspace, String> {
    let mut workspace = store::load(root)?;
    collection::live::apply(&mut workspace, &entries(root)?);
    Ok(workspace)
}
#[cfg(test)]
mod tests {
    use super::*;
    use collection::model::Variable;
    use std::collections::BTreeMap;

    fn fixture() -> PathBuf {
        static TEST_CONFIG: OnceLock<PathBuf> = OnceLock::new();
        let config = TEST_CONFIG.get_or_init(|| {
            let directory = std::env::temp_dir().join("opencode").join(format!(
                "rq-live-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            initialize(directory.join("variables.json"));
            directory
        });
        let root = config.join(format!(
            "workspace-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let collection = serde_norway::from_str("name: Demo\nvariables:\n - {name: id, value: saved}\n - {name: composed, value: '{{origin}}/{{id}}'}\n - {name: token, value: old-secret, secret: true}\nitems:\n - name: Folder\n   variables:\n    - {name: id, value: folder}\n   items:\n    - name: Get\n      request:\n       method: GET\n       url: '{{composed}}'\n       headers:\n        - {name: Authorization, value: '{{token}}'}\n - name: Other\n   request:\n    method: GET\n    url: '{{composed}}'\n").unwrap();
        let dev = serde_norway::from_str(
            "name: dev\nvariables:\n - {name: origin, value: 'https://dev.test'}\n",
        )
        .unwrap();
        let stage = serde_norway::from_str(
            "name: stage\nvariables:\n - {name: origin, value: 'https://stage.test'}\n",
        )
        .unwrap();
        store::write(&root, &store::render(&collection, &[dev, stage])).unwrap();
        root
    }

    fn entry(scope: Scope, name: &str, value: &str, secret: bool) -> Entry {
        Entry {
            scope,
            variable: Variable {
                name: name.into(),
                value: Some(value.into()),
                secret,
            },
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for item in std::fs::read_dir(root).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
        files
    }

    #[test]
    fn live_values_persist_outside_workspace_and_resolve_in_query_and_curl() {
        let root = fixture();
        let before = snapshot(&root);
        write(
            &root,
            vec![
                entry(
                    Scope::Item {
                        collection: "demo".into(),
                        path: vec![0],
                    },
                    "id",
                    "folder-live",
                    false,
                ),
                entry(
                    Scope::Environment {
                        environment: "dev".into(),
                    },
                    "origin",
                    "https://local.test",
                    false,
                ),
                entry(
                    Scope::Collection {
                        collection: "demo".into(),
                    },
                    "token",
                    "local-secret",
                    true,
                ),
            ],
            false,
        )
        .unwrap();
        // A fresh file read, not an in-memory cache, reconstructs the values.
        assert_eq!(entries(&root).unwrap().len(), 3);
        let context = crate::query::Context {
            collection: "demo".into(),
            path: vec![0, 0],
            environment: Some("dev".into()),
            deployment: None,
        };
        let vars = context.variables(&root).unwrap();
        assert_eq!(vars["composed"], "https://local.test/folder-live");
        assert_eq!(vars["token"], "local-secret");
        let selection =
            crate::query::parse(r#"{ id == "{{id}}" }"#, || context.variables(&root)).unwrap();
        assert_eq!(
            response_query::project_to_string(br#"{"id":"folder-live"}"#, &selection).unwrap(),
            r#"{"id":"folder-live"}"#
        );
        let preview =
            crate::curl_for(&root, "demo", &[0, 0], None, Some("dev"), None, false).unwrap();
        let preview = serde_json::to_string(&preview).unwrap();
        assert!(preview.contains("https://local.test/folder-live"));
        assert!(!preview.contains("local-secret"));
        let dto = serde_json::to_string(&crate::workspace_dto(&root).unwrap()).unwrap();
        assert!(dto.contains("folder-live"));
        assert!(!dto.contains("local-secret"));
        assert!(!dto.contains("old-secret"));
        assert_eq!(snapshot(&root), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scopes_workspaces_and_reset_are_isolated_and_defaults_stay_separate() {
        let root = fixture();
        let other = fixture();
        let scope = Scope::Collection {
            collection: "demo".into(),
        };
        let edit = entry(scope.clone(), "id", "local", false);
        write(&root, vec![edit.clone()], false).unwrap();
        let located = crate::locate(&root, "demo", Some("dev")).unwrap();
        let vars = collection::send::variables(
            &located.collection,
            located.environment.as_ref(),
            &located.globals,
            &[1],
        );
        assert_eq!(vars["id"], "local");
        let vars = collection::send::variables(
            &located.collection,
            located.environment.as_ref(),
            &located.globals,
            &[0, 0],
        );
        assert_eq!(vars["id"], "folder"); // a wider live value cannot shadow a narrower default
        assert!(entries(&other).unwrap().is_empty());
        let mut defaults = store::load(&root).unwrap();
        assert_eq!(
            defaults.collections[0].variables[0].value.as_deref(),
            Some("saved")
        );
        assert_eq!(
            target(&mut defaults, "demo", &[0, 0], Some("dev"), "id", None).unwrap(),
            Scope::Item {
                collection: "demo".into(),
                path: vec![0]
            }
        );
        // Saving an unrelated collection edit must never pick up live values.
        let saved = &defaults.collections[0];
        store::save(
            &root,
            "demo",
            &store::Edit::Collection {
                name: saved.name.clone(),
                description: Some("edited".into()),
                variables: saved.variables.clone(),
                auth: saved.auth.clone(),
                scripts: saved.scripts.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            store::load(&root).unwrap().collections[0].variables[0]
                .value
                .as_deref(),
            Some("saved")
        );
        assert_eq!(
            entries(&root).unwrap()[0].variable.value.as_deref(),
            Some("local")
        );
        write(&root, vec![edit], true).unwrap();
        assert!(entries(&root).unwrap().is_empty());
        assert_eq!(
            load(&root).unwrap().collections[0].variables[0]
                .value
                .as_deref(),
            Some("saved")
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(other).unwrap();
    }

    #[test]
    fn script_writes_are_local_and_preserve_secret_flags() {
        let root = fixture();
        let before = snapshot(&root);
        crate::persist_script_writes(
            &root,
            "demo",
            vec![
                (
                    Scope::Collection {
                        collection: "demo".into(),
                    },
                    BTreeMap::from([("token".into(), "refreshed-token".into())]),
                ),
                (
                    Scope::Globals,
                    BTreeMap::from([("globalId".into(), "42".into())]),
                ),
                (
                    Scope::Environment {
                        environment: "dev".into(),
                    },
                    BTreeMap::from([("origin".into(), "https://script.test".into())]),
                ),
                (
                    Scope::Item {
                        collection: "demo".into(),
                        path: vec![0, 0],
                    },
                    BTreeMap::from([("id".into(), "request-live".into())]),
                ),
            ],
        )
        .unwrap();
        let context = crate::query::Context {
            collection: "demo".into(),
            path: vec![0, 0],
            environment: Some("dev".into()),
            deployment: None,
        };
        let vars = context.variables(&root).unwrap();
        assert_eq!(vars["composed"], "https://script.test/request-live");
        assert_eq!(vars["globalId"], "42");
        assert_eq!(vars["token"], "refreshed-token");
        assert!(
            entries(&root)
                .unwrap()
                .iter()
                .find(|entry| entry.variable.name == "token")
                .unwrap()
                .variable
                .secret
        );
        assert_eq!(snapshot(&root), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_only_values_empty_strings_and_malformed_config_are_handled() {
        let root = fixture();
        let edit = entry(Scope::Globals, "new", "", false);
        write(&root, vec![edit.clone()], false).unwrap();
        assert_eq!(load(&root).unwrap().globals[0].value.as_deref(), Some(""));
        write(&root, vec![edit], true).unwrap();
        assert!(load(&root).unwrap().globals.is_empty());
        let broken = root.parent().unwrap().join(format!(
            "broken-{}.json",
            root.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&broken, b"invalid").unwrap();
        assert!(LiveValues::new(&broken)
            .write(&root, vec![], false)
            .is_err());
        assert_eq!(std::fs::read(&broken).unwrap(), b"invalid");
        std::fs::remove_file(broken).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
