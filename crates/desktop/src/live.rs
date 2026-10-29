//! Per-user current values. Saved workspace defaults are never modified here.
use collection::{model::Variable, store, Collection};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

static CONFIG: OnceLock<PathBuf> = OnceLock::new();
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Scope {
    Globals,
    Environment {
        environment: String,
    },
    Collection {
        collection: String,
    },
    Item {
        collection: String,
        path: Vec<usize>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub scope: Scope,
    pub variable: Variable,
}

#[derive(Default, Serialize, Deserialize)]
struct Values {
    #[serde(default)]
    workspaces: BTreeMap<String, Vec<Entry>>,
}

pub(crate) fn initialize(path: PathBuf) {
    let _ = CONFIG.set(path);
}

fn workspace_key(root: &Path) -> String {
    let path = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let key = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

fn read(path: &Path) -> Result<Values, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("reading local variable config: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Values::default()),
        Err(error) => Err(format!("reading local variable config: {error}")),
    }
}

pub(crate) fn entries(root: &Path) -> Result<Vec<Entry>, String> {
    let Some(path) = CONFIG.get() else {
        return Ok(Vec::new());
    };
    Ok(read(path)?
        .workspaces
        .remove(&workspace_key(root))
        .unwrap_or_default())
}

fn write_at(path: &Path, root: &Path, edits: Vec<Entry>, reset: bool) -> Result<(), String> {
    let _guard = WRITER
        .lock()
        .map_err(|_| "local variable config lock poisoned")?;
    let mut values = read(path)?;
    let entries = values.workspaces.entry(workspace_key(root)).or_default();
    for edit in edits {
        entries
            .retain(|entry| entry.scope != edit.scope || entry.variable.name != edit.variable.name);
        if !reset {
            entries.push(edit);
        }
    }
    let parent = path.parent().ok_or("local variable config has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&values).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("saving local variables: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|error| format!("saving local variables: {error}"))
}

pub(crate) fn write(root: &Path, edits: Vec<Entry>, reset: bool) -> Result<(), String> {
    write_at(
        CONFIG.get().ok_or("local variable config is unavailable")?,
        root,
        edits,
        reset,
    )
}

pub(crate) fn variables_mut<'a>(
    workspace: &'a mut store::Workspace,
    scope: &Scope,
) -> Option<&'a mut Vec<Variable>> {
    match scope {
        Scope::Globals => Some(&mut workspace.globals),
        Scope::Environment { environment } => Some(
            &mut workspace
                .environments
                .iter_mut()
                .find(|item| item.name == *environment)?
                .variables,
        ),
        Scope::Collection { collection } => Some(
            &mut workspace
                .collections
                .iter_mut()
                .find(|item| store::slugify(&item.name) == *collection)?
                .variables,
        ),
        Scope::Item { collection, path } => {
            let collection = workspace
                .collections
                .iter_mut()
                .find(|item| store::slugify(&item.name) == *collection)?;
            let mut items = &mut collection.items;
            for (depth, index) in path.iter().enumerate() {
                let item = items.get_mut(*index)?;
                if depth + 1 == path.len() {
                    return Some(match &mut item.request {
                        Some(request) => &mut request.variables,
                        None => &mut item.variables,
                    });
                }
                items = &mut item.items;
            }
            None
        }
    }
}

pub(crate) fn apply(workspace: &mut store::Workspace, entries: &[Entry]) {
    for entry in entries {
        if let Some(variables) = variables_mut(workspace, &entry.scope) {
            if let Some(variable) = variables
                .iter_mut()
                .find(|variable| variable.name == entry.variable.name)
            {
                *variable = entry.variable.clone();
            } else {
                variables.push(entry.variable.clone());
            }
        }
    }
}

pub(crate) fn load(root: &Path) -> Result<store::Workspace, String> {
    let mut workspace = store::load(root)?;
    apply(&mut workspace, &entries(root)?);
    Ok(workspace)
}

/// Find the same winning definition as request expansion, not a new top layer.
pub(crate) fn target(
    workspace: &mut store::Workspace,
    slug: &str,
    path: &[usize],
    environment: Option<&str>,
    name: &str,
    create: Option<&str>,
) -> Result<Scope, String> {
    let collection = workspace
        .collections
        .iter()
        .find(|item| store::slugify(&item.name) == slug)
        .ok_or("variable collection no longer exists")?;
    check_reserved(collection, name)?;
    let mut scopes = Vec::new();
    for end in (1..=path.len()).rev() {
        scopes.push(Scope::Item {
            collection: slug.into(),
            path: path[..end].to_vec(),
        });
    }
    scopes.push(Scope::Collection {
        collection: slug.into(),
    });
    if let Some(environment) = environment {
        scopes.push(Scope::Environment {
            environment: environment.into(),
        });
    }
    scopes.push(Scope::Globals);
    for scope in scopes {
        if variables_mut(workspace, &scope)
            .is_some_and(|variables| variables.iter().any(|variable| variable.name == name))
        {
            return Ok(scope);
        }
    }
    match create.unwrap_or("collection") {
        "globals" => Ok(Scope::Globals),
        "environment" => {
            let scope = Scope::Environment {
                environment: environment.ok_or("no environment is selected")?.into(),
            };
            variables_mut(workspace, &scope).ok_or("selected environment no longer exists")?;
            Ok(scope)
        }
        _ => Ok(Scope::Collection {
            collection: slug.into(),
        }),
    }
}

pub(crate) fn check_reserved(collection: &Collection, name: &str) -> Result<(), String> {
    if collection::deployment::enabled(collection)
        && collection::deployment::RESERVED.contains(&name)
    {
        return Err(format!(
            "`{name}` is read-only and comes from the selected deployment"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let key = root.display().to_string();
        crate::persist_script_writes(
            &root,
            "demo",
            BTreeMap::from([
                (
                    format!("collection::{key}::demo"),
                    BTreeMap::from([("token".into(), "refreshed-token".into())]),
                ),
                (
                    format!("global::{key}"),
                    BTreeMap::from([("globalId".into(), "42".into())]),
                ),
                (
                    format!("environment::{key}::dev"),
                    BTreeMap::from([("origin".into(), "https://script.test".into())]),
                ),
                (
                    format!("request::{key}::demo::0.0"),
                    BTreeMap::from([("id".into(), "request-live".into())]),
                ),
            ]),
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
        assert!(write_at(&broken, &root, vec![], false).is_err());
        assert_eq!(std::fs::read(&broken).unwrap(), b"invalid");
        std::fs::remove_file(broken).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
