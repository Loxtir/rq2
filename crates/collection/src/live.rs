//! Per-user current values. Saved workspace defaults are never modified here.
//!
//! Values live in one JSON file outside every workspace, keyed by workspace
//! path, so a value typed into one checkout never reaches a committed file.

use crate::model::{Collection, Variable};
use crate::store::{self, Workspace};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes writers within this process; the file itself is replaced
/// atomically, so a reader never sees half a write.
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
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
pub struct Entry {
    pub scope: Scope,
    pub variable: Variable,
}

#[derive(Default, Serialize, Deserialize)]
struct Values {
    #[serde(default)]
    workspaces: BTreeMap<String, Vec<Entry>>,
}

/// The per-user values file, usually `<config dir>/variables.json`.
pub struct LiveValues {
    path: PathBuf,
}

impl LiveValues {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `<config dir>/rq/variables.json`, the file the desktop app writes:
    /// `%APPDATA%` on Windows, `~/Library/Application Support` on macOS,
    /// `$XDG_CONFIG_HOME` (or `~/.config`) elsewhere.
    pub fn default_path() -> Option<PathBuf> {
        let env = |name| std::env::var_os(name).filter(|value| !value.is_empty());
        let config = if cfg!(windows) {
            PathBuf::from(env("APPDATA")?)
        } else if cfg!(target_os = "macos") {
            PathBuf::from(env("HOME")?).join("Library/Application Support")
        } else {
            env("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from(env("HOME")?).join(".config")))?
        };
        Some(config.join("rq").join("variables.json"))
    }

    pub fn entries(&self, root: &Path) -> Result<Vec<Entry>, String> {
        Ok(read(&self.path)?
            .workspaces
            .remove(&workspace_key(root))
            .unwrap_or_default())
    }

    /// Replaces each edit's `(scope, name)` entry, or removes it when `reset`.
    pub fn write(&self, root: &Path, edits: Vec<Entry>, reset: bool) -> Result<(), String> {
        let _guard = WRITER
            .lock()
            .map_err(|_| "local variable config lock poisoned")?;
        let mut values = read(&self.path)?;
        let entries = values.workspaces.entry(workspace_key(root)).or_default();
        for edit in edits {
            entries.retain(|entry| {
                entry.scope != edit.scope || entry.variable.name != edit.variable.name
            });
            if !reset {
                entries.push(edit);
            }
        }
        let parent = self
            .path
            .parent()
            .ok_or("local variable config has no parent")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&values).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)
            .map_err(|error| format!("saving local variables: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("saving local variables: {error}"))
    }

    /// The workspace on disk with this user's current values laid over it.
    pub fn load(&self, root: &Path) -> Result<Workspace, String> {
        let mut workspace = store::load(root)?;
        apply(&mut workspace, &self.entries(root)?);
        Ok(workspace)
    }
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

pub fn variables_mut<'a>(
    workspace: &'a mut Workspace,
    scope: &Scope,
) -> Option<&'a mut Vec<Variable>> {
    match scope {
        Scope::Globals => Some(&mut workspace.globals),
        Scope::Environment { environment } => {
            Some(&mut workspace.environment_mut(environment)?.variables)
        }
        Scope::Collection { collection } => {
            Some(&mut workspace.collection_mut(collection)?.variables)
        }
        Scope::Item { collection, path } => {
            let collection = workspace.collection_mut(collection)?;
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

pub fn apply(workspace: &mut Workspace, entries: &[Entry]) {
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

/// Find the same winning definition as request expansion, not a new top layer.
///
/// `create` says where a name nothing defines goes: `globals`, `environment`,
/// or the collection (the default).
pub fn target(
    workspace: &mut Workspace,
    slug: &str,
    path: &[usize],
    environment: Option<&str>,
    name: &str,
    create: Option<&str>,
) -> Result<Scope, String> {
    let collection = workspace
        .collection(slug)
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

pub fn check_reserved(collection: &Collection, name: &str) -> Result<(), String> {
    if crate::deployment::enabled(collection) && crate::deployment::RESERVED.contains(&name) {
        return Err(format!(
            "`{name}` is read-only and comes from the selected deployment"
        ));
    }
    Ok(())
}
