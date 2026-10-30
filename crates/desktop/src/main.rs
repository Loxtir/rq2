//! The desktop shell.
//!
//! One rule shapes everything here: **the response body never crosses into the
//! webview**. `send` keeps the bytes in this process and returns metadata; the
//! frontend asks for a projection and gets back the projected text. That is the
//! same boundary the finished product needs for a 500k-line body, and it costs
//! nothing to hold to it now.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use collection::context::RequestContext;
use collection::model::{
    AuthSetting, Body, Collection, Environment, Item, KeyValue, Scripts, Variable,
};
use collection::store::{self, Edit, RequestDraft};
#[cfg(test)]
use response_query::project;
use response_query::{infer, scaffold};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

mod history;
mod live;
mod query;
mod view;

use history::History;

/// Raw bodies larger than this are still kept, but the raw view is truncated so
/// a careless click does not push tens of megabytes through IPC.
const RAW_VIEW_LIMIT: usize = 512 * 1024;
/// Keep a handful of bodies around: enough to flip between two responses.
const MAX_KEPT_RESPONSES: usize = 24;

#[derive(Default)]
struct AppState {
    workspace: Mutex<Option<PathBuf>>,
    responses: Mutex<Responses>,
    cancellations: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
}

/// Everything `send` works out before the network call.
struct Prepared {
    name: String,
    target: Option<collection::deployment::Target>,
    collection: String,
    path: Vec<usize>,
    auth: Option<collection::model::Auth>,
    options: collection::send::SendOptions,
    request: collection::model::RequestDef,
    plan: rq_scripts::Plan,
    variables: BTreeMap<String, String>,
}

/// Script writes update local current values, never shared defaults.
fn persist_script_writes(
    root: &std::path::Path,
    collection_slug: &str,
    writes: rq_scripts::Writes,
) -> Result<(), String> {
    if writes.is_empty() {
        return Ok(());
    }
    let mut workspace = live::load(root)?;
    let edits = rq_scripts::edits(&mut workspace, collection_slug, writes)?;
    live::write(root, edits, false)
}

#[derive(Default)]
struct Responses {
    next_id: u64,
    bodies: BTreeMap<u64, Vec<u8>>,
    next_view_id: u64,
    views: BTreeMap<u64, (u64, view::FrozenQuery)>,
}

impl Responses {
    fn insert(&mut self, body: Vec<u8>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.bodies.insert(id, body);
        while self.bodies.len() > MAX_KEPT_RESPONSES {
            let oldest = *self.bodies.keys().next().expect("non-empty");
            self.bodies.remove(&oldest);
            self.views.retain(|_, (response, _)| *response != oldest);
        }
        id
    }

    fn remember_view(&mut self, response_id: u64, query: view::FrozenQuery) -> Result<u64, String> {
        if !self.bodies.contains_key(&response_id) {
            return Err("that response is no longer in memory; send it again".into());
        }
        self.next_view_id += 1;
        self.views.insert(self.next_view_id, (response_id, query));
        // Bound stored query literals as well as response bodies.
        while self.views.len() > MAX_KEPT_RESPONSES * 4 {
            self.views.pop_first();
        }
        Ok(self.next_view_id)
    }

    fn export_view(&self, response_id: u64, view_id: u64) -> Result<Vec<u8>, String> {
        let (id, query) = self
            .views
            .get(&view_id)
            .ok_or("that applied view is no longer in memory; Apply the query again")?;
        if *id != response_id {
            return Err("that applied view belongs to a different response".into());
        }
        query.export(
            self.bodies
                .get(id)
                .ok_or("that response is no longer in memory; send it again")?,
        )
    }
}

#[derive(Serialize)]
struct WorkspaceDto {
    path: String,
    collections: Vec<CollectionDto>,
    environments: Vec<EnvironmentDto>,
    /// `globals.yml`: the widest variable layer, shared by every collection.
    globals: Vec<VariableDto>,
    live_variables: Vec<LiveVariableDto>,
    scripts: Vec<SharedScriptDto>,
}

#[derive(Serialize)]
struct SharedScriptDto {
    name: String,
    pre_request: String,
    post_response: String,
    /// Names of the collections that include it anywhere in their tree.
    used_by: Vec<String>,
}

#[derive(Serialize)]
struct LiveVariableDto {
    scope: live::Scope,
    variable: VariableDto,
}

/// Persisted between launches. Kept as an object so a recent-workspaces list
/// can be added without another file format.
#[derive(Default, Serialize, Deserialize)]
struct AppConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
}

#[derive(Serialize)]
struct EnvironmentDto {
    name: String,
    main_slot: Option<String>,
    platforms: Vec<collection::model::Platform>,
    /// Shown in the UI next to the selector, so an unverified connection is
    /// never a surprise.
    insecure: bool,
    variables: Vec<VariableDto>,
}

#[derive(Serialize)]
struct CollectionDto {
    name: String,
    service_name: Option<String>,
    base_path: Option<String>,
    deployments: Vec<collection::model::Deployment>,
    slug: String,
    description: Option<String>,
    /// The collection's own setting, in the same shape the editor sends back.
    auth: AuthSetting,
    variables: Vec<VariableDto>,
    scripts: Scripts,
    items: Vec<TreeNode>,
}

/// A secret's value never crosses into the webview. The editor shows a
/// placeholder and sends the variable back without a value, which
/// [`store::save`] fills from disk.
#[derive(Serialize)]
struct VariableDto {
    name: String,
    value: Option<String>,
    secret: bool,
    has_value: bool,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TreeNode {
    Folder {
        name: String,
        path: Vec<usize>,
        /// What the folder sets itself, so the editor can change it.
        auth: AuthSetting,
        /// Variables the requests below it can read.
        variables: Vec<VariableDto>,
        scripts: Scripts,
        /// The method it inherits from above, if any, for the "inherits ..."
        /// hint. A method is never copied down onto the folder.
        inherited: Option<String>,
        items: Vec<TreeNode>,
    },
    Request {
        name: String,
        method: String,
        /// Base URL only. Enabled and disabled parameters are in `params`, and
        /// `:name` segments in the path have their values in `path_params`.
        url: String,
        path: Vec<usize>,
        auth: AuthSetting,
        /// Variables this request alone can read; the narrowest layer.
        variables: Vec<VariableDto>,
        inherited: Option<String>,
        params: Vec<KeyValue>,
        path_params: BTreeMap<String, String>,
        headers: Vec<KeyValue>,
        body: Option<Body>,
        scripts: Scripts,
        queries: BTreeMap<String, String>,
    },
}

#[derive(Serialize)]
struct ResponseDto {
    id: u64,
    environment: Option<String>,
    deployment: Option<collection::deployment::Target>,
    status: u16,
    elapsed_ms: u64,
    bytes: usize,
    content_type: Option<String>,
    url: String,
    insecure: bool,
    missing: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    script_errors: Vec<String>,
}

#[derive(Serialize)]
struct ProjectionDto {
    view_id: u64,
    text: String,
    /// Length of the projection as JSON, before pretty-printing for display.
    bytes: usize,
    elapsed_ms: u64,
    /// True when the raw body was cut to fit the view.
    truncated: bool,
    is_raw: bool,
}

#[derive(Serialize)]
struct PreviewDto {
    text: String,
    deployment: Option<collection::deployment::Target>,
    /// True when a secret was substituted with `***`, so the command as shown is
    /// not runnable until the value is supplied.
    masked: bool,
    /// Why the command would not run as-is, e.g. an unfilled `:path` parameter.
    problem: Option<String>,
}

#[derive(Serialize)]
struct SaveResultDto {
    workspace: WorkspaceDto,
    /// The collection's slug afterwards; a rename changes it.
    collection: String,
    written: usize,
    removed: usize,
}

/// `rq-desktop [workspace]`, then `RQ_WORKSPACE`, then whatever was open last
/// time, then the sample that ships with the repository.
#[tauri::command]
fn initial_workspace(app: tauri::AppHandle) -> Option<String> {
    if let Some(path) = std::env::args().nth(1) {
        return Some(path);
    }
    if let Ok(path) = std::env::var("RQ_WORKSPACE") {
        return Some(path);
    }
    // Only trust a remembered path that still looks like a workspace; a moved
    // or deleted folder should fall back instead of failing at launch.
    if let Some(path) = remembered_workspace(&app) {
        if path.join("collections").is_dir() {
            return Some(path.display().to_string());
        }
    }
    let sample = PathBuf::from("examples/jsonplaceholder");
    sample.is_dir().then(|| sample.display().to_string())
}

/// The native folder picker. Called from Rust rather than through the plugin's
/// JS API so the frontend needs no npm packages at all.
#[tauri::command]
fn pick_workspace(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;

    app.dialog()
        .file()
        .set_title("Open a workspace folder")
        .blocking_pick_folder()
        .and_then(|folder| folder.into_path().ok())
        .map(|path| path.display().to_string())
}

/// Where the app remembers things. Tauri resolves the per-OS config directory
/// (`%APPDATA%` on Windows, `~/Library/Application Support` on macOS,
/// `$XDG_CONFIG_HOME` on Linux), so this is not a Windows-only assumption.
fn config_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;

    app.path()
        .config_dir()
        .ok()
        .map(|dir| dir.join("rq").join("state.json"))
}

fn history(app: &tauri::AppHandle) -> Result<History, String> {
    use tauri::Manager;
    app.path()
        .app_data_dir()
        .map(|dir| History::new(dir.join("rq").join("history")))
        .map_err(|error| error.to_string())
}

struct ResponseContext {
    environment: String,
    deployment: Option<collection::deployment::Target>,
}

/// The parts of a history entry every send knows, sent or failed.
fn history_entry(
    name: &str,
    method: &str,
    collection: &str,
    path: &[usize],
    url: &str,
    context: &ResponseContext,
) -> history::Entry {
    history::Entry {
        environment: Some(context.environment.clone()),
        deployment: context.deployment.clone(),
        name: name.to_string(),
        collection: collection.to_string(),
        path: path.to_vec(),
        method: method.to_string(),
        url: url.to_string(),
        ..Default::default()
    }
}

#[tauri::command]
fn list_history(
    app: tauri::AppHandle,
    collection: Option<String>,
    path: Option<Vec<usize>>,
) -> Result<history::Listing, String> {
    Ok(history(&app)?.list(collection.as_deref(), path.as_deref()))
}

#[tauri::command]
fn set_history_limit(app: tauri::AppHandle, limit: u64) -> Result<history::Listing, String> {
    let history = history(&app)?;
    history.set_limit(limit)?;
    Ok(history.list(None, None))
}

#[tauri::command]
fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    history(&app)?.clear()
}

#[tauri::command]
fn open_history(
    id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<ResponseDto, String> {
    let (entry, body) = history(&app)?.open(&id)?;
    let mut responses = state.responses.lock().map_err(|_| "state poisoned")?;
    let response_id = responses.insert(body);
    Ok(ResponseDto {
        id: response_id,
        environment: entry.environment,
        deployment: entry.deployment,
        status: entry.status,
        elapsed_ms: entry.elapsed_ms,
        bytes: entry.bytes,
        content_type: entry.content_type,
        url: entry.url,
        insecure: entry.insecure,
        missing: entry.missing,
        history_id: Some(id),
        error: entry.error,
        script_errors: Vec::new(),
    })
}

fn remembered_workspace(app: &tauri::AppHandle) -> Option<PathBuf> {
    let bytes = std::fs::read(config_path(app)?).ok()?;
    let state: AppConfig = serde_json::from_slice(&bytes).ok()?;
    state.workspace.map(PathBuf::from)
}

/// Failing to remember a path must never block opening it, so this is
/// best-effort by design.
fn remember_workspace(app: &tauri::AppHandle, root: &std::path::Path) {
    let Some(config) = config_path(app) else {
        return;
    };
    let Some(parent) = config.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let state = AppConfig {
        workspace: Some(root.display().to_string()),
    };
    if let Ok(body) = serde_json::to_vec_pretty(&state) {
        let _ = std::fs::write(config, body);
    }
}

fn workspace_dto(root: &std::path::Path) -> Result<WorkspaceDto, String> {
    let loaded = store::load(root)?;
    let live_variables = live::entries(root)?
        .iter()
        .map(|entry| LiveVariableDto {
            scope: entry.scope.clone(),
            variable: variable_dto(&entry.variable),
        })
        .collect();

    let collections = loaded
        .collections
        .iter()
        .map(|collection| CollectionDto {
            name: collection.name.clone(),
            service_name: collection.service_name.clone(),
            base_path: collection.base_path.clone(),
            deployments: collection.deployments.clone(),
            slug: store::slugify(&collection.name),
            description: collection.description.clone(),
            auth: collection.auth.clone(),
            variables: collection.variables.iter().map(variable_dto).collect(),
            scripts: collection.scripts.clone(),
            items: tree(&collection.items, &mut Vec::new(), collection),
        })
        .collect();

    let environments = loaded
        .environments
        .iter()
        .map(|environment| EnvironmentDto {
            name: environment.name.clone(),
            main_slot: environment.main_slot.clone(),
            platforms: environment.platforms.clone(),
            insecure: environment.insecure,
            variables: environment.variables.iter().map(variable_dto).collect(),
        })
        .collect();

    Ok(WorkspaceDto {
        path: root.display().to_string(),
        collections,
        environments,
        globals: loaded.globals.iter().map(variable_dto).collect(),
        live_variables,
        scripts: loaded
            .scripts
            .iter()
            .map(|(name, scripts)| SharedScriptDto {
                name: name.clone(),
                pre_request: scripts.pre_request.clone(),
                post_response: scripts.post_response.clone(),
                used_by: loaded
                    .collections
                    .iter()
                    .filter(|collection| collection.includes(name))
                    .map(|collection| collection.name.clone())
                    .collect(),
            })
            .collect(),
    })
}

fn variable_dto(variable: &Variable) -> VariableDto {
    VariableDto {
        name: variable.name.clone(),
        // A secret's value stays in Rust. The window learns only that one is
        // set, and an edit that omits the value keeps what is on disk.
        value: if variable.secret {
            None
        } else {
            variable.value.clone()
        },
        secret: variable.secret,
        has_value: variable.value.is_some(),
    }
}

#[tauri::command]
fn open_workspace(
    path: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    // Store and report an absolute path: a remembered relative path would
    // resolve against whatever directory the next launch happens to use.
    let root = std::path::absolute(&path).unwrap_or_else(|_| PathBuf::from(&path));
    let dto = workspace_dto(&root)?;

    *state.workspace.lock().map_err(|_| "state poisoned")? = Some(root.clone());
    remember_workspace(&app, &root);

    Ok(dto)
}

#[tauri::command]
fn refresh_workspace(state: tauri::State<'_, AppState>) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    workspace_dto(&root)
}

#[derive(Serialize)]
struct ImportResultDto {
    workspace: WorkspaceDto,
    warnings: Vec<String>,
}

#[tauri::command]
fn import_postman(
    kind: String,
    secret_names: Vec<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<ImportResultDto>, String> {
    use tauri_plugin_dialog::DialogExt;

    let root = workspace_path(&state)?;
    let Some(file) = app
        .dialog()
        .file()
        .set_title("Import Postman JSON into workspace")
        .add_filter("Postman JSON", &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|error| error.to_string())?;
    let bytes =
        std::fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let warnings = store::import_postman(&root, &kind, &bytes, &secret_names)?;
    Ok(Some(ImportResultDto {
        workspace: workspace_dto(&root)?,
        warnings,
    }))
}

fn tree(items: &[Item], path: &mut Vec<usize>, collection: &Collection) -> Vec<TreeNode> {
    let mut out = Vec::new();

    for (index, item) in items.iter().enumerate() {
        path.push(index);
        // Everything above this item: the folder's or request's own setting is
        // sent separately so the editor can change it.
        let inherited = collection
            .inherited_auth_at(path)
            .map(|auth| auth.kind.clone());

        match &item.request {
            Some(request) => out.push(TreeNode::Request {
                name: item.name.clone(),
                method: request.method.clone(),
                url: request.url.clone(),
                path: path.clone(),
                auth: request.auth.clone(),
                variables: request.variables.iter().map(variable_dto).collect(),
                inherited,
                params: request.params.clone(),
                path_params: request.path_params.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
                scripts: request.scripts.clone(),
                queries: request.queries.clone(),
            }),
            None => out.push(TreeNode::Folder {
                name: item.name.clone(),
                path: path.clone(),
                auth: item.auth.clone(),
                variables: item.variables.iter().map(variable_dto).collect(),
                scripts: item.scripts.clone(),
                inherited,
                items: tree(&item.items, path, collection),
            }),
        }
        path.pop();
    }

    out
}

fn workspace_path(state: &tauri::State<'_, AppState>) -> Result<PathBuf, String> {
    state
        .workspace
        .lock()
        .map_err(|_| "state poisoned")?
        .clone()
        .ok_or_else(|| "no workspace is open".to_string())
}

/// Owned, so a command can build a request from either disk or the window's
/// draft without lifetimes tying it to the loaded workspace.
struct Located {
    collection: Collection,
    environment: Option<Environment>,
    /// `globals.yml`, the widest variable layer.
    globals: Vec<Variable>,
    /// `scripts/`, for includes.
    scripts: BTreeMap<String, Scripts>,
}

impl Located {
    fn context(
        &self,
        deployment: Option<&collection::model::Deployment>,
    ) -> Result<RequestContext<'_>, String> {
        RequestContext::new(
            &self.collection,
            self.environment.as_ref(),
            &self.globals,
            deployment,
        )
    }
}

fn locate(
    root: &std::path::Path,
    slug: &str,
    environment: Option<&str>,
) -> Result<Located, String> {
    let mut loaded = live::load(root)?;

    let index = loaded
        .collections
        .iter()
        .position(|candidate| store::slugify(&candidate.name) == slug)
        .ok_or_else(|| format!("no collection called `{slug}`"))?;

    let environment = match environment {
        Some(name) => Some(
            loaded
                .environment(name)
                .cloned()
                .ok_or_else(|| format!("no environment called `{name}`"))?,
        ),
        None => None,
    };

    Ok(Located {
        collection: loaded.collections.swap_remove(index),
        environment,
        globals: loaded.globals,
        scripts: loaded.scripts,
    })
}

/// The window sends what you see, but `description` comes from the stored
/// request: the editor does not own it.
fn request_for(
    draft: Option<RequestDraft>,
    resolved: &collection::model::ResolvedRequest,
) -> collection::model::RequestDef {
    match draft {
        Some(draft) => draft.onto(resolved.request),
        None => resolved.request.clone(),
    }
}

/// Why `send` produced no response. The window shows a cancellation quietly
/// and records only real failures in history.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
enum SendError {
    Cancelled,
    Failed(String),
}

impl From<String> for SendError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for SendError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_string())
    }
}

/// Forgets a request's cancel handle however `send` returns.
struct CancelHandle<'a> {
    state: &'a AppState,
    request_id: String,
}

impl Drop for CancelHandle<'_> {
    fn drop(&mut self) {
        if let Ok(mut cancellations) = self.state.cancellations.lock() {
            cancellations.remove(&self.request_id);
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn send(
    app: tauri::AppHandle,
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
    deployment: Option<collection::model::Deployment>,
    request_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResponseDto, SendError> {
    let root = workspace_path(&state)?;
    let response_root = root.clone();
    let response_environment = environment.clone().unwrap_or_default();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    state
        .cancellations
        .lock()
        .map_err(|_| "state poisoned")?
        .insert(request_id.clone(), cancel_tx);
    let _cancel_handle = CancelHandle {
        state: &state,
        request_id,
    };

    let prepared = tauri::async_runtime::spawn_blocking(move || {
        let located = locate(&root, &collection, environment.as_deref())?;
        let context = located.context(deployment.as_ref())?;
        let resolved = context
            .collection
            .request_at(&path)
            .ok_or("that request is no longer in the file")?;
        let name = resolved.name.to_string();
        let mut request = request_for(draft, &resolved);
        let auth = context.auth(&path, &request).cloned();
        let plan = rq_scripts::Plan::new(&context, &path, &request, &located.scripts)?;

        let mut variables: BTreeMap<_, _> = context
            .variables(&path)
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let writes = plan.pre_request(&mut request, &mut variables)?;
        persist_script_writes(&root, &collection, writes)?;
        // Script writes are current values now; read them back through the
        // same scope chain so a narrower saved value still wins.
        let updated = locate(&root, &collection, environment.as_deref())?;
        let variables: BTreeMap<_, _> = updated
            .context(deployment.as_ref())?
            .variables(&path)
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Ok::<_, String>(Prepared {
            name,
            target: context.target.clone(),
            collection,
            path,
            auth,
            options: collection::send::SendOptions {
                insecure: context.insecure(),
                timeout: Duration::from_secs(60),
            },
            request,
            plan,
            variables,
        })
    })
    .await
    .map_err(|error| format!("request task failed: {error}"))
    .and_then(|prepared| prepared);

    let sent = match prepared {
        Ok(prepared) => {
            let Prepared {
                name,
                target,
                collection: collection_slug,
                path: request_path,
                auth,
                options,
                request,
                plan,
                mut variables,
            } = prepared;
            let method = request.method.clone();
            let requested_url = request.url.clone();
            let vars = collection::send::Variables::from(variables.clone());
            let result = tokio::select! {
                _ = cancel_rx => Err(SendError::Cancelled),
                result = collection::send::send_async(&request, auth.as_ref(), &vars, &options) => result.map_err(SendError::Failed),
            };
            match result {
                Ok(sent) => {
                    let post_root = response_root.clone();
                    let post_collection = collection_slug.clone();
                    let post = tauri::async_runtime::spawn_blocking(move || {
                        if !plan.has_post_response() {
                            return (sent, Vec::new());
                        }
                        let mut request = request;
                        let body = String::from_utf8_lossy(&sent.body).to_string();
                        let (writes, mut script_errors) = plan.post_response(
                            &mut request,
                            &mut variables,
                            &rq_scripts::Response {
                                status: sent.status,
                                elapsed_ms: sent.elapsed.as_millis() as u64,
                                size: sent.body.len(),
                                body: &body,
                            },
                        );
                        if let Err(error) =
                            persist_script_writes(&post_root, &post_collection, writes)
                        {
                            script_errors.push(format!("script variable write failed: {error}"));
                        }
                        (sent, script_errors)
                    })
                    .await
                    .map_err(|error| format!("post-response script task failed: {error}"))?;
                    Ok((
                        name,
                        target,
                        collection_slug,
                        request_path,
                        method,
                        requested_url,
                        Ok(post),
                    ))
                }
                Err(error) => Ok((
                    name,
                    target,
                    collection_slug,
                    request_path,
                    method,
                    requested_url,
                    Err(error),
                )),
            }
        }
        Err(error) => Err(error),
    };
    let (name, target, collection_slug, request_path, method, requested_url, sent) = sent?;
    let response_context = ResponseContext {
        environment: response_environment,
        deployment: target,
    };
    let (sent, script_errors) = match sent {
        Ok((sent, errors)) => (sent, errors),
        Err(error) => {
            if let SendError::Failed(message) = &error {
                if let Ok(history) = history(&app) {
                    let mut entry = history_entry(
                        &name,
                        &method,
                        &collection_slug,
                        &request_path,
                        &requested_url,
                        &response_context,
                    );
                    entry.error = Some(message.clone());
                    history.record(entry, &[]);
                }
            }
            return Err(error);
        }
    };
    let history_id = history(&app).ok().and_then(|history| {
        let entry = history::Entry {
            status: sent.status,
            elapsed_ms: sent.elapsed.as_millis() as u64,
            bytes: sent.body.len(),
            content_type: sent.content_type.clone(),
            insecure: sent.insecure,
            missing: sent.missing.clone(),
            ..history_entry(
                &name,
                &method,
                &collection_slug,
                &request_path,
                &sent.url,
                &response_context,
            )
        };
        history.record(entry, &sent.body)
    });

    let mut responses = state.responses.lock().map_err(|_| "state poisoned")?;
    let bytes = sent.body.len();
    let id = responses.insert(sent.body);

    Ok(ResponseDto {
        id,
        environment: Some(response_context.environment),
        deployment: response_context.deployment,
        status: sent.status,
        elapsed_ms: sent.elapsed.as_millis() as u64,
        bytes,
        content_type: sent.content_type,
        url: sent.url,
        insecure: sent.insecure,
        missing: sent.missing,
        history_id,
        error: None,
        script_errors,
    })
}

#[tauri::command]
fn cancel_send(request_id: String, state: tauri::State<'_, AppState>) -> bool {
    state
        .cancellations
        .lock()
        .ok()
        .and_then(|mut cancellations| cancellations.remove(&request_id))
        .is_some_and(|cancel| cancel.send(()).is_ok())
}

#[tauri::command]
fn save(
    collection: String,
    edit: Edit,
    state: tauri::State<'_, AppState>,
) -> Result<SaveResultDto, String> {
    let root = workspace_path(&state)?;
    let outcome = store::save(&root, &collection, &edit)?;

    Ok(SaveResultDto {
        workspace: workspace_dto(&root)?,
        collection: outcome.collection,
        written: outcome.written,
        removed: outcome.removed,
    })
}

#[tauri::command]
fn create_collection(
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::create_collection(&root, &name)?;
    workspace_dto(&root)
}

#[derive(Serialize)]
struct CreatedDto {
    workspace: WorkspaceDto,
    path: Vec<usize>,
    warnings: Vec<String>,
}

/// Adds a request (optionally read from a cURL command) or, with `folder`,
/// an empty folder under `parent`.
#[tauri::command]
fn create_item(
    collection: String,
    parent: Vec<usize>,
    name: String,
    folder: bool,
    curl: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<CreatedDto, String> {
    let root = workspace_path(&state)?;
    let (request, warnings) = match curl.as_deref().map(str::trim) {
        _ if folder => (None, Vec::new()),
        Some(text) if !text.is_empty() => {
            let imported = collection::curl_import::parse(text)?;
            (Some(imported.request), imported.warnings)
        }
        _ => (
            Some(collection::model::RequestDef {
                method: "GET".to_string(),
                ..Default::default()
            }),
            Vec::new(),
        ),
    };
    let path = store::add_item(&root, &collection, &parent, &name, request)?;
    Ok(CreatedDto {
        workspace: workspace_dto(&root)?,
        path,
        warnings,
    })
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum TreeEdit {
    Rename {
        path: Vec<usize>,
        name: String,
    },
    Delete {
        path: Vec<usize>,
    },
    Duplicate {
        path: Vec<usize>,
    },
    Move {
        from: Vec<usize>,
        parent: Vec<usize>,
        index: usize,
    },
    RenameCollection {
        name: String,
    },
    DeleteCollection,
}

#[derive(Serialize)]
struct TreeEditDto {
    workspace: WorkspaceDto,
    /// The collection's slug afterwards; `None` once it is deleted.
    collection: Option<String>,
    /// Where a duplicated or moved item now is.
    path: Option<Vec<usize>>,
}

/// Structural edits from the tree's context menu and drag and drop.
#[tauri::command]
fn edit_tree(
    collection: String,
    edit: TreeEdit,
    state: tauri::State<'_, AppState>,
) -> Result<TreeEditDto, String> {
    let root = workspace_path(&state)?;
    let mut slug = Some(collection.clone());
    let path = match edit {
        TreeEdit::Rename { path, name } => {
            store::rename_item(&root, &collection, &path, &name)?;
            None
        }
        TreeEdit::Delete { path } => {
            store::delete_item(&root, &collection, &path)?;
            None
        }
        TreeEdit::Duplicate { path } => Some(store::duplicate_item(&root, &collection, &path)?),
        TreeEdit::Move {
            from,
            parent,
            index,
        } => Some(store::move_item(&root, &collection, &from, &parent, index)?),
        TreeEdit::RenameCollection { name } => {
            slug = Some(store::rename_collection(&root, &collection, &name)?);
            None
        }
        TreeEdit::DeleteCollection => {
            store::delete_collection(&root, &collection)?;
            slug = None;
            None
        }
    };
    Ok(TreeEditDto {
        workspace: workspace_dto(&root)?,
        collection: slug,
        path,
    })
}

#[derive(Serialize)]
struct SharedScriptSaved {
    workspace: WorkspaceDto,
    name: String,
}

/// Creates or updates `scripts/<name>.yml`. `previous` is the name being
/// edited; renaming updates every include of it.
#[tauri::command]
fn save_shared_script(
    previous: Option<String>,
    name: String,
    pre_request: String,
    post_response: String,
    state: tauri::State<'_, AppState>,
) -> Result<SharedScriptSaved, String> {
    let root = workspace_path(&state)?;
    let name = store::save_shared_script(
        &root,
        previous.as_deref(),
        &name,
        Scripts {
            pre_request,
            post_response,
            ..Default::default()
        },
    )?;
    Ok(SharedScriptSaved {
        workspace: workspace_dto(&root)?,
        name,
    })
}

#[tauri::command]
fn delete_shared_script(
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::delete_shared_script(&root, &name)?;
    workspace_dto(&root)
}

#[tauri::command]
fn create_environment(
    name: String,
    insecure: bool,
    variables: Vec<Variable>,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::create_environment(&root, name, insecure, variables)?;
    workspace_dto(&root)
}

#[tauri::command]
fn update_environment(
    old_name: String,
    name: String,
    insecure: bool,
    variables: Vec<Variable>,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::update_environment(&root, &store::slugify(&old_name), name, insecure, variables)?;
    workspace_dto(&root)
}

#[tauri::command]
fn delete_environment(
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::delete_environment(&root, &store::slugify(&name))?;
    workspace_dto(&root)
}

#[tauri::command]
fn update_globals(
    variables: Vec<Variable>,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let root = workspace_path(&state)?;
    store::save_globals(&root, variables)?;
    workspace_dto(&root)
}

/// Edits one variable from the hover menu in the window.
///
/// It edits the narrowest definition that binds the name, in the same order a
/// send uses: request, folder, collection, environment, globals. A name nothing
/// defines is created where the card asks — the collection by default.
/// The variable the hover card is applying locally. Grouped so the command keeps a
/// readable signature and the JSON payload names its fields.
#[derive(Deserialize)]
struct VariableEdit {
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    secret: bool,
    /// Where to create a name nothing defines: `collection` (the default),
    /// `environment`, or `globals`.
    #[serde(default)]
    create: Option<String>,
    #[serde(default)]
    reset: bool,
}

#[tauri::command]
fn set_variable(
    collection: String,
    path: Option<Vec<usize>>,
    environment: Option<String>,
    edit: VariableEdit,
    state: tauri::State<'_, AppState>,
) -> Result<WorkspaceDto, String> {
    let VariableEdit {
        name,
        value,
        secret,
        create,
        reset,
    } = edit;
    let root = workspace_path(&state)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("a variable needs a name".to_string());
    }
    let path = path.unwrap_or_default();

    let mut workspace = live::load(&root)?;
    let scope = live::target(
        &mut workspace,
        &collection,
        &path,
        environment.as_deref(),
        &name,
        create.as_deref(),
    )?;
    let mut variable = live::variables_mut(&mut workspace, &scope)
        .and_then(|variables| variables.iter().find(|variable| variable.name == name))
        .cloned()
        .unwrap_or(Variable {
            name,
            value: None,
            secret,
        });
    update_variable(&mut variable, value, secret);
    live::write(&root, vec![live::Entry { scope, variable }], reset)?;
    workspace_dto(&root)
}

/// A missing value means "leave it as it is" for a secret, because its value
/// never left Rust. For a visible variable an empty box is an empty string,
/// which arrives as `Some("")`.
fn update_variable(variable: &mut Variable, value: Option<String>, secret: bool) {
    if let Some(value) = value {
        variable.value = Some(value);
    }
    variable.secret = secret;
}

#[tauri::command]
fn curl_preview(
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
    deployment: Option<collection::model::Deployment>,
    state: tauri::State<'_, AppState>,
) -> Result<PreviewDto, String> {
    let root = workspace_path(&state)?;
    curl_for(
        &root,
        &collection,
        &path,
        draft,
        environment.as_deref(),
        deployment.as_ref(),
        false,
    )
}

/// Secret-bearing exports stay in Rust and use the same renderer as the preview.
fn curl_for(
    root: &std::path::Path,
    collection: &str,
    path: &[usize],
    draft: Option<RequestDraft>,
    environment: Option<&str>,
    deployment: Option<&collection::model::Deployment>,
    include_secrets: bool,
) -> Result<PreviewDto, String> {
    let located = locate(root, collection, environment)?;
    let context = located.context(deployment)?;
    let resolved = context
        .collection
        .request_at(path)
        .ok_or("that request is no longer in the file")?;
    let request = request_for(draft, &resolved);
    let auth = context.auth(path, &request);

    let variables = if include_secrets {
        context.variables(path)
    } else {
        context.masked_variables(path)
    };
    let preview = collection::curl::render(&request, auth, &variables);
    Ok(PreviewDto {
        text: preview.text,
        masked: preview.masked,
        problem: preview.problem,
        deployment: context.target,
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn copy_curl(
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
    deployment: Option<collection::model::Deployment>,
    include_secrets: bool,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let root = workspace_path(&state)?;
    let preview = curl_for(
        &root,
        &collection,
        &path,
        draft,
        environment.as_deref(),
        deployment.as_ref(),
        include_secrets,
    )?;
    app.clipboard()
        .write_text(preview.text)
        .map_err(|_| "Could not write to the system clipboard".to_string())
}

/// Captured response data is exported as-is, never variable-expanded. An empty
/// query preserves the original bytes, including bodies beyond the display cap.
#[cfg(test)]
fn response_export(body: &[u8], query: &str) -> Result<Vec<u8>, String> {
    response_export_with_variables(body, query, || Ok(BTreeMap::new().into()))
}

#[cfg(test)]
fn response_export_with_variables(
    body: &[u8],
    query: &str,
    load: impl FnOnce() -> Result<collection::send::Variables, String>,
) -> Result<Vec<u8>, String> {
    view::FrozenQuery::parse(query, load)?.export(body)
}

#[tauri::command]
fn save_response(
    id: u64,
    view_id: u64,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let bytes = {
        let responses = state.responses.lock().map_err(|_| "state poisoned")?;
        responses.export_view(id, view_id)?
    };
    let is_json = serde_json::from_slice::<serde::de::IgnoredAny>(&bytes).is_ok();
    let Some(file) = app
        .dialog()
        .file()
        .set_title("Save response — may contain sensitive data")
        .set_file_name(if is_json {
            "response.json"
        } else {
            "response.txt"
        })
        .add_filter("JSON", &["json"])
        .add_filter("Text", &["txt"])
        .add_filter("All files", &["*"])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = file.into_path().map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| format!("Could not save response: {error}"))?;
    Ok(true)
}

#[tauri::command]
fn project_body(
    id: u64,
    query: String,
    context: Option<query::Context>,
    state: tauri::State<'_, AppState>,
) -> Result<ProjectionDto, String> {
    let body = {
        let responses = state.responses.lock().map_err(|_| "state poisoned")?;
        responses
            .bodies
            .get(&id)
            .cloned()
            .ok_or("that response is no longer in memory; send it again")?
    };

    let frozen = view::FrozenQuery::parse(&query, || {
        let context = context
            .as_ref()
            .ok_or("select a request to resolve query variables")?;
        context.variables(&workspace_path(&state)?)
    })?;

    let mut projection = frozen.project(&body)?;
    projection.view_id = state
        .responses
        .lock()
        .map_err(|_| "state poisoned")?
        .remember_view(id, frozen)?;
    Ok(projection)
}

/// The no-spec path: infer a schema from what just came back, then write a
/// starter query from it. This is what makes spec-less APIs first-class.
#[tauri::command]
fn suggest_query(id: u64, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let body = {
        let responses = state.responses.lock().map_err(|_| "state poisoned")?;
        responses
            .bodies
            .get(&id)
            .cloned()
            .ok_or("that response is no longer in memory; send it again")?
    };

    let schema = infer(&[&body]).map_err(|diag| diag.message)?;
    Ok(scaffold(&schema, 2))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            if let Some(path) = config_path(app.handle()) {
                live::initialize(path.with_file_name("variables.json"));
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            initial_workspace,
            pick_workspace,
            open_workspace,
            refresh_workspace,
            import_postman,
            send,
            list_history,
            set_history_limit,
            clear_history,
            open_history,
            cancel_send,
            save,
            create_collection,
            create_item,
            edit_tree,
            save_shared_script,
            delete_shared_script,
            create_environment,
            update_environment,
            delete_environment,
            update_globals,
            set_variable,
            curl_preview,
            copy_curl,
            project_body,
            save_response,
            suggest_query
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod deployment_tests {
    use super::*;

    #[tokio::test]
    async fn both_transports_send_to_the_selected_target() {
        use std::io::{BufRead, Write};
        for (asynchronous, tenant) in [
            (false, "north"),
            (false, "south"),
            (true, "north"),
            (true, "south"),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let template = format!(
                "http://{}/{{{{serviceName}}}}{{{{basePath}}}}",
                listener.local_addr().unwrap()
            );
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                reader
                    .get_mut()
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .unwrap();
                request_line
            });
            let mut collection: Collection = serde_norway::from_str(
                "name: Navigation\nservice_name: navsvc\ndeployments:\n - {tenant: acme, platform: stage-a, slot: candidate}\nvariables:\n - {name: baseUrl, value: 'https://wrong.test'}",
            ).unwrap();
            collection.deployments[0].tenant = tenant.into();
            collection.base_path =
                Some("/{{tenant == 'north' ? serviceName : 'shared'}}-{{slot}}".into());
            let environment = collection::Environment {
                name: "stage".into(),
                main_slot: None,
                insecure: false,
                variables: Vec::new(),
                platforms: vec![collection::model::Platform {
                    name: "stage-a".into(),
                    url_template: template,
                }],
            };
            let target = collection::deployment::resolve(
                &collection,
                Some(&environment),
                Some(&collection.deployments[0]),
            )
            .unwrap();
            let variables = collection::send::variables_for_target(
                &collection,
                Some(&environment),
                &[],
                &[],
                target.as_ref(),
            );
            let request = collection::model::RequestDef {
                method: "GET".into(),
                url: "{{baseUrl}}/menu".into(),
                ..Default::default()
            };
            let options = collection::send::SendOptions::default();
            let sent = if asynchronous {
                collection::send::send_async(&request, None, &variables, &options)
                    .await
                    .unwrap()
            } else {
                collection::send::send(&request, None, &variables, &options).unwrap()
            };
            assert_eq!(sent.status, 200);
            assert_eq!(
                server.join().unwrap(),
                format!(
                    "GET /navsvc/{}-candidate/menu HTTP/1.1\r\n",
                    if tenant == "north" {
                        "navsvc"
                    } else {
                        "shared"
                    }
                )
            );
        }
    }

    #[test]
    fn curl_exports_require_a_valid_target_and_use_the_generated_destination() {
        let root = std::env::temp_dir().join("opencode").join(format!(
            "rq-target-export-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut collection: Collection = serde_norway::from_str(
            "name: Navigation\nservice_name: navsvc\ndeployments:\n - {tenant: acme, platform: stage-a, slot: candidate}\nvariables:\n - {name: baseUrl, value: 'https://wrong.test'}\n - {name: token, value: 'private-token', secret: true}\nitems:\n - name: Menu\n   request:\n     method: GET\n     url: '{{baseUrl}}/menu'\n     headers:\n      - {name: Authorization, value: 'Bearer {{token}}'}\n",
        ).unwrap();
        collection.base_path =
            Some("/{{tenant == 'acme' ? serviceName : 'shared'}}-{{slot}}/{{tenant}}".into());
        let environment: collection::Environment = serde_norway::from_str(
            "name: stage\nplatforms:\n - name: stage-a\n   url_template: 'https://{{serviceName}}.stage-a.test{{basePath}}'",
        ).unwrap();
        store::write(&root, &store::render(&collection, &[environment])).unwrap();
        let choice = &collection.deployments[0];
        assert!(curl_for(&root, "navigation", &[0], None, Some("stage"), None, false).is_err());
        assert!(curl_for(&root, "navigation", &[0], None, None, Some(choice), false).is_err());
        let protected = curl_for(
            &root,
            "navigation",
            &[0],
            None,
            Some("stage"),
            Some(choice),
            false,
        )
        .unwrap();
        let revealed = curl_for(
            &root,
            "navigation",
            &[0],
            None,
            Some("stage"),
            Some(choice),
            true,
        )
        .unwrap();
        for preview in [&protected, &revealed] {
            assert!(preview
                .text
                .contains("https://navsvc.stage-a.test/navsvc-candidate/acme/menu"));
            assert!(!preview.text.contains("wrong.test"));
            assert_eq!(
                preview.deployment.as_ref().unwrap().base_path,
                "/navsvc-candidate/acme"
            );
        }
        assert!(protected.masked);
        assert!(!protected.text.contains("private-token"));
        assert!(revealed.text.contains("private-token"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod request_body_tests {
    use super::*;
    use std::io::{BufRead, Read, Write};

    #[tokio::test]
    async fn json_comments_are_omitted_by_both_transports_and_curl() {
        let source =
            "{\n// {{unused}}\n\"url\":\"https://api.test//path\",\n\"id\":{{id}} // note\n}";
        let expected = "{\n\n\"url\":\"https://api.test//path\",\n\"id\":42 \n}";
        let vars = collection::send::Variables::from(BTreeMap::from([(
            "id".to_string(),
            "42".to_string(),
        )]));
        for asynchronous in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(stream);
                let mut length = None;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let mut body = vec![0; length.expect("content length")];
                reader.read_exact(&mut body).unwrap();
                reader
                    .get_mut()
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .unwrap();
                body
            });
            let request = collection::model::RequestDef {
                method: "POST".to_string(),
                url,
                body: Some(Body {
                    mode: "raw".to_string(),
                    language: Some("json".to_string()),
                    text: Some(source.to_string()),
                }),
                ..Default::default()
            };
            let preview = collection::curl::render(&request, None, &vars);
            assert!(preview.text.contains(expected));
            assert!(!preview.text.contains("unused"));
            assert!(!preview.text.contains("// note"));
            let options = collection::send::SendOptions::default();
            let response = if asynchronous {
                collection::send::send_async(&request, None, &vars, &options)
                    .await
                    .unwrap()
            } else {
                collection::send::send(&request, None, &vars, &options).unwrap()
            };
            assert_eq!(response.status, 200);
            assert!(response.missing.is_empty());
            assert_eq!(server.join().unwrap(), expected.as_bytes());
            assert_eq!(request.body.unwrap().text.as_deref(), Some(source));
        }
    }
}

#[cfg(test)]
mod response_export_tests {
    use super::*;

    #[test]
    fn empty_query_preserves_original_json_bytes() {
        let body = b" { \"id\": 42, \"title\": \"Example\" }\r\n";
        assert_eq!(response_export(body, " \n").unwrap(), body);
        assert_eq!(response_export(b"", "").unwrap(), b"");
    }

    #[test]
    fn export_is_not_truncated_or_lossy_for_non_json_bodies() {
        let mut body = vec![0xff; RAW_VIEW_LIMIT + 4096];
        body.extend_from_slice(b"\0last bytes\r\n");
        assert_eq!(response_export(&body, "").unwrap(), body);
    }

    #[test]
    fn query_exports_formatted_projection_not_viewer_decoration() {
        let body = br#"{"id":42,"title":"Example","hidden":"not selected"}"#;
        let exported = response_export(body, "{ id title }").unwrap();
        assert_eq!(
            String::from_utf8(exported).unwrap(),
            "{\n  \"id\": 42,\n  \"title\": \"Example\"\n}"
        );
        assert!(response_export(body, "not a query").is_err());
        assert!(response_export(body, "{ missing! }").is_err());
        assert_eq!(
            body,
            br#"{"id":42,"title":"Example","hidden":"not selected"}"#
        );
    }

    #[test]
    fn query_exports_apply_equality_and_inequality_without_modifying_the_body() {
        let body = br#"[{"id":1,"status":"ACTIVE"},{"id":2,"status":"DELETED"}]"#;
        for query in [
            r#"{ id status == "ACTIVE" }"#,
            r#"{ id status != "DELETED" }"#,
        ] {
            let exported = response_export(body, query).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
            assert_eq!(
                value,
                serde_json::json!([{"id":1,"status":"ACTIVE"},{"id":2}])
            );
        }
        assert_eq!(response_export(body, "").unwrap(), body.to_vec());
    }

    #[test]
    fn query_exports_apply_array_filters_and_membership_without_modifying_the_body() {
        let body = br#"{"profiles":[{"name":"Mike","age":30},{"name":"Anna","age":25},{"name":"X","age":40}]}"#;
        for (query, expected) in [
            (
                r#"{ profiles[? name in ["Mike", "X"]] { name age } }"#,
                serde_json::json!({"profiles":[{"name":"Mike","age":30},{"name":"X","age":40}]}),
            ),
            (
                r#"{ profiles[? name not in ["Mike", "X"]] { age } }"#,
                serde_json::json!({"profiles":[{"age":25}]}),
            ),
            (
                r#"{ profiles { name in ["Mike", "X"] age } }"#,
                serde_json::json!({"profiles":[{"name":"Mike","age":30},{"age":25},{"name":"X","age":40}]}),
            ),
        ] {
            let exported = response_export(body, query).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
            assert_eq!(value, expected);
        }
        assert_eq!(response_export(body, "").unwrap(), body.to_vec());
    }

    #[test]
    fn query_exports_filter_nested_values_before_projecting_other_fields() {
        let body = br#"{"items":[{"definition":{"item_id":{"value":"2762467"}},"label":"keep"},{"definition":{"item_id":{"value":"other"}},"label":"drop"}]}"#;
        let query = r#"{ items[? definition { item_id { value in ["2762467"] } }] { label } }"#;
        let exported = response_export(body, query).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(value, serde_json::json!({"items":[{"label":"keep"}]}));
        assert_eq!(response_export(body, "").unwrap(), body.to_vec());
    }

    #[test]
    fn query_variable_export_matches_projection_and_preserves_original_body_and_query() {
        let body = br#"{"items":[{"id":"2762467"},{"id":"other"}]}"#;
        let query = r#"{ items[? id == "{{myVar}}"] { id } }"#;
        let load = || Ok(BTreeMap::from([("myVar".into(), "2762467".into())]).into());
        let selection = query::parse(query, load).unwrap();
        let projected = project(body, &selection).unwrap();
        let exported = response_export_with_variables(body, query, load).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&projected).unwrap(),
            serde_json::from_slice::<serde_json::Value>(&exported).unwrap()
        );
        assert!(query.contains("{{myVar}}"));
        assert_eq!(
            response_export_with_variables(body, "", || panic!(
                "empty query must not load variables"
            ))
            .unwrap(),
            body.to_vec()
        );
    }

    #[test]
    fn recursive_filter_exports_keep_both_shapes_and_query_variable_binding() {
        let body = br#"{"items":[{"id":"other","children":[{"id":"2762467","hidden":1}]}]}"#;
        let load = || Ok(BTreeMap::from([("myVar".into(), "2762467".into())]).into());
        for (mode, expected) in [
            ("...", serde_json::json!({"items":[{"id":"2762467"}]})),
            (
                "**",
                serde_json::json!({"items":[{"children":[{"id":"2762467"}]}]}),
            ),
        ] {
            let query = format!("{{ items{mode}children[? id == \"{{{{myVar}}}}\"] {{ id }} }}");
            let exported = response_export_with_variables(body, &query, load).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&exported).unwrap(),
                expected
            );
        }
        assert_eq!(response_export(body, "").unwrap(), body.to_vec());
    }
}

#[cfg(test)]
mod response_environment_tests {
    use super::*;

    #[test]
    fn history_records_response_environment_and_accepts_older_entries() {
        let legacy = serde_json::json!({
            "id": "old", "timestamp": 1, "name": "Request", "method": "GET",
            "url": "https://api.test", "status": 200, "elapsed_ms": 1,
            "bytes": 2, "content_type": null, "insecure": false, "missing": []
        });
        let entry: history::Entry = serde_json::from_value(legacy).unwrap();
        assert_eq!(entry.environment, None);
        assert_eq!(entry.deployment, None);
        for environment in ["", "Dev", "Stage"] {
            let mut entry = entry.clone();
            entry.environment = Some(environment.to_string());
            entry.deployment = Some(collection::deployment::Target {
                base_path: String::new(),
                service_name: "navsvc".into(),
                environment: environment.into(),
                tenant: "acme".into(),
                platform: "stage-a".into(),
                slot: "candidate".into(),
                slot_suffix: "-candidate".into(),
                base_url: "https://navsvc-candidate.stage-a.test/acme".into(),
            });
            let restored: history::Entry =
                serde_json::from_slice(&serde_json::to_vec(&entry).unwrap()).unwrap();
            assert_eq!(restored.environment.as_deref(), Some(environment));
            assert_eq!(restored.deployment, entry.deployment);
        }
    }
}

#[cfg(test)]
mod curl_tests {
    use super::*;

    #[test]
    fn secret_export_is_explicit_and_leaves_the_preview_and_workspace_protected() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join("opencode")
            .join(format!("rq-curl-{}-{nonce}", std::process::id()));
        let parsed = collection::postman::parse_collection(
            br#"{
            "info":{"name":"Export"},
            "variable":[
                {"key":"token","value":"collection-secret-value","type":"secret"},
                {"key":"authorization","value":"Bearer {{token}}"}
            ],
            "auth":{"type":"bearer","bearer":[{"key":"token","value":"{{token}}"}]},
            "item":[{"name":"Saved","request":{"url":"{{baseUrl}}/saved","method":"GET"}}]
        }"#,
            &[],
        )
        .unwrap();
        let environment = collection::postman::parse_environment(
            br#"{
            "name":"Dev","values":[
                {"key":"baseUrl","value":"https://dev.test"},
                {"key":"token","value":"environment-secret-value","type":"secret"}
            ]
        }"#,
            &[],
        )
        .unwrap()
        .environment;
        store::write(&root, &store::render(&parsed.collection, &[environment])).unwrap();
        store::save_globals(
            &root,
            vec![Variable {
                name: "globalToken".to_string(),
                value: Some("global-secret-value".to_string()),
                secret: true,
            }],
        )
        .unwrap();
        let yaml = root.join("collections/export/01-saved.yml");
        let before = std::fs::read(&yaml).unwrap();
        let draft = || {
            serde_json::from_value(serde_json::json!({
                "name":"Draft", "method":"POST", "url":"{{baseUrl}}/draft",
                "headers":[{"name":"X-Composed","value":"{{authorization}}"}],
                "params":[{"name":"token","value":"{{token}}"}],
                "body":{"mode":"raw","language":"json","text":"{\"key\":\"{{globalToken}}\"}"}
            }))
            .unwrap()
        };
        let protected = curl_for(
            &root,
            "export",
            &[0],
            Some(draft()),
            Some("Dev"),
            None,
            false,
        )
        .unwrap();
        assert!(protected.masked);
        assert!(protected.text.contains("https://dev.test/draft"));
        assert!(!protected.text.contains("collection-secret-value"));
        assert!(!protected.text.contains("global-secret-value"));

        let revealed = curl_for(
            &root,
            "export",
            &[0],
            Some(draft()),
            Some("Dev"),
            None,
            true,
        )
        .unwrap();
        assert!(!revealed.masked);
        assert!(revealed.text.contains("curl -X POST"));
        assert!(revealed
            .text
            .contains("Authorization: Bearer collection-secret-value"));
        assert!(revealed
            .text
            .contains("X-Composed: Bearer collection-secret-value"));
        assert!(revealed.text.contains("token=collection-secret-value"));
        assert!(revealed.text.contains("global-secret-value"));
        assert!(!revealed.text.contains("environment-secret-value"));
        assert_eq!(
            curl_for(
                &root,
                "export",
                &[0],
                Some(draft()),
                Some("Dev"),
                None,
                false
            )
            .unwrap()
            .text,
            protected.text
        );
        assert_eq!(std::fs::read(&yaml).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
