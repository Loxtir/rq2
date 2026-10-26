//! The on-disk workspace.
//!
//! One file per request, YAML, because a git-synced collection has to survive
//! review and merge. A single `collection.json` puts every unrelated edit in the
//! same file, and two people adding a request in different folders conflict.
//!
//! ```text
//! collections/acme-orders/collection.yml        name, description, variables
//! collections/acme-orders/01-orders/folder.yml  folder display name
//! collections/acme-orders/01-orders/01-list-orders.yml
//! environments/dev.yml
//! .secrets/acme-orders.env                      gitignored, values only
//! ```
//!
//! The `NN-` prefix instead of an order list in a manifest: the directory
//! listing is then the order, for folders and requests together, with no second
//! file to keep in sync. Reordering renames files, which git records as renames.
//!
//! Two rules hold everywhere:
//!
//! 1. A secret variable's value is written to `.secrets/` and nowhere else.
//!    [`render`] is the single place that serialises, so there is one chokepoint
//!    to audit rather than a rule to remember at every call site.
//! 2. `.secrets/` is ignored by git before the first secret is written, not
//!    after.

use crate::model::{
    AuthSetting, Body, Collection, Environment, Item, KeyValue, RequestDef, Scripts, Variable,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

/// The line `write` guarantees in the workspace `.gitignore`.
pub const GITIGNORE_LINE: &str = "/.secrets/";

/// One file per request means many small files, so a CRLF checkout would turn
/// every save into a whole-file diff. One general rule beats a per-extension
/// list that goes stale; `auto` still leaves binaries alone.
pub const GITATTRIBUTES_LINES: [&str; 1] = ["* text=auto eol=lf"];

pub struct ImportReport {
    pub folders: usize,
    pub requests: usize,
    pub secret_variables: Vec<String>,
    /// Directory the collection was written to, relative to the workspace root.
    pub collection_dir: String,
    /// Non-empty means a committed file contains a secret value. `write`
    /// refuses to run, so this can only happen if the model is wrong.
    pub leaks: Vec<String>,
}

pub struct Rendered {
    /// Relative path to file contents. Tests inspect this instead of the disk.
    pub files: Files,
    pub report: ImportReport,
}

/// Relative path to file contents.
pub type Files = BTreeMap<String, String>;
/// A secret's name and value, where `None` means the value stays in
/// `.secrets/` and is not known to the caller.
pub type SecretValues = Vec<(String, Option<String>)>;

#[derive(Serialize, Deserialize)]
struct CollectionFile {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "AuthSetting::is_inherit")]
    auth: AuthSetting,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Scripts::is_empty")]
    scripts: Scripts,
}

#[derive(Serialize, Deserialize)]
struct FolderFile {
    name: String,
    #[serde(default, skip_serializing_if = "AuthSetting::is_inherit")]
    auth: AuthSetting,
    /// Read by every request under this folder; a request's own variables win.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Scripts::is_empty")]
    scripts: Scripts,
}

#[derive(Serialize, Deserialize)]
struct RequestFile {
    name: String,
    #[serde(flatten)]
    request: RequestDef,
}

/// `globals.yml`: the widest variable layer, shared by the whole workspace.
#[derive(Serialize, Deserialize)]
struct GlobalsFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variables: Vec<Variable>,
}

pub fn render(collection: &Collection, environments: &[Environment]) -> Rendered {
    let (mut files, collection_secrets) = collection_files(collection);
    let mut example = String::new();
    let mut secrets: SecretValues = Vec::new();
    let mut report = ImportReport {
        folders: collection.folder_count(),
        requests: collection.requests().len(),
        secret_variables: Vec::new(),
        collection_dir: format!("collections/{}", slug_or(&collection.name, "collection")),
        leaks: Vec::new(),
    };

    example_block(
        &mut example,
        "collection",
        &collection.name,
        &collection_secrets,
    );
    for (name, value) in collection_secrets {
        report.secret_variables.push(name.clone());
        secrets.push((name, value));
    }

    for environment in environments {
        let (env_files, env_secrets) = environment_files(environment);
        files.extend(env_files);
        example_block(&mut example, "environment", &environment.name, &env_secrets);
        for (name, value) in env_secrets {
            report.secret_variables.push(name.clone());
            secrets.push((name, value));
        }
    }

    if !example.is_empty() {
        files.insert("secrets.example.env".to_string(), example);
    }

    report.secret_variables.sort();
    report.secret_variables.dedup();
    report.leaks = find_leaks(&files, &secrets);

    Rendered { files, report }
}

/// The files a single collection owns, plus its secrets as `(name, value)`.
/// The value is `None` for a secret that has not been filled in yet; its name
/// still belongs in `secrets.example.env`.
fn collection_files(collection: &Collection) -> (Files, SecretValues) {
    let mut files = BTreeMap::new();
    let root = format!("collections/{}", slug_or(&collection.name, "collection"));

    let mut committed = collection.clone();
    strip_secrets(committed.all_variables_mut());
    let meta = CollectionFile {
        name: committed.name,
        description: committed.description,
        auth: committed.auth,
        variables: committed.variables,
        scripts: committed.scripts,
    };
    files.insert(format!("{root}/collection.yml"), yaml(&meta));

    // Rendered from `committed`: a folder or request may declare a secret too,
    // and its value must not be written to that file.
    render_items(&committed.items, &root, &mut files);

    let secrets: SecretValues = collection
        .secrets()
        .into_iter()
        .map(|variable| (variable.name.clone(), variable.value.clone()))
        .collect();

    let body = secret_body(&secrets);
    if !body.is_empty() {
        files.insert(
            format!(".secrets/{}.env", slug_or(&collection.name, "collection")),
            body,
        );
    }

    (files, secrets)
}

fn environment_files(environment: &Environment) -> (Files, SecretValues) {
    let mut files = BTreeMap::new();
    let slug = slug_or(&environment.name, "environment");

    let mut committed = environment.clone();
    strip_secrets(committed.variables.iter_mut());
    files.insert(format!("environments/{slug}.yml"), yaml(&committed));

    let secrets: SecretValues = environment
        .secrets()
        .into_iter()
        .map(|variable| (variable.name.clone(), variable.value.clone()))
        .collect();

    let body = secret_body(&secrets);
    if !body.is_empty() {
        files.insert(format!(".secrets/{slug}.env"), body);
    }

    (files, secrets)
}

fn globals_files(variables: &[Variable]) -> (Files, SecretValues) {
    let mut files = BTreeMap::new();

    let mut committed = variables.to_vec();
    strip_secrets(committed.iter_mut());
    files.insert(
        "globals.yml".to_string(),
        yaml(&GlobalsFile {
            variables: committed,
        }),
    );

    let secrets: SecretValues = variables
        .iter()
        .filter(|variable| variable.secret)
        .map(|variable| (variable.name.clone(), variable.value.clone()))
        .collect();

    let body = secret_body(&secrets);
    if !body.is_empty() {
        files.insert(".secrets/globals.env".to_string(), body);
    }

    (files, secrets)
}

/// One `NAME=value` line per secret that has a value, in scope order.
fn secret_body(secrets: &SecretValues) -> String {
    secrets
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|value| format!("{name}={}\n", quote_env(value)))
        })
        .collect()
}

/// One block of `secrets.example.env`: the names only, grouped by scope.
fn example_block(out: &mut String, kind: &str, name: &str, secrets: &SecretValues) {
    if secrets.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("# {kind}: {name}\n"));
    for (secret, _) in secrets {
        out.push_str(&format!("{secret}=\n"));
    }
}

fn render_items(items: &[Item], base: &str, files: &mut BTreeMap<String, String>) {
    let width = items.len().to_string().len().max(2);
    let mut used = BTreeSet::new();

    for (index, item) in items.iter().enumerate() {
        let stem = unique_stem(&item.name, &mut used);
        let prefix = format!("{:0width$}", index + 1, width = width);

        match &item.request {
            Some(request) => {
                let file = RequestFile {
                    name: item.name.clone(),
                    request: request.clone(),
                };
                files.insert(format!("{base}/{prefix}-{stem}.yml"), yaml(&file));
            }
            None => {
                let dir = format!("{base}/{prefix}-{stem}");
                let folder = FolderFile {
                    name: item.name.clone(),
                    auth: item.auth.clone(),
                    variables: item.variables.clone(),
                    scripts: item.scripts.clone(),
                };
                files.insert(format!("{dir}/folder.yml"), yaml(&folder));
                render_items(&item.items, &dir, files);
            }
        }
    }
}

fn unique_stem(name: &str, used: &mut BTreeSet<String>) -> String {
    let base = slug_or(name, "untitled");
    if used.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Imports one Postman export into an existing workspace without replacing data.
/// Globals add new names; collections and environments must have unused paths.
pub fn import_postman(
    root: &Path,
    kind: &str,
    bytes: &[u8],
    extra_secrets: &[String],
) -> Result<Vec<String>, String> {
    let workspace = load(root)?;
    let before = if kind == "globals" {
        globals_files(&workspace.globals).0
    } else {
        BTreeMap::new()
    };
    let (files, secrets, warnings) = match kind {
        "collection" => {
            let parsed = crate::postman::parse_collection(bytes, extra_secrets)?;
            required_name(&parsed.collection.name)?;
            let directory = format!(
                "collections/{}",
                slug_or(&parsed.collection.name, "collection")
            );
            if root.join(&directory).exists() {
                return Err(format!("refusing to overwrite existing `{directory}`"));
            }
            let (files, secrets) = collection_files(&parsed.collection);
            (files, secrets, parsed.warnings)
        }
        "environment" => {
            let parsed = crate::postman::parse_environment(bytes, extra_secrets)?;
            required_name(&parsed.environment.name)?;
            let slug = slug_or(&parsed.environment.name, "environment");
            if workspace
                .environments
                .iter()
                .any(|env| slug_or(&env.name, "environment") == slug)
            {
                return Err(format!(
                    "an environment called `{}` already exists",
                    parsed.environment.name
                ));
            }
            let (files, secrets) = environment_files(&parsed.environment);
            (files, secrets, parsed.warnings)
        }
        "globals" => {
            let parsed = crate::postman::parse_globals(bytes, extra_secrets)?;
            let mut variables = workspace.globals;
            for variable in parsed.variables {
                if variables
                    .iter()
                    .any(|existing| existing.name == variable.name)
                {
                    return Err(format!(
                        "global variable `{}` already exists; edit it in Globals instead",
                        variable.name
                    ));
                }
                variables.push(variable);
            }
            let (files, secrets) = globals_files(&variables);
            (files, secrets, parsed.warnings)
        }
        _ => return Err("choose collection, environment, or globals".to_string()),
    };

    let leaks = find_leaks(&files, &secrets);
    if !leaks.is_empty() {
        return Err(format!(
            "refusing to import: secret value found in a committed file ({})",
            leaks.join("; ")
        ));
    }
    for path in files.keys() {
        // Globals were explicitly merged above, including their stored secrets.
        if root.join(path).exists() && (kind != "globals" || !before.contains_key(path)) {
            return Err(format!("refusing to overwrite existing `{path}`"));
        }
    }
    ensure_gitignore(root).map_err(|error| error.to_string())?;
    ensure_gitattributes(root).map_err(|error| error.to_string())?;
    apply_delta(root, &before, &files)?;
    Ok(warnings)
}

pub fn write(out: &Path, rendered: &Rendered) -> io::Result<()> {
    if !rendered.report.leaks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing to write: secret value found in a committed file ({})",
                rendered.report.leaks.join("; ")
            ),
        ));
    }

    for (relative, contents) in &rendered.files {
        let path = out.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
    }
    ensure_gitignore(out)?;
    ensure_gitattributes(out)?;
    Ok(())
}

/// One edit made in the window.
///
/// Saving does not write files directly: it applies the edit to the model,
/// re-renders, and writes only the difference. That keeps [`render`] the single
/// serialiser (so a secret has one path to the disk), and it makes a rename one
/// file move rather than a tree rewrite.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Edit {
    Request {
        path: Vec<usize>,
        draft: RequestDraft,
    },
    Folder {
        path: Vec<usize>,
        name: String,
        auth: AuthSetting,
        #[serde(default)]
        scripts: Scripts,
    },
    Collection {
        name: String,
        description: Option<String>,
        variables: Vec<Variable>,
        auth: AuthSetting,
        #[serde(default)]
        scripts: Scripts,
    },
    /// Only the variables of the folder or request at `path`. The hover card
    /// writes through this instead of replacing a whole request, so a
    /// half-finished edit to its URL is not lost.
    Variables {
        path: Vec<usize>,
        variables: Vec<Variable>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RequestDraft {
    pub name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub params: Vec<KeyValue>,
    #[serde(default)]
    pub path_params: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub body: Option<Body>,
    /// `None` keeps whatever the stored request had, so a caller that does not
    /// edit auth (the CLI, an older window) cannot clear it by omission.
    #[serde(default)]
    pub auth: Option<AuthSetting>,
    /// `None` keeps the stored request's variables, for the same reason. The
    /// hover card edits them through [`Edit::Variables`] instead.
    #[serde(default)]
    pub variables: Option<Vec<Variable>>,
    #[serde(default)]
    pub scripts: Option<Scripts>,
}

#[derive(Debug)]
pub struct SaveOutcome {
    /// The collection's slug afterwards; a rename changes it.
    pub collection: String,
    pub written: usize,
    pub removed: usize,
}

impl RequestDraft {
    /// The draft's editable fields on top of the stored request, so whatever the
    /// editor does not own (`description`) survives a send or a save.
    pub fn onto(&self, base: &RequestDef) -> RequestDef {
        let mut request = RequestDef {
            method: self.method.trim().to_uppercase(),
            url: self.url.clone(),
            params: self.params.clone(),
            path_params: self.path_params.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            auth: self.auth.clone().unwrap_or_else(|| base.auth.clone()),
            variables: self
                .variables
                .clone()
                .unwrap_or_else(|| base.variables.clone()),
            scripts: self.scripts.clone().unwrap_or_else(|| base.scripts.clone()),
            description: base.description.clone(),
        };
        // A URL typed with `?a=1` still works; the canonical form keeps the
        // query in `params` so a later toggle can disable one entry.
        request.normalize_query();
        request
    }
}

pub fn save(root: &Path, collection_slug: &str, edit: &Edit) -> Result<SaveOutcome, String> {
    let mut workspace = load(root)?;
    let index = workspace
        .collections
        .iter()
        .position(|collection| slug_or(&collection.name, "collection") == collection_slug)
        .ok_or_else(|| format!("no collection called `{collection_slug}`"))?;

    let before = delta_source(&render(
        &workspace.collections[index],
        &workspace.environments,
    ));

    match edit {
        Edit::Request { path, draft } => {
            let item = item_at_mut(&mut workspace.collections[index].items, path)
                .ok_or_else(|| "that request is no longer in the tree".to_string())?;
            let request = item
                .request
                .as_mut()
                .ok_or_else(|| "that item is a folder".to_string())?;

            item.name = required_name(&draft.name)?;
            let updated = draft.onto(request);
            *request = updated;
        }
        Edit::Folder {
            path,
            name,
            auth,
            scripts,
        } => {
            let item = item_at_mut(&mut workspace.collections[index].items, path)
                .ok_or_else(|| "that folder is no longer in the tree".to_string())?;
            if item.request.is_some() {
                return Err("that item is a request, not a folder".to_string());
            }
            item.name = required_name(name)?;
            item.auth = auth.clone();
            item.scripts = scripts.clone();
        }
        Edit::Collection {
            name,
            description,
            variables,
            auth,
            scripts,
        } => {
            let collection = &mut workspace.collections[index];
            collection.name = required_name(name)?;
            collection.description = description
                .as_ref()
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            collection.variables = keep_secret_values(&collection.variables, variables);
            collection.auth = auth.clone();
            collection.scripts = scripts.clone();
        }
        Edit::Variables { path, variables } => {
            let item = item_at_mut(&mut workspace.collections[index].items, path)
                .ok_or_else(|| "that folder or request is no longer in the tree".to_string())?;
            let existing = item
                .request
                .as_ref()
                .map(|request| request.variables.clone())
                .unwrap_or_else(|| item.variables.clone());
            let variables = keep_secret_values(&existing, variables);
            match &mut item.request {
                Some(request) => request.variables = variables,
                None => item.variables = variables,
            }
        }
    }

    let after = delta_source(&render(
        &workspace.collections[index],
        &workspace.environments,
    ));

    if !after.report.leaks.is_empty() {
        return Err(format!(
            "refusing to save: secret value found in a committed file ({})",
            after.report.leaks.join("; ")
        ));
    }

    let (written, removed) = apply_delta(root, &before.files, &after.files)?;

    Ok(SaveOutcome {
        collection: slug_or(&workspace.collections[index].name, "collection"),
        written,
        removed,
    })
}

/// Saves an environment's variables.
///
/// Separate from [`save`] because an environment does not belong to a
/// collection: there is no slug to hand back and no tree to re-render. The
/// delta still comes from the same renderer, so a secret has one path to disk.
pub fn save_environment(
    root: &Path,
    environment_slug: &str,
    variables: Vec<Variable>,
) -> Result<(), String> {
    let workspace = load(root)?;
    let environment = workspace
        .environments
        .iter()
        .find(|environment| slugify(&environment.name) == environment_slug)
        .ok_or_else(|| format!("no environment called `{environment_slug}`"))?;
    update_environment(
        root,
        environment_slug,
        environment.name.clone(),
        environment.insecure,
        variables,
    )
}

/// Creates or updates an environment, moving its secret file when it is renamed.
pub fn update_environment(
    root: &Path,
    old_slug: &str,
    name: String,
    insecure: bool,
    variables: Vec<Variable>,
) -> Result<(), String> {
    let mut workspace = load(root)?;
    let name = required_name(&name)?;
    let new_slug = slugify(&name);
    if workspace
        .environments
        .iter()
        .any(|environment| slugify(&environment.name) == new_slug && old_slug != new_slug)
    {
        return Err(format!("an environment called `{name}` already exists"));
    }
    let environment = workspace
        .environments
        .iter_mut()
        .find(|environment| slugify(&environment.name) == old_slug)
        .ok_or_else(|| format!("no environment called `{old_slug}`"))?;

    let before = environment_files(environment).0;
    environment.name = name;
    environment.insecure = insecure;
    environment.variables = keep_secret_values(&environment.variables, &variables);
    let (after, secrets) = environment_files(environment);

    let leaks = find_leaks(&after, &secrets);
    if !leaks.is_empty() {
        return Err(format!(
            "refusing to save: secret value found in a committed file ({})",
            leaks.join("; ")
        ));
    }

    apply_delta(root, &before, &after)?;
    Ok(())
}

pub fn create_environment(
    root: &Path,
    name: String,
    insecure: bool,
    variables: Vec<Variable>,
) -> Result<(), String> {
    let workspace = load(root)?;
    let name = required_name(&name)?;
    let slug = slugify(&name);
    if workspace
        .environments
        .iter()
        .any(|environment| slugify(&environment.name) == slug)
    {
        return Err(format!("an environment called `{name}` already exists"));
    }
    let environment = Environment {
        name,
        insecure,
        variables,
    };
    let (after, secrets) = environment_files(&environment);
    let leaks = find_leaks(&after, &secrets);
    if !leaks.is_empty() {
        return Err(format!(
            "refusing to save: secret value found in a committed file ({})",
            leaks.join("; ")
        ));
    }
    apply_delta(root, &BTreeMap::new(), &after)?;
    Ok(())
}

pub fn delete_environment(root: &Path, environment_slug: &str) -> Result<(), String> {
    let mut workspace = load(root)?;
    let index = workspace
        .environments
        .iter()
        .position(|environment| slugify(&environment.name) == environment_slug)
        .ok_or_else(|| format!("no environment called `{environment_slug}`"))?;
    let before = environment_files(&workspace.environments.remove(index)).0;
    apply_delta(root, &before, &BTreeMap::new())?;
    Ok(())
}

/// Saves `globals.yml`, the widest variable layer.
///
/// Like an environment it belongs to no collection, so there is no tree to hand
/// back. It still goes through the same renderer, delta, and leak scan, so a
/// global secret has one path to disk like every other.
pub fn save_globals(root: &Path, variables: Vec<Variable>) -> Result<(), String> {
    let workspace = load(root)?;
    let variables = keep_secret_values(&workspace.globals, &variables);

    let before = globals_files(&workspace.globals).0;
    let (after, secrets) = globals_files(&variables);

    let leaks = find_leaks(&after, &secrets);
    if !leaks.is_empty() {
        return Err(format!(
            "refusing to save: secret value found in a committed file ({})",
            leaks.join("; ")
        ));
    }

    apply_delta(root, &before, &after)?;
    Ok(())
}

/// `secrets.example.env` lists names from every collection and environment, but
/// [`render`] only knows about one collection. Rewriting it from a single save
/// would drop the other scopes' names, so it is left to the import path.
fn delta_source(rendered: &Rendered) -> Rendered {
    let mut files = rendered.files.clone();
    files.remove("secrets.example.env");
    Rendered {
        files,
        report: ImportReport {
            folders: rendered.report.folders,
            requests: rendered.report.requests,
            secret_variables: rendered.report.secret_variables.clone(),
            collection_dir: rendered.report.collection_dir.clone(),
            leaks: rendered.report.leaks.clone(),
        },
    }
}

fn apply_delta(
    root: &Path,
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Result<(usize, usize), String> {
    let mut removed: Vec<String> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .cloned()
        .collect();
    let added: Vec<String> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .cloned()
        .collect();

    // A renamed collection moves its directory, which moves `.secrets/<slug>.env`
    // too. Pair identical content so the secret file is *renamed* rather than
    // deleted and rewritten; a delete alone would lose the values on disk.
    let mut renamed_to: BTreeSet<String> = BTreeSet::new();
    for path in &added {
        let Some(position) = removed
            .iter()
            .position(|old| before.get(old) == after.get(path))
        else {
            continue;
        };
        let old = removed.remove(position);
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::rename(root.join(&old), &target)
            .map_err(|error| format!("renaming {} to {}: {error}", old, path))?;
        renamed_to.insert(path.clone());
    }

    let mut written = 0;
    for (path, contents) in after {
        if renamed_to.contains(path) || before.get(path) == Some(contents) {
            continue;
        }
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&target, contents).map_err(|error| format!("writing {path}: {error}"))?;
        written += 1;
    }

    for path in &removed {
        let target = root.join(path);
        if fs::remove_file(&target).is_err() {
            continue;
        }
        prune_empty_dirs(root, path);
    }

    // A rename empties the directory it left behind, and that directory has no
    // removal of its own to trigger a prune. Sweep the old paths instead; the
    // walk stops at the first directory that still has anything in it.
    for path in before.keys() {
        prune_empty_dirs(root, path);
    }

    Ok((written, removed.len()))
}

/// After removing the last file in a folder, take the folder with it.
fn prune_empty_dirs(root: &Path, removed_file: &str) {
    let stop = root.join("collections");
    let mut current = root.join(removed_file).parent().map(Path::to_path_buf);

    while let Some(dir) = current {
        if !dir.starts_with(&stop) || dir == stop {
            break;
        }
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }
}

fn item_at_mut<'a>(items: &'a mut [Item], path: &[usize]) -> Option<&'a mut Item> {
    let (first, rest) = path.split_first()?;
    let item = items.get_mut(*first)?;
    if rest.is_empty() {
        Some(item)
    } else {
        item_at_mut(&mut item.items, rest)
    }
}

fn required_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a name is required".to_string());
    }
    Ok(name.to_string())
}

/// A secret's value never has to cross into the window, so an edit that arrives
/// without one keeps the value already on disk.
fn keep_secret_values(existing: &[Variable], edited: &[Variable]) -> Vec<Variable> {
    edited
        .iter()
        .map(|variable| {
            let mut variable = variable.clone();
            if variable.secret && variable.value.is_none() {
                if let Some(previous) = existing.iter().find(|old| old.name == variable.name) {
                    variable.value = previous.value.clone();
                }
            }
            variable
        })
        .collect()
}

pub struct Workspace {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    /// `globals.yml`: the widest variable layer, shared by every collection.
    pub globals: Vec<Variable>,
}
/// Reads a workspace back, re-attaching secret values from `.secrets/`.
pub fn load(root: &Path) -> Result<Workspace, String> {
    let mut collections = Vec::new();
    for path in sorted_dirs(&root.join("collections"))? {
        let slug = file_stem(&path);
        let mut collection = load_collection(&path)?;
        // Old and hand-written files may still carry `?a=1` inside `url`; the
        // window and the sender only ever see the canonical form.
        collection.normalize();
        let values = read_env(&root.join(".secrets").join(format!("{slug}.env")));
        attach_secrets(collection.all_variables_mut(), &values);
        collections.push(collection);
    }

    let mut environments = Vec::new();
    for path in sorted_yaml_files(&root.join("environments"))? {
        let slug = file_stem(&path);
        let text = read(&path)?;
        let mut environment: Environment =
            serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let values = read_env(&root.join(".secrets").join(format!("{slug}.env")));
        attach_secrets(environment.variables.iter_mut(), &values);
        environments.push(environment);
    }

    let globals_path = root.join("globals.yml");
    let mut globals = if globals_path.is_file() {
        let text = read(&globals_path)?;
        let file: GlobalsFile = serde_norway::from_str(&text)
            .map_err(|e| format!("{}: {e}", globals_path.display()))?;
        file.variables
    } else {
        Vec::new()
    };
    let values = read_env(&root.join(".secrets").join("globals.env"));
    attach_secrets(globals.iter_mut(), &values);

    Ok(Workspace {
        collections,
        environments,
        globals,
    })
}

fn load_collection(dir: &Path) -> Result<Collection, String> {
    let meta_path = dir.join("collection.yml");
    let text = read(&meta_path)?;
    let meta: CollectionFile =
        serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", meta_path.display()))?;

    Ok(Collection {
        name: meta.name,
        description: meta.description,
        auth: meta.auth,
        variables: meta.variables,
        scripts: meta.scripts,
        items: load_items(dir)?,
    })
}

fn load_items(dir: &Path) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();

    for entry in sorted_entries(dir)? {
        if entry.is_dir() {
            let folder_path = entry.join("folder.yml");
            if !folder_path.exists() {
                continue;
            }
            let text = read(&folder_path)?;
            let folder: FolderFile = serde_norway::from_str(&text)
                .map_err(|e| format!("{}: {e}", folder_path.display()))?;
            items.push(Item {
                name: folder.name,
                auth: folder.auth,
                variables: folder.variables,
                scripts: folder.scripts,
                request: None,
                items: load_items(&entry)?,
            });
        } else if is_numbered(&entry) {
            let text = read(&entry)?;
            let file: RequestFile =
                serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", entry.display()))?;
            items.push(Item {
                name: file.name,
                auth: AuthSetting::Inherit,
                variables: Vec::new(),
                scripts: Scripts::default(),
                request: Some(file.request),
                items: Vec::new(),
            });
        }
    }

    Ok(items)
}

fn read_env(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    parse_env(&text).into_iter().collect()
}

/// Re-attaches values from `.secrets/` by name.
///
/// Collection, folder, and request secrets share the collection's `.env`, so a
/// secret name is unique within one collection directory.
fn attach_secrets<'a>(
    variables: impl IntoIterator<Item = &'a mut Variable>,
    values: &BTreeMap<String, String>,
) {
    for variable in variables {
        if variable.secret {
            if let Some(value) = values.get(&variable.name) {
                variable.value = Some(value.clone());
            }
        }
    }
}

fn sorted_entries(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    Ok(paths)
}

fn sorted_dirs(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    Ok(sorted_entries(dir)?
        .into_iter()
        .filter(|path| path.is_dir())
        .collect())
}

fn sorted_yaml_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    Ok(sorted_entries(dir)?
        .into_iter()
        .filter(|path| path.is_file() && is_yaml(path))
        .collect())
}

fn is_yaml(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yml") | Some("yaml")
    )
}

/// `01-list-orders.yml` is a request; `collection.yml` and `notes.md` are not.
fn is_numbered(path: &Path) -> bool {
    if !is_yaml(path) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let digits = name.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && name[digits..].starts_with('-')
}

/// The file or directory name without a `.yml` / `.yaml` extension. Collection
/// directories have no extension, so this is the directory name for them.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Adds `/.secrets/` to the workspace `.gitignore`, keeping whatever is there.
/// Returns true when the file changed.
pub fn ensure_gitignore(out: &Path) -> io::Result<bool> {
    let path = out.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();

    if existing
        .lines()
        .any(|line| line.trim().trim_end_matches('/') == GITIGNORE_LINE.trim_end_matches('/'))
    {
        return Ok(false);
    }

    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(GITIGNORE_LINE);
    next.push('\n');
    fs::write(&path, next)?;
    Ok(true)
}

/// Adds the newline rules a synced workspace needs, one line at a time, so an
/// existing `.gitattributes` keeps its own rules.
pub fn ensure_gitattributes(out: &Path) -> io::Result<bool> {
    let path = out.join(".gitattributes");
    let existing = fs::read_to_string(&path).unwrap_or_default();

    let missing: Vec<&str> = GITATTRIBUTES_LINES
        .into_iter()
        .filter(|wanted| !existing.lines().any(|line| line.trim() == *wanted))
        .collect();

    if missing.is_empty() {
        return Ok(false);
    }

    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    for line in missing {
        next.push_str(line);
        next.push('\n');
    }
    fs::write(&path, next)?;
    Ok(true)
}

fn strip_secrets<'a>(variables: impl IntoIterator<Item = &'a mut Variable>) {
    for variable in variables {
        if variable.secret {
            variable.value = None;
        }
    }
}

fn yaml<T: Serialize>(value: &T) -> String {
    serde_norway::to_string(value).expect("model is serialisable")
}

/// Belt and braces for the one rule that must never break. Values shorter than
/// six characters are skipped because they collide with ordinary text.
fn find_leaks(files: &Files, secrets: &SecretValues) -> Vec<String> {
    let secrets: Vec<(&str, &str)> = secrets
        .iter()
        .filter_map(|(name, value)| value.as_deref().map(|value| (name.as_str(), value)))
        .collect();

    let mut leaks = Vec::new();
    for (path, contents) in files {
        if path.starts_with(".secrets/") {
            continue;
        }
        for (name, value) in &secrets {
            if value.len() >= 6 && contents.contains(value) {
                leaks.push(format!("`{name}` leaked into {path}"));
            }
        }
    }
    leaks
}

pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn slug_or(name: &str, fallback: &str) -> String {
    let slug = slugify(name);
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

/// Quotes a value only when a naive parser would get it wrong.
pub fn quote_env(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let safe = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/+@".contains(c));
    if safe {
        return value.to_string();
    }

    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Reads back what `quote_env` wrote. The HTTP engine will need this.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, raw)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        out.push((name.to_string(), unquote(raw.trim())));
    }
    out
}

fn unquote(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return raw.to_string();
    };

    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}
