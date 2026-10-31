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
    AuthSetting, Body, Collection, Deployment, Environment, Item, KeyValue, RequestDef, Scripts,
    SharedScript, Variable,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_path: Option<String>,
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
#[serde(deny_unknown_fields)]
struct DeploymentsFile {
    deployments: Vec<Deployment>,
}

#[derive(Serialize, Deserialize)]
struct FolderFile {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
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
        id: committed.id,
        name: committed.name,
        service_name: committed.service_name,
        base_path: committed.base_path,
        description: committed.description,
        auth: committed.auth,
        variables: committed.variables,
        scripts: committed.scripts,
    };
    files.insert(format!("{root}/collection.yml"), yaml(&meta));
    if !committed.deployments.is_empty() {
        files.insert(
            format!("{root}/deployments.yml"),
            yaml(&DeploymentsFile {
                deployments: committed.deployments,
            }),
        );
    }

    // Rendered from `committed`: a folder or request may declare a secret too,
    // and its value must not be written to that file.
    render_items(&committed.items, &root, &root, &mut files);

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

fn render_items(items: &[Item], root: &str, base: &str, files: &mut BTreeMap<String, String>) {
    let width = items.len().to_string().len().max(2);
    let mut used = BTreeSet::new();

    for (index, item) in items.iter().enumerate() {
        let stem = unique_stem(&item.name, &mut used);
        let prefix = format!("{:0width$}", index + 1, width = width);

        match &item.request {
            Some(request) => {
                let mut request = request.clone();
                // Each saved query is its own file keyed by the request id, so
                // keeping a query never touches the request file and a rename or
                // reorder never touches the queries.
                if !item.id.is_empty() {
                    for (name, query) in std::mem::take(&mut request.queries) {
                        files.insert(
                            format!(
                                "{root}/queries/{}/{}.query",
                                item.id,
                                query_file_name(&name)
                            ),
                            query,
                        );
                    }
                }
                let file = RequestFile {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    request,
                };
                files.insert(format!("{base}/{prefix}-{stem}.yml"), yaml(&file));
            }
            None => {
                let dir = format!("{base}/{prefix}-{stem}");
                let folder = FolderFile {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    auth: item.auth.clone(),
                    variables: item.variables.clone(),
                    scripts: item.scripts.clone(),
                };
                files.insert(format!("{dir}/folder.yml"), yaml(&folder));
                render_items(&item.items, root, &dir, files);
            }
        }
    }
}

/// A query name as a file stem: characters a file name cannot hold, and `%`
/// itself, become `%XX`, so any name round-trips.
fn query_file_name(name: &str) -> String {
    let mut out = String::new();
    for (index, c) in name.char_indices() {
        let trailing = index + c.len_utf8() == name.len() && (c == '.' || c == ' ');
        if c.is_control() || "%<>:\"/\\|?*".contains(c) || trailing {
            out.push_str(&format!("%{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn query_name(stem: &str) -> String {
    let mut out = String::new();
    let mut rest = stem;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let code = rest
            .get(at + 1..at + 3)
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match code {
            Some(code) => {
                out.push(code as char);
                rest = &rest[at + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `queries/<request id>/<name>.query` over each request's queries. Files win
/// over a `queries:` map left in an older request file.
fn load_queries(dir: &Path, items: &mut [Item]) -> Result<(), String> {
    for item in items {
        load_queries(dir, &mut item.items)?;
        let Some(request) = item.request.as_mut() else {
            continue;
        };
        if item.id.is_empty() {
            continue;
        }
        let folder = dir.join("queries").join(&item.id);
        if !folder.is_dir() {
            continue;
        }
        for path in sorted_entries(&folder)?
            .into_iter()
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "query"))
        {
            request
                .queries
                .insert(query_name(&file_stem(&path)), read(&path)?);
        }
    }
    Ok(())
}

/// Adds, replaces (`Some`) or removes (`None`) one saved query of the request
/// at `path`. Only that query's file changes.
pub fn save_query(
    root: &Path,
    collection_slug: &str,
    path: &[usize],
    name: &str,
    query: Option<&str>,
) -> Result<(), String> {
    let name = required_name(name)?;
    edit_collection(root, collection_slug, |collection| {
        let request = item_at_mut(&mut collection.items, path)
            .and_then(|item| item.request.as_mut())
            .ok_or_else(|| "that request is no longer in the tree".to_string())?;
        match query {
            Some(query) if !query.trim().is_empty() => {
                request.queries.insert(name, query.to_string());
            }
            Some(_) => return Err("a query needs some text".to_string()),
            None => {
                request.queries.remove(&name);
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Moves `queries:` maps left in request files into `queries/<id>/` files.
/// Returns how many request files changed.
pub fn migrate_queries(root: &Path) -> Result<usize, String> {
    let workspace = load(root)?;
    let mut changed = 0;
    for collection in &workspace.collections {
        let rendered = render(collection, &workspace.environments);
        refuse_leaks("migrate queries", &rendered.report.leaks)?;
        let legacy: Vec<_> = rendered
            .files
            .iter()
            .filter(|(path, _)| path.ends_with(".yml") && path.starts_with("collections/"))
            .filter(|(path, _)| {
                fs::read_to_string(root.join(path))
                    .is_ok_and(|text| text.lines().any(|line| line.starts_with("queries:")))
            })
            .collect();
        if legacy.is_empty() {
            continue;
        }
        for (path, query) in &rendered.files {
            let file = root.join(path);
            if path.ends_with(".query") && !file.exists() {
                if let Some(parent) = file.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&file, query).map_err(|error| format!("writing {path}: {error}"))?;
            }
        }
        for (path, contents) in legacy {
            fs::write(root.join(path), contents)
                .map_err(|error| format!("writing {path}: {error}"))?;
            changed += 1;
        }
    }
    Ok(changed)
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

    refuse_leaks("import", &find_leaks(&files, &secrets))?;
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
    refuse_leaks("write", &rendered.report.leaks)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;

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
// Request edits carry a full draft. Keep the public edit shape inline rather
// than boxing it solely to match the much smaller metadata-only edits.
#[allow(clippy::large_enum_variant)]
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
            queries: base.queries.clone(),
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
    workspace.collections[index].assign_ids();

    let after = delta_source(&render(
        &workspace.collections[index],
        &workspace.environments,
    ));

    refuse_leaks("save", &after.report.leaks)?;

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

    refuse_leaks("save", &find_leaks(&after, &secrets))?;

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
        main_slot: None,
        platforms: Vec::new(),
        insecure,
        variables,
    };
    let (after, secrets) = environment_files(&environment);
    refuse_leaks("save", &find_leaks(&after, &secrets))?;
    apply_delta(root, &BTreeMap::new(), &after)?;
    Ok(())
}

/// Creates an empty collection and returns its slug.
pub fn create_collection(root: &Path, name: &str) -> Result<String, String> {
    let workspace = load(root)?;
    let name = required_name(name)?;
    let slug = slug_or(&name, "collection");
    if workspace
        .collections
        .iter()
        .any(|collection| slug_or(&collection.name, "collection") == slug)
    {
        return Err(format!("a collection called `{name}` already exists"));
    }
    let collection = Collection {
        id: String::new(),
        name,
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        description: None,
        auth: AuthSetting::Inherit,
        variables: Vec::new(),
        scripts: Scripts::default(),
        items: Vec::new(),
    };
    let mut collection = collection;
    collection.assign_ids();
    let rendered = render(&collection, &workspace.environments);
    refuse_leaks("save", &rendered.report.leaks)?;
    ensure_gitignore(root).map_err(|error| error.to_string())?;
    ensure_gitattributes(root).map_err(|error| error.to_string())?;
    apply_delta(root, &BTreeMap::new(), &delta_source(&rendered).files)?;
    Ok(slug)
}

/// Applies `change` to one collection and writes only what it changed. Every
/// tree edit goes through here, so each one is a render + delta with the leak
/// scan, like [`save`]. Returns `change`'s result and the collection's slug
/// afterwards (a rename changes it).
fn edit_collection<T>(
    root: &Path,
    collection_slug: &str,
    change: impl FnOnce(&mut Collection) -> Result<T, String>,
) -> Result<(T, String), String> {
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
    let result = change(&mut workspace.collections[index])?;
    workspace.collections[index].assign_ids();
    let slug = slug_or(&workspace.collections[index].name, "collection");
    if slug != collection_slug
        && workspace
            .collections
            .iter()
            .enumerate()
            .any(|(other, collection)| {
                other != index && slug_or(&collection.name, "collection") == slug
            })
    {
        return Err(format!(
            "a collection called `{}` already exists",
            workspace.collections[index].name
        ));
    }
    let after = delta_source(&render(
        &workspace.collections[index],
        &workspace.environments,
    ));
    refuse_leaks("save", &after.report.leaks)?;
    apply_delta(root, &before.files, &after.files)?;
    Ok((result, slug))
}

/// The list that holds the children of `parent`: the collection's own items
/// for an empty path, otherwise a folder's.
fn children_mut<'a>(
    collection: &'a mut Collection,
    parent: &[usize],
) -> Result<&'a mut Vec<Item>, String> {
    if parent.is_empty() {
        return Ok(&mut collection.items);
    }
    let folder = item_at_mut(&mut collection.items, parent)
        .ok_or_else(|| "that folder is no longer in the tree".to_string())?;
    if folder.request.is_some() {
        return Err("items can only be placed in a collection or a folder".to_string());
    }
    Ok(&mut folder.items)
}

fn split_path(path: &[usize]) -> Result<(&[usize], usize), String> {
    path.split_last()
        .map(|(last, parent)| (parent, *last))
        .ok_or_else(|| "that item is no longer in the tree".to_string())
}

/// Appends a folder (`request: None`) or a request under the folder at
/// `parent` (empty for the collection root) and returns its path.
///
/// Secret variables are kept per collection by name, so a new request's
/// secret that would collide with a different stored value is renamed, along
/// with its `{{name}}` uses in the request's auth.
pub fn add_item(
    root: &Path,
    collection_slug: &str,
    parent: &[usize],
    name: &str,
    request: Option<RequestDef>,
) -> Result<Vec<usize>, String> {
    let name = required_name(name)?;
    edit_collection(root, collection_slug, |collection| {
        let request = request.map(|mut request| {
            let taken: BTreeMap<String, Option<String>> = collection
                .secrets()
                .into_iter()
                .map(|variable| (variable.name.clone(), variable.value.clone()))
                .collect();
            for variable in request.variables.iter_mut().filter(|v| v.secret) {
                let original = variable.name.clone();
                let mut candidate = original.clone();
                let mut suffix = 2;
                while taken
                    .get(&candidate)
                    .is_some_and(|value| *value != variable.value)
                {
                    candidate = format!("{original}{suffix}");
                    suffix += 1;
                }
                if candidate != original {
                    if let AuthSetting::Method(auth) = &mut request.auth {
                        for field in auth.fields.values_mut() {
                            *field = field.replace(
                                &format!("{{{{{original}}}}}"),
                                &format!("{{{{{candidate}}}}}"),
                            );
                        }
                    }
                    variable.name = candidate;
                }
            }
            request
        });
        let siblings = children_mut(collection, parent)?;
        siblings.push(Item {
            id: String::new(),
            name,
            auth: AuthSetting::Inherit,
            variables: Vec::new(),
            scripts: Scripts::default(),
            request,
            items: Vec::new(),
        });
        let mut path = parent.to_vec();
        path.push(siblings.len() - 1);
        Ok(path)
    })
    .map(|(path, _)| path)
}

/// Renames a folder or request. Its file (or directory) moves with the name.
pub fn rename_item(
    root: &Path,
    collection_slug: &str,
    path: &[usize],
    name: &str,
) -> Result<(), String> {
    let name = required_name(name)?;
    edit_collection(root, collection_slug, |collection| {
        item_at_mut(&mut collection.items, path)
            .ok_or_else(|| "that item is no longer in the tree".to_string())?
            .name = name;
        Ok(())
    })
    .map(|_| ())
}

/// Renames a collection and returns its new slug; its directory and secrets
/// file move with it.
pub fn rename_collection(root: &Path, collection_slug: &str, name: &str) -> Result<String, String> {
    let name = required_name(name)?;
    edit_collection(root, collection_slug, |collection| {
        collection.name = name;
        Ok(())
    })
    .map(|(_, slug)| slug)
}

/// Removes a folder (with everything in it) or a request. Later siblings move
/// up one place.
pub fn delete_item(root: &Path, collection_slug: &str, path: &[usize]) -> Result<(), String> {
    let (parent, index) = split_path(path)?;
    edit_collection(root, collection_slug, |collection| {
        let siblings = children_mut(collection, parent)?;
        if index >= siblings.len() {
            return Err("that item is no longer in the tree".to_string());
        }
        siblings.remove(index);
        Ok(())
    })
    .map(|_| ())
}

/// Copies a folder or request, secrets included, to just below itself and
/// returns the copy's path.
pub fn duplicate_item(
    root: &Path,
    collection_slug: &str,
    path: &[usize],
) -> Result<Vec<usize>, String> {
    let (parent, index) = split_path(path)?;
    edit_collection(root, collection_slug, |collection| {
        let siblings = children_mut(collection, parent)?;
        let mut copy = siblings
            .get(index)
            .cloned()
            .ok_or_else(|| "that item is no longer in the tree".to_string())?;
        crate::model::clear_ids(&mut copy);
        let taken: BTreeSet<String> = siblings.iter().map(|item| item.name.clone()).collect();
        let base = format!("{} copy", copy.name);
        copy.name = base.clone();
        let mut suffix = 2;
        while taken.contains(&copy.name) {
            copy.name = format!("{base} {suffix}");
            suffix += 1;
        }
        siblings.insert(index + 1, copy);
        let mut path = parent.to_vec();
        path.push(index + 1);
        Ok(path)
    })
    .map(|(path, _)| path)
}

/// Moves an item within its collection. `parent` and `index` describe the
/// destination in the tree as it is once the item has been taken out, so
/// `index` may equal the destination's length to append. Returns the new path.
pub fn move_item(
    root: &Path,
    collection_slug: &str,
    from: &[usize],
    parent: &[usize],
    index: usize,
) -> Result<Vec<usize>, String> {
    let (from_parent, from_index) = split_path(from)?;
    if parent.starts_with(from) {
        return Err("a folder cannot be moved into itself".to_string());
    }
    edit_collection(root, collection_slug, |collection| {
        let siblings = children_mut(collection, from_parent)?;
        if from_index >= siblings.len() {
            return Err("that item is no longer in the tree".to_string());
        }
        let item = siblings.remove(from_index);
        let destination = children_mut(collection, parent)?;
        let index = index.min(destination.len());
        destination.insert(index, item);
        let mut path = parent.to_vec();
        path.push(index);
        Ok(path)
    })
    .map(|(path, _)| path)
}

/// Removes a collection's files and its stored secrets.
pub fn delete_collection(root: &Path, collection_slug: &str) -> Result<(), String> {
    let workspace = load(root)?;
    let collection = workspace
        .collection(collection_slug)
        .ok_or_else(|| format!("no collection called `{collection_slug}`"))?;
    let before = delta_source(&render(collection, &workspace.environments));
    apply_delta(root, &before.files, &BTreeMap::new())?;
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

    refuse_leaks("save", &find_leaks(&after, &secrets))?;

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

#[derive(Clone)]
pub struct Workspace {
    pub collections: Vec<Collection>,
    pub environments: Vec<Environment>,
    /// `globals.yml`: the widest variable layer, shared by every collection.
    pub globals: Vec<Variable>,
    /// `scripts/*.js`, by `// @id`: what a collection, folder or request runs
    /// through [`Scripts::include`].
    pub scripts: BTreeMap<String, SharedScript>,
}

impl Workspace {
    /// Finds a collection by its directory slug; `key` may be the slug or the
    /// display name.
    pub fn collection(&self, key: &str) -> Option<&Collection> {
        let slug = slugify(key);
        self.collections
            .iter()
            .find(|collection| slugify(&collection.name) == slug)
    }

    pub fn collection_mut(&mut self, key: &str) -> Option<&mut Collection> {
        let slug = slugify(key);
        self.collections
            .iter_mut()
            .find(|collection| slugify(&collection.name) == slug)
    }

    /// Finds an environment by exact name, falling back to its file slug.
    pub fn environment(&self, key: &str) -> Option<&Environment> {
        let index = self.environment_index(key)?;
        self.environments.get(index)
    }

    pub fn environment_mut(&mut self, key: &str) -> Option<&mut Environment> {
        let index = self.environment_index(key)?;
        self.environments.get_mut(index)
    }

    fn environment_index(&self, key: &str) -> Option<usize> {
        self.environments
            .iter()
            .position(|environment| environment.name == key)
            .or_else(|| {
                let slug = slugify(key);
                self.environments
                    .iter()
                    .position(|environment| slugify(&environment.name) == slug)
            })
    }
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

    let mut scripts = BTreeMap::new();
    let scripts_dir = root.join("scripts");
    if let Some(old) = sorted_yaml_files(&scripts_dir)?.first() {
        return Err(format!(
            "{}: shared scripts are now `scripts/<name>.js` files starting with `// @id <id>`; convert this file",
            old.display()
        ));
    }
    for path in sorted_entries(&scripts_dir)?
        .into_iter()
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "js"))
    {
        let script = parse_shared_script(&file_stem(&path), &read(&path)?)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(other) = scripts.insert(script.id.clone(), script) {
            return Err(format!(
                "{}: `// @id {}` is also used by scripts/{}.js; ids must be unique",
                path.display(),
                other.id,
                other.name
            ));
        }
    }

    crate::deployment::validate(&collections, &environments)?;
    Ok(Workspace {
        collections,
        environments,
        globals,
        scripts,
    })
}

/// The text of `scripts/<name>.js`.
pub fn render_shared_script(script: &SharedScript) -> String {
    format!("// @id {}\n{}", script.id, script.code)
}

/// Reads a shared script file: its first line must be `// @id <id>`.
pub fn parse_shared_script(name: &str, text: &str) -> Result<SharedScript, String> {
    let (first, code) = text.split_once('\n').unwrap_or((text, ""));
    let id = first
        .trim()
        .strip_prefix("//")
        .map(str::trim)
        .and_then(|rest| rest.strip_prefix("@id"))
        .map(str::trim)
        .filter(|id| valid_script_id(id))
        .ok_or("the first line must be `// @id <id>` (letters, digits, `-` or `_`)")?;
    Ok(SharedScript {
        id: id.to_string(),
        name: name.to_string(),
        code: code.to_string(),
    })
}

fn valid_script_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A short id no script in `taken` uses.
fn new_script_id(taken: &BTreeMap<String, SharedScript>) -> String {
    crate::model::fresh_id(&taken.keys().cloned().collect())
}

/// Writes `scripts/<name>.js` and returns its id. `id` is the script being
/// edited, or `None` for a new one. Renaming moves the file; includes refer to
/// the id, so nothing else changes.
pub fn save_shared_script(
    root: &Path,
    id: Option<&str>,
    name: &str,
    code: &str,
) -> Result<String, String> {
    let workspace = load(root)?;
    let slug = slugify(&required_name(name)?);
    if slug.is_empty() {
        return Err("a name needs at least one letter or digit".to_string());
    }
    if let Some(other) = workspace
        .scripts
        .values()
        .find(|script| script.name == slug && Some(script.id.as_str()) != id)
    {
        return Err(format!(
            "a shared script called `{}` already exists",
            other.name
        ));
    }
    let file = |script: &SharedScript| {
        (
            format!("scripts/{}.js", script.name),
            render_shared_script(script),
        )
    };
    let mut before = BTreeMap::new();
    let id = match id {
        Some(id) => {
            let old = workspace
                .scripts
                .get(id)
                .ok_or_else(|| format!("no shared script with id `{id}`"))?;
            before.extend([file(old)]);
            id.to_string()
        }
        None => new_script_id(&workspace.scripts),
    };
    let after = BTreeMap::from([file(&SharedScript {
        id: id.clone(),
        name: slug,
        code: code.to_string(),
    })]);
    refuse_leaks("save", &find_leaks(&after, &workspace_secrets(&workspace)))?;
    apply_delta(root, &before, &after)?;
    Ok(id)
}

/// Writes an id into every collection, folder and request file that lacks one
/// (or shares one with an earlier item, as a hand-copied file does). Per-user
/// state is keyed by these ids, so callers run this when opening a workspace.
/// Files are edited in place, only by their `id:` line: nothing is renamed or
/// re-rendered. Returns how many files changed.
pub fn ensure_ids(root: &Path) -> Result<usize, String> {
    scan_ids(root).map(|(changed, _)| changed)
}

/// What opening a workspace needs: [`ensure_ids`], then [`migrate_queries`]
/// only when that same pass saw a `queries:` map left in a request file.
pub fn prepare(root: &Path) -> Result<(), String> {
    if scan_ids(root)?.1 {
        migrate_queries(root)?;
    }
    Ok(())
}

/// Returns how many files got an id, and whether any still has `queries:`.
fn scan_ids(root: &Path) -> Result<(usize, bool), String> {
    // A top-level key starts in column 0; anything nested or inside a block
    // scalar is indented. Reading that one line avoids parsing every file.
    fn top_level_id(text: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix("id:"))
            .map(|value| {
                value
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string()
            })
            .filter(|id| !id.is_empty())
    }
    fn fix(
        path: &Path,
        seen: &mut BTreeSet<String>,
        changed: &mut usize,
        legacy: &mut bool,
    ) -> Result<(), String> {
        let text = read(path)?;
        *legacy |= text.lines().any(|line| line.starts_with("queries:"));
        if let Some(id) = top_level_id(&text) {
            if seen.insert(id) {
                return Ok(());
            }
        }
        let id = crate::model::fresh_id(seen);
        seen.insert(id.clone());
        let rest: String = text
            .split_inclusive('\n')
            .filter(|line| !line.starts_with("id:"))
            .collect();
        fs::write(path, format!("id: {id}\n{rest}"))
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
        *changed += 1;
        Ok(())
    }
    fn walk(
        dir: &Path,
        seen: &mut BTreeSet<String>,
        changed: &mut usize,
        legacy: &mut bool,
    ) -> Result<(), String> {
        for entry in sorted_entries(dir)? {
            if entry.is_dir() {
                let folder = entry.join("folder.yml");
                if folder.is_file() {
                    fix(&folder, seen, changed, legacy)?;
                    walk(&entry, seen, changed, legacy)?;
                }
            } else if is_numbered(&entry) {
                fix(&entry, seen, changed, legacy)?;
            }
        }
        Ok(())
    }

    let mut changed = 0;
    let mut legacy = false;
    for dir in sorted_dirs(&root.join("collections"))? {
        let meta = dir.join("collection.yml");
        if !meta.is_file() {
            continue;
        }
        // Ids are unique within a collection, which includes its own.
        let mut seen = BTreeSet::new();
        fix(&meta, &mut seen, &mut changed, &mut legacy)?;
        walk(&dir, &mut seen, &mut changed, &mut legacy)?;
    }
    Ok((changed, legacy))
}

/// Removes a shared script's file. Anything still including it fails to send
/// with a clear error until the include is removed.
pub fn delete_shared_script(root: &Path, id: &str) -> Result<(), String> {
    let workspace = load(root)?;
    let script = workspace
        .scripts
        .get(id)
        .ok_or_else(|| format!("no shared script with id `{id}`"))?;
    let before = BTreeMap::from([(
        format!("scripts/{}.js", script.name),
        render_shared_script(script),
    )]);
    apply_delta(root, &before, &BTreeMap::new())?;
    Ok(())
}

/// Every secret value in the workspace, for scanning files that belong to no
/// single collection.
fn workspace_secrets(workspace: &Workspace) -> SecretValues {
    let mut secrets: SecretValues = Vec::new();
    for collection in &workspace.collections {
        secrets.extend(
            collection
                .secrets()
                .into_iter()
                .map(|variable| (variable.name.clone(), variable.value.clone())),
        );
    }
    for variable in workspace
        .environments
        .iter()
        .flat_map(|environment| &environment.variables)
        .chain(&workspace.globals)
        .filter(|variable| variable.secret)
    {
        secrets.push((variable.name.clone(), variable.value.clone()));
    }
    secrets
}

fn load_collection(dir: &Path) -> Result<Collection, String> {
    let meta_path = dir.join("collection.yml");
    let text = read(&meta_path)?;
    let meta: CollectionFile =
        serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", meta_path.display()))?;
    let mut items = load_items(dir)?;
    load_queries(dir, &mut items)?;

    Ok(Collection {
        id: meta.id,
        name: meta.name,
        service_name: meta.service_name,
        base_path: meta.base_path,
        deployments: if dir.join("deployments.yml").is_file() {
            let path = dir.join("deployments.yml");
            let file: DeploymentsFile = serde_norway::from_str(&read(&path)?)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            file.deployments
        } else {
            Vec::new()
        },
        description: meta.description,
        auth: meta.auth,
        variables: meta.variables,
        scripts: meta.scripts,
        items,
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
                id: folder.id,
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
                id: file.id,
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
/// The one refusal every write path gives when the leak scan finds anything.
fn refuse_leaks(action: &str, leaks: &[String]) -> Result<(), String> {
    if leaks.is_empty() {
        return Ok(());
    }
    Err(format!(
        "refusing to {action}: secret value found in a committed file ({})",
        leaks.join("; ")
    ))
}

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
