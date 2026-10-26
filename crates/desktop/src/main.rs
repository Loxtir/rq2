//! The desktop shell.
//!
//! One rule shapes everything here: **the response body never crosses into the
//! webview**. `send` keeps the bytes in this process and returns metadata; the
//! frontend asks for a projection and gets back the projected text. That is the
//! same boundary the finished product needs for a 500k-line body, and it costs
//! nothing to hold to it now.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use boa_engine::native_function::NativeFunction;
use boa_engine::string::JsString;
use boa_engine::value::JsValue;
use boa_engine::{Context as JsContext, Source};
use collection::model::{
    AuthSetting, Body, Collection, Environment, Item, KeyValue, Scripts, Variable,
};
use collection::store::{self, Edit, RequestDraft};
use response_query::{infer, parse, project, scaffold};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Raw bodies larger than this are still kept, but the raw view is truncated so
/// a careless click does not push tens of megabytes through IPC.
const RAW_VIEW_LIMIT: usize = 512 * 1024;
/// Keep a handful of bodies around: enough to flip between two responses.
const MAX_KEPT_RESPONSES: usize = 24;
const DEFAULT_HISTORY_LIMIT: u64 = 250 * 1024 * 1024;

#[derive(Default)]
struct AppState {
    workspace: Mutex<Option<PathBuf>>,
    responses: Mutex<Responses>,
    cancellations: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
}

fn scripts_for_path(
    collection: &Collection,
    collection_slug: &str,
    path: &[usize],
) -> Vec<(String, Scripts)> {
    let mut scopes = vec![(
        format!("collection::{collection_slug}"),
        collection.scripts.clone(),
    )];
    let mut items = &collection.items;
    let mut prefix = Vec::new();
    for (depth, index) in path.iter().enumerate() {
        let Some(item) = items.get(*index) else { break };
        prefix.push(index.to_string());
        if depth + 1 == path.len() {
            if let Some(request) = &item.request {
                scopes.push((
                    format!("request::{collection_slug}::{}", prefix.join(".")),
                    request.scripts.clone(),
                ));
            } else {
                scopes.push((
                    format!("folder::{collection_slug}::{}", prefix.join(".")),
                    item.scripts.clone(),
                ));
            }
        } else {
            scopes.push((
                format!("folder::{collection_slug}::{}", prefix.join(".")),
                item.scripts.clone(),
            ));
        }
        items = &item.items;
    }
    scopes
}

fn apply_script(
    script: &str,
    request: &mut collection::model::RequestDef,
    variables: &mut BTreeMap<String, String>,
    scope: &str,
    scopes: &[String],
    insecure: bool,
    response: Option<(u16, u64, usize, &str)>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    if script.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut initial = serde_json::json!({
        "request": request,
        "variables": variables,
        "scope": scope,
        "scopes": scopes,
        "insecure": insecure,
        "writes": {},
        "response": response.map(|(code, time, size, body)| serde_json::json!({"code":code,"responseTime":time,"responseSize":size,"body":body})),
    });
    let initial_text = serde_json::to_string(&initial).map_err(|e| e.to_string())?;
    let initial_literal = serde_json::to_string(&initial_text).map_err(|e| e.to_string())?;
    let js = format!(
        r#"
      globalThis.__rqExecution = (async function() {{
        const __rq = JSON.parse({initial_literal});
        const stringify = (v) => typeof v === 'string' ? v : JSON.stringify(v);
        const set = (name, value, target) => {{
          name = String(name); value = stringify(value);
          target = target || __rq.scope;
          if (typeof target === 'object') target = target.scope;
          if (target === 'globals') target = 'global';
          if (target === 'environment') target = __rq.scopes.find(s => s.startsWith('environment::')) || 'environment';
          if (target === 'collection') target = __rq.scopes.find(s => s.startsWith('collection::')) || 'collection';
          if (target === 'folder') target = [...__rq.scopes].reverse().find(s => s.startsWith('folder::')) || __rq.scope;
          if (target === 'request') target = __rq.scopes.find(s => s.startsWith('request::')) || __rq.scope;
          if (target === 'global') target = __rq.scopes.find(s => s === 'global' || s.startsWith('global::')) || 'global';
          __rq.writes[target] ||= {{}}; __rq.writes[target][name] = value; __rq.variables[name] = value;
        }};
        const get = (name) => __rq.variables[String(name)];
        const headerApi = __rq.request.headers || (__rq.request.headers = []);
        headerApi.add = (h) => headerApi.push({{name:h.key || h.name, value:String(h.value ?? '')}});
        headerApi.upsert = (h) => {{ const k=h.key||h.name; const found=headerApi.find(x=>String(x.name).toLowerCase()===String(k).toLowerCase()); if(found) found.value=String(h.value??''); else headerApi.add(h); }};
        const body = __rq.request.body || (__rq.request.body = {{mode:'raw',language:'json',text:''}});
        const bodyApi = {{ get raw() {{return body.text || '';}}, set raw(v) {{body.text=String(v); body.mode='raw';}} }};
        const sendRequest = (spec, callback) => {{
          let sent;
          try {{
            sent = JSON.parse(__rqSendRequest(JSON.stringify({{request:spec,variables:__rq.variables,insecure:__rq.insecure}})));
            if (sent.__error) throw new Error(sent.__error);
          }} catch (error) {{
            if (typeof callback === 'function') {{ callback(String(error),undefined); return Promise.resolve(); }}
            throw error;
          }}
          const response = {{code:sent.code,status:sent.status,responseTime:sent.responseTime,responseSize:sent.responseSize,headers:sent.headers || {{}},text:()=>sent.body,json:()=>JSON.parse(sent.body)}};
          if (typeof callback === 'function') {{ callback(null,response); return Promise.resolve(); }}
          return Promise.resolve(response);
        }};
        const pm = {{
          variables: {{ get, set }},
          globals: {{ get, set:(n,v)=>set(n,v,'global') }},
          environment: {{ get, set:(n,v)=>set(n,v,'environment') }},
          collectionVariables: {{ get, set:(n,v)=>set(n,v,'collection') }},
          sendRequest,
          request: {{ get method(){{return __rq.request.method}}, set method(v){{__rq.request.method=String(v)}}, get url(){{return __rq.request.url}}, set url(v){{__rq.request.url=String(v)}}, headers:headerApi, body:bodyApi }},
          response: __rq.response ? {{ code:__rq.response.code, status:{{get:()=>__rq.response.code}}, responseTime:__rq.response.responseTime, responseSize:__rq.response.responseSize, text:()=>__rq.response.body, json:()=>JSON.parse(__rq.response.body) }} : undefined,
          test:()=>{{}}, expect:(v)=>({{to:{{equal:(x)=>{{if(v!==x) throw new Error('Expected '+v+' to equal '+x)}}}}}}),
        }};
        {script}
        delete __rq.response;
        return JSON.stringify(__rq);
      }})();
      globalThis.__rqExecution.then(v => globalThis.__rqResult = v, e => globalThis.__rqError = String(e));
    "#
    );
    let mut context = JsContext::default();
    context
        .register_global_callable(
            JsString::from("__rqSendRequest"),
            1,
            NativeFunction::from_copy_closure(|_, args, context| {
                let payload = args
                    .first()
                    .cloned()
                    .unwrap_or_default()
                    .to_string(context)
                    .map_err(|error| error)?
                    .to_std_string_escaped();
                let result = send_script_request(&payload)
                    .unwrap_or_else(|error| serde_json::json!({"__error":error}));
                let text = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
                Ok(JsValue::from(JsString::from(text)))
            }),
        )
        .map_err(|error| error.to_string())?;
    context
        .eval(Source::from_bytes(js.as_bytes()))
        .map_err(|e| e.to_string())?;
    context.run_jobs().map_err(|error| error.to_string())?;
    let value = context
        .eval(Source::from_bytes(
            b"JSON.stringify({result:globalThis.__rqResult,error:globalThis.__rqError})",
        ))
        .map_err(|e| e.to_string())?;
    let output = value
        .to_string(&mut context)
        .map_err(|e| e.to_string())?
        .to_std_string_escaped();
    let output: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| format!("script result: {e}"))?;
    if let Some(error) = output["error"].as_str() {
        return Err(error.to_string());
    }
    initial = serde_json::from_str(output["result"].as_str().ok_or("script did not complete")?)
        .map_err(|e| format!("script result: {e}"))?;
    *request = serde_json::from_value(initial["request"].clone()).map_err(|e| e.to_string())?;
    *variables = serde_json::from_value(initial["variables"].clone()).map_err(|e| e.to_string())?;
    serde_json::from_value(initial["writes"].clone()).map_err(|e| e.to_string())
}

fn send_script_request(payload: &str) -> Result<serde_json::Value, String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let spec = &payload["request"];
    let url = spec
        .as_str()
        .or_else(|| spec["url"].as_str())
        .or_else(|| spec["url"]["raw"].as_str())
        .unwrap_or_default();
    let mut request = collection::model::RequestDef {
        method: spec["method"].as_str().unwrap_or("GET").to_uppercase(),
        url: url.to_string(),
        ..Default::default()
    };
    if let Some(headers) = spec["header"]
        .as_array()
        .or_else(|| spec["headers"].as_array())
    {
        request.headers = headers
            .iter()
            .map(|header| KeyValue {
                name: header["key"]
                    .as_str()
                    .or_else(|| header["name"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                value: header["value"].as_str().unwrap_or_default().to_string(),
                disabled: header["disabled"].as_bool().unwrap_or(false),
            })
            .collect();
    }
    if let Some(body) = spec.get("body") {
        let text = body["raw"].as_str().or_else(|| body["text"].as_str());
        if let Some(text) = text {
            request.body = Some(Body {
                mode: "raw".into(),
                language: Some("json".into()),
                text: Some(text.into()),
            });
        }
    }
    request.normalize_query();
    let variables: BTreeMap<String, String> =
        serde_json::from_value(payload["variables"].clone()).unwrap_or_default();
    let vars = collection::send::Variables::from(variables);
    let insecure = payload["insecure"].as_bool().unwrap_or(false);
    let sent = collection::send::send(
        &request,
        None,
        &vars,
        &collection::send::SendOptions {
            insecure,
            timeout: Duration::from_secs(60),
        },
    )?;
    Ok(serde_json::json!({
        "code": sent.status,
        "status": sent.status.to_string(),
        "responseTime": sent.elapsed.as_millis() as u64,
        "responseSize": sent.body.len(),
        "body": String::from_utf8_lossy(&sent.body),
        "headers": {},
    }))
}

fn apply_variable_writes(variables: &mut Vec<Variable>, writes: BTreeMap<String, String>) {
    for (name, value) in writes {
        if let Some(variable) = variables.iter_mut().find(|variable| variable.name == name) {
            variable.value = Some(value);
        } else {
            variables.push(Variable {
                name,
                value: Some(value),
                secret: false,
            });
        }
    }
}

fn variables_at_path(collection: &Collection, path: &[usize]) -> Option<Vec<Variable>> {
    let (last, parents) = path.split_last()?;
    let mut items = &collection.items;
    for index in parents {
        items = &items.get(*index)?.items;
    }
    Some(item_variables(items.get(*last)?).to_vec())
}

/// Persist script writes through the collection store so secrets remain split
/// into `.secrets/` and every edit follows the renderer's leak checks.
fn persist_script_writes(
    root: &std::path::Path,
    collection_slug: &str,
    writes: BTreeMap<String, BTreeMap<String, String>>,
) -> Result<(), String> {
    if writes.is_empty() {
        return Ok(());
    }
    let root_key = root.display().to_string();
    let collection_key = format!("{root_key}::{collection_slug}");
    let mut workspace = store::load(root)?;

    for (scope, values) in writes {
        if scope == format!("global::{root_key}") {
            apply_variable_writes(&mut workspace.globals, values);
            store::save_globals(root, workspace.globals.clone())?;
            continue;
        }

        let environment_prefix = format!("environment::{root_key}::");
        if let Some(slug) = scope.strip_prefix(&environment_prefix) {
            let environment = workspace
                .environments
                .iter()
                .find(|environment| store::slugify(&environment.name) == slug)
                .ok_or_else(|| {
                    "pm.environment.set requires a selected, saved environment".to_string()
                })?;
            let name = environment.name.clone();
            let insecure = environment.insecure;
            let mut variables = environment.variables.clone();
            apply_variable_writes(&mut variables, values);
            store::update_environment(root, slug, name, insecure, variables)?;
            continue;
        }

        let collection_prefix = format!("collection::{collection_key}");
        if scope == collection_prefix {
            let collection = workspace
                .collections
                .iter()
                .find(|collection| store::slugify(&collection.name) == collection_slug)
                .ok_or_else(|| "script collection scope no longer exists".to_string())?;
            let mut variables = collection.variables.clone();
            apply_variable_writes(&mut variables, values);
            let edit = Edit::Collection {
                name: collection.name.clone(),
                description: collection.description.clone(),
                variables,
                auth: collection.auth.clone(),
                scripts: collection.scripts.clone(),
            };
            store::save(root, collection_slug, &edit)?;
            continue;
        }

        let collection = workspace
            .collections
            .iter()
            .find(|collection| store::slugify(&collection.name) == collection_slug)
            .ok_or_else(|| "script collection scope no longer exists".to_string())?;
        let mut matched_path = None;
        for kind in ["folder", "request"] {
            let prefix = format!("{kind}::{collection_key}::");
            if let Some(path) = scope.strip_prefix(&prefix) {
                let path = path
                    .split('.')
                    .map(|part| {
                        part.parse::<usize>()
                            .map_err(|_| "invalid script variable scope".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                matched_path = Some(path);
                break;
            }
        }
        let path =
            matched_path.ok_or_else(|| format!("unsupported script variable scope: {scope}"))?;
        let mut variables = variables_at_path(collection, &path)
            .ok_or_else(|| "script variable target no longer exists".to_string())?;
        apply_variable_writes(&mut variables, values);
        store::save(root, collection_slug, &Edit::Variables { path, variables })?;
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryEntry {
    id: String,
    timestamp: u64,
    name: String,
    #[serde(default)]
    collection: String,
    #[serde(default)]
    path: Vec<usize>,
    method: String,
    url: String,
    status: u16,
    elapsed_ms: u64,
    bytes: usize,
    content_type: Option<String>,
    insecure: bool,
    missing: Vec<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    response_available: bool,
}

#[derive(Serialize)]
struct HistoryDto {
    entries: Vec<HistoryEntry>,
    bytes: u64,
    limit: u64,
}

#[derive(Default)]
struct Responses {
    next_id: u64,
    bodies: BTreeMap<u64, Vec<u8>>,
}

impl Responses {
    fn insert(&mut self, body: Vec<u8>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.bodies.insert(id, body);
        while self.bodies.len() > MAX_KEPT_RESPONSES {
            let oldest = *self.bodies.keys().next().expect("non-empty");
            self.bodies.remove(&oldest);
        }
        id
    }
}

#[derive(Serialize)]
struct WorkspaceDto {
    path: String,
    collections: Vec<CollectionDto>,
    environments: Vec<EnvironmentDto>,
    /// `globals.yml`: the widest variable layer, shared by every collection.
    globals: Vec<VariableDto>,
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
    /// Shown in the UI next to the selector, so an unverified connection is
    /// never a surprise.
    insecure: bool,
    variables: Vec<VariableDto>,
}

#[derive(Serialize)]
struct CollectionDto {
    name: String,
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
    },
}

#[derive(Serialize)]
struct ResponseDto {
    id: u64,
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

fn history_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("rq").join("history"))
        .map_err(|error| error.to_string())
}

fn history_limit(dir: &std::path::Path) -> u64 {
    std::fs::read(dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<u64>(&bytes).ok())
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
}

fn read_history(dir: &std::path::Path) -> Vec<HistoryEntry> {
    let Ok(files) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = files
        .flatten()
        .filter(|item| item.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|item| std::fs::read(item.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<HistoryEntry>(&bytes).ok())
        .collect::<Vec<_>>();
    for entry in &mut entries {
        entry.response_available = dir.join(format!("{}.body", entry.id)).is_file();
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| b.id.cmp(&a.id)));
    entries
}

fn history_entry_bytes(dir: &std::path::Path, entry: &HistoryEntry) -> u64 {
    std::fs::metadata(dir.join(format!("{}.body", entry.id)))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn prune_history(dir: &std::path::Path, limit: u64) -> Result<u64, String> {
    let mut entries = read_history(dir);
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
    let mut total = entries.iter().fold(0u64, |sum, entry| {
        sum.saturating_add(history_entry_bytes(dir, entry))
    });
    for mut entry in entries {
        if total <= limit {
            break;
        }
        let entry_bytes = history_entry_bytes(dir, &entry);
        let body = dir.join(format!("{}.body", entry.id));
        if entry_bytes == 0 {
            continue;
        }
        if std::fs::remove_file(&body).is_err() {
            continue;
        }
        entry.response_available = false;
        if let Ok(metadata) = serde_json::to_vec(&entry) {
            let _ = std::fs::write(dir.join(format!("{}.json", entry.id)), metadata);
        }
        total = total.saturating_sub(entry_bytes);
    }
    let known = read_history(dir)
        .into_iter()
        .map(|entry| entry.id)
        .collect::<std::collections::HashSet<_>>();
    if let Ok(files) = std::fs::read_dir(dir) {
        for file in files.flatten() {
            let path = file.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "body")
            {
                if let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) {
                    if !known.contains(id) {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
    }
    Ok(total)
}

fn persist_response(
    app: &tauri::AppHandle,
    name: &str,
    method: &str,
    collection: &str,
    path: &[usize],
    sent: &collection::send::SentResponse,
) -> Option<String> {
    let dir = history_dir(app).ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    let id = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos(),
        std::process::id()
    );
    let entry = HistoryEntry {
        id: id.clone(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
        name: name.to_string(),
        collection: collection.to_string(),
        path: path.to_vec(),
        method: method.to_string(),
        url: sent.url.clone(),
        status: sent.status,
        elapsed_ms: sent.elapsed.as_millis() as u64,
        bytes: sent.body.len(),
        content_type: sent.content_type.clone(),
        insecure: sent.insecure,
        missing: sent.missing.clone(),
        error: None,
        response_available: true,
    };
    std::fs::write(dir.join(format!("{id}.body")), &sent.body).ok()?;
    let metadata = serde_json::to_vec(&entry).ok()?;
    if std::fs::write(dir.join(format!("{id}.json")), metadata).is_err() {
        let _ = std::fs::remove_file(dir.join(format!("{id}.body")));
        return None;
    }
    let _ = prune_history(&dir, history_limit(&dir));
    Some(id)
}

fn persist_failed_request(
    app: &tauri::AppHandle,
    name: &str,
    method: &str,
    collection: &str,
    path: &[usize],
    url: &str,
    error: &str,
) {
    let Some(dir) = history_dir(app).ok() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(timestamp) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    let id = format!("{}-{}", timestamp.as_nanos(), std::process::id());
    let entry = HistoryEntry {
        id: id.clone(),
        timestamp: timestamp.as_secs(),
        name: name.to_string(),
        collection: collection.to_string(),
        path: path.to_vec(),
        method: method.to_string(),
        url: url.to_string(),
        status: 0,
        elapsed_ms: 0,
        bytes: 0,
        content_type: None,
        insecure: false,
        missing: Vec::new(),
        error: Some(error.to_string()),
        response_available: false,
    };
    if std::fs::write(dir.join(format!("{id}.body")), []).is_err() {
        return;
    }
    if let Ok(metadata) = serde_json::to_vec(&entry) {
        if std::fs::write(dir.join(format!("{id}.json")), metadata).is_ok() {
            let _ = prune_history(&dir, history_limit(&dir));
        }
    }
}

#[tauri::command]
fn list_history(
    app: tauri::AppHandle,
    collection: Option<String>,
    path: Option<Vec<usize>>,
) -> Result<HistoryDto, String> {
    let dir = history_dir(&app)?;
    let entries = read_history(&dir)
        .into_iter()
        .filter(|entry| {
            collection
                .as_ref()
                .is_none_or(|slug| &entry.collection == slug)
        })
        .filter(|entry| path.as_ref().is_none_or(|path| &entry.path == path))
        .collect::<Vec<_>>();
    let bytes = entries.iter().fold(0u64, |sum, entry| {
        sum.saturating_add(history_entry_bytes(&dir, entry))
    });
    Ok(HistoryDto {
        entries,
        bytes,
        limit: history_limit(&dir),
    })
}

#[tauri::command]
fn set_history_limit(app: tauri::AppHandle, limit: u64) -> Result<HistoryDto, String> {
    let dir = history_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    std::fs::write(
        dir.join("settings.json"),
        serde_json::to_vec(&limit).unwrap(),
    )
    .map_err(|error| error.to_string())?;
    prune_history(&dir, limit)?;
    list_history(app, None, None)
}

#[tauri::command]
fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    let dir = history_dir(&app)?;
    if dir.exists() {
        for item in std::fs::read_dir(&dir)
            .map_err(|error| error.to_string())?
            .flatten()
        {
            if item.file_name() != "settings.json" {
                std::fs::remove_file(item.path()).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn open_history(
    id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<ResponseDto, String> {
    let dir = history_dir(&app)?;
    let metadata = std::fs::read(dir.join(format!("{id}.json")))
        .map_err(|_| "history entry was removed".to_string())?;
    let entry: HistoryEntry =
        serde_json::from_slice(&metadata).map_err(|error| error.to_string())?;
    let body = match std::fs::read(dir.join(format!("{id}.body"))) {
        Ok(body) => body,
        Err(_) if entry.error.is_some() => Vec::new(),
        Err(_) => return Err("response body was removed by history retention".to_string()),
    };
    let mut responses = state.responses.lock().map_err(|_| "state poisoned")?;
    let response_id = responses.insert(body);
    Ok(ResponseDto {
        id: response_id,
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

    let collections = loaded
        .collections
        .iter()
        .map(|collection| CollectionDto {
            name: collection.name.clone(),
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
            insecure: environment.insecure,
            variables: environment.variables.iter().map(variable_dto).collect(),
        })
        .collect();

    Ok(WorkspaceDto {
        path: root.display().to_string(),
        collections,
        environments,
        globals: loaded.globals.iter().map(variable_dto).collect(),
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
}

fn locate(
    root: &std::path::Path,
    slug: &str,
    environment: Option<&str>,
) -> Result<Located, String> {
    let loaded = store::load(root)?;

    let collection = loaded
        .collections
        .into_iter()
        .find(|candidate| store::slugify(&candidate.name) == slug)
        .ok_or_else(|| format!("no collection called `{slug}`"))?;

    let environment = match environment {
        Some(name) => Some(
            loaded
                .environments
                .into_iter()
                .find(|candidate| candidate.name == name)
                .ok_or_else(|| format!("no environment called `{name}`"))?,
        ),
        None => None,
    };

    Ok(Located {
        collection,
        environment,
        globals: loaded.globals,
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

/// The auth that applies to `request` at `path`: its own setting (which the
/// editor may have just changed) resolved against everything above it. Using
/// the stored `resolved.auth` after a draft edit would send the old method.
fn auth_for<'a>(
    collection: &'a Collection,
    path: &[usize],
    request: &'a collection::model::RequestDef,
) -> Option<&'a collection::model::Auth> {
    request.auth.resolve(collection.inherited_auth_at(path))
}

#[tauri::command]
async fn send(
    app: tauri::AppHandle,
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
    request_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ResponseDto, String> {
    let root = workspace_path(&state)?;
    let response_root = root.clone();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    state
        .cancellations
        .lock()
        .map_err(|_| "state poisoned")?
        .insert(request_id.clone(), cancel_tx);

    let runtime_namespace = root.display().to_string();
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        let collection_slug = collection.clone();
        let request_path = path.clone();
        let located = locate(&root, &collection, environment.as_deref())?;
        let collection = &located.collection;
        let resolved = collection
            .request_at(&path)
            .ok_or("that request is no longer in the file")?;
        let name = resolved.name.to_string();
        let mut request = request_for(draft, &resolved);
        let auth = auth_for(collection, &path, &request).cloned();
        let insecure = located
            .environment
            .as_ref()
            .is_some_and(|environment| environment.insecure);

        let base_vars = collection::send::variables(
            collection,
            located.environment.as_ref(),
            &located.globals,
            &path,
        );
        let mut variable_map = base_vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<BTreeMap<_, _>>();
        let scoped_collection = format!("{runtime_namespace}::{collection_slug}");
        let mut script_layers = scripts_for_path(collection, &scoped_collection, &path);
        if let Some((key, scripts)) = script_layers.last_mut() {
            if key.starts_with("request::") {
                *scripts = request.scripts.clone();
            }
        }
        let mut scope_keys = vec![
            format!("global::{runtime_namespace}"),
            format!(
                "environment::{}::{}",
                runtime_namespace,
                located
                    .environment
                    .as_ref()
                    .map(|e| store::slugify(&e.name))
                    .unwrap_or_else(|| "default".into())
            ),
        ];
        scope_keys.extend(script_layers.iter().map(|(key, _)| key.clone()));
        let mut runtime_writes = BTreeMap::<String, BTreeMap<String, String>>::new();
        for (scope, scripts) in &script_layers {
            let writes = apply_script(
                &scripts.pre_request,
                &mut request,
                &mut variable_map,
                scope,
                &scope_keys,
                insecure,
                None,
            )?;
            for (target, values) in writes {
                runtime_writes.entry(target).or_default().extend(values);
            }
        }
        persist_script_writes(&root, &collection_slug, runtime_writes)?;
        let updated = locate(&root, &collection_slug, environment.as_deref())?;
        let effective = collection::send::variables(
            &updated.collection,
            updated.environment.as_ref(),
            &updated.globals,
            &request_path,
        );
        variable_map = effective
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let vars = collection::send::Variables::from(variable_map.clone());
        Ok::<_, String>((
            name,
            collection_slug,
            request_path,
            request.method.clone(),
            request,
            auth,
            vars,
            script_layers,
            scope_keys,
            variable_map,
            collection::send::SendOptions {
                insecure,
                timeout: Duration::from_secs(60),
            },
        ))
    })
    .await
    .map_err(|error| format!("request task failed: {error}"));

    let sent = match prepared {
        Ok(Ok((
            name,
            collection_slug,
            request_path,
            method,
            request,
            auth,
            vars,
            script_layers,
            scope_keys,
            mut variable_map,
            options,
        ))) => {
            let requested_url = request.url.clone();
            let result = tokio::select! {
                _ = cancel_rx => Err("Request cancelled".to_string()),
                result = collection::send::send_async(&request, auth.as_ref(), &vars, &options) => result.map_err(|e| e.to_string()),
            };
            match result {
                Ok(sent) => {
                    let post_root = response_root.clone();
                    let post_collection = collection_slug.clone();
                    let post = tauri::async_runtime::spawn_blocking(move || {
                        let mut writes_all = BTreeMap::<String, BTreeMap<String, String>>::new();
                        let mut script_errors = Vec::new();
                        let mut post_request = request;
                        let body = if script_layers
                            .iter()
                            .any(|(_, scripts)| !scripts.post_response.trim().is_empty())
                        {
                            String::from_utf8_lossy(&sent.body).to_string()
                        } else {
                            String::new()
                        };
                        for (scope, scripts) in &script_layers {
                            match apply_script(
                                &scripts.post_response,
                                &mut post_request,
                                &mut variable_map,
                                scope,
                                &scope_keys,
                                options.insecure,
                                Some((
                                    sent.status,
                                    sent.elapsed.as_millis() as u64,
                                    sent.body.len(),
                                    &body,
                                )),
                            ) {
                                Ok(writes) => {
                                    for (target, values) in writes {
                                        writes_all.entry(target).or_default().extend(values);
                                    }
                                }
                                Err(error) => script_errors.push(format!("{scope}: {error}")),
                            }
                        }
                        if let Err(error) =
                            persist_script_writes(&post_root, &post_collection, writes_all)
                        {
                            script_errors.push(format!("script variable write failed: {error}"));
                        }
                        (sent, script_errors)
                    })
                    .await
                    .map_err(|error| format!("post-response script task failed: {error}"))?;
                    Ok((
                        name,
                        collection_slug,
                        request_path,
                        method,
                        requested_url,
                        Ok((post.0, post.1)),
                    ))
                }
                Err(error) => Ok((
                    name,
                    collection_slug,
                    request_path,
                    method,
                    requested_url,
                    Err(error),
                )),
            }
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(error),
    };
    state
        .cancellations
        .lock()
        .map_err(|_| "state poisoned")?
        .remove(&request_id);
    let (name, collection_slug, request_path, method, requested_url, sent) = sent?;
    let (sent, script_errors) = match sent {
        Ok((sent, errors)) => (sent, errors),
        Err(error) => {
            if error != "Request cancelled" {
                persist_failed_request(
                    &app,
                    &name,
                    &method,
                    &collection_slug,
                    &request_path,
                    &requested_url,
                    &error,
                );
            }
            return Err(error);
        }
    };
    let history_id = persist_response(&app, &name, &method, &collection_slug, &request_path, &sent);

    let mut responses = state.responses.lock().map_err(|_| "state poisoned")?;
    let bytes = sent.body.len();
    let id = responses.insert(sent.body);

    Ok(ResponseDto {
        id,
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
/// The variable the hover card is writing. Grouped so the command keeps a
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
    } = edit;
    let root = workspace_path(&state)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("a variable needs a name".to_string());
    }
    let path = path.unwrap_or_default();

    let mut workspace = store::load(&root)?;
    let index = workspace
        .collections
        .iter()
        .position(|candidate| store::slugify(&candidate.name) == collection)
        .ok_or_else(|| format!("no collection called `{collection}`"))?;
    let slug = store::slugify(&workspace.collections[index].name);

    // 1. The item at `path`, then each folder above it, innermost first.
    if !path.is_empty() {
        let mut targets = vec![path.clone()];
        targets.extend((1..path.len()).rev().map(|end| path[..end].to_vec()));

        for target in targets {
            let Some(item) = item_at(&workspace.collections[index].items, &target) else {
                continue;
            };
            if !item_variables(item)
                .iter()
                .any(|variable| variable.name == name)
            {
                continue;
            }

            let mut variables = item_variables(item).to_vec();
            if let Some(variable) = variables.iter_mut().find(|variable| variable.name == name) {
                update_variable(variable, value.clone(), secret);
            }
            store::save(
                &root,
                &slug,
                &Edit::Variables {
                    path: target,
                    variables,
                },
            )?;
            return workspace_dto(&root);
        }
    }

    // 2. The collection's own variables.
    if workspace.collections[index]
        .variables
        .iter()
        .any(|variable| variable.name == name)
    {
        let target = &mut workspace.collections[index];
        if let Some(variable) = target
            .variables
            .iter_mut()
            .find(|variable| variable.name == name)
        {
            update_variable(variable, value.clone(), secret);
        }
        save_collection(&root, target)?;
        return workspace_dto(&root);
    }

    // 3. The selected environment.
    if let Some(environment_name) = environment.as_deref() {
        if let Some(target) = workspace
            .environments
            .iter_mut()
            .find(|candidate| candidate.name == environment_name)
        {
            if target
                .variables
                .iter()
                .any(|variable| variable.name == name)
            {
                if let Some(variable) = target
                    .variables
                    .iter_mut()
                    .find(|variable| variable.name == name)
                {
                    update_variable(variable, value.clone(), secret);
                }
                let slug = store::slugify(&target.name);
                let variables = target.variables.clone();
                store::save_environment(&root, &slug, variables)?;
                return workspace_dto(&root);
            }
        }
    }

    // 4. Globals, the widest layer.
    if workspace
        .globals
        .iter()
        .any(|variable| variable.name == name)
    {
        if let Some(variable) = workspace
            .globals
            .iter_mut()
            .find(|variable| variable.name == name)
        {
            update_variable(variable, value.clone(), secret);
        }
        store::save_globals(&root, workspace.globals.clone())?;
        return workspace_dto(&root);
    }

    // Nothing defines it: create it where the card asked.
    let variable = Variable {
        name,
        value,
        secret,
    };
    match create.as_deref().unwrap_or("collection") {
        "globals" => {
            workspace.globals.push(variable);
            store::save_globals(&root, workspace.globals.clone())?;
        }
        "environment" => {
            let Some(environment_name) = environment.as_deref() else {
                return Err("no environment is selected".to_string());
            };
            let target = workspace
                .environments
                .iter_mut()
                .find(|candidate| candidate.name == environment_name)
                .ok_or_else(|| format!("no environment called `{environment_name}`"))?;
            target.variables.push(variable);
            let slug = store::slugify(&target.name);
            let variables = target.variables.clone();
            store::save_environment(&root, &slug, variables)?;
        }
        _ => {
            workspace.collections[index].variables.push(variable);
            save_collection(&root, &workspace.collections[index])?;
        }
    }

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

fn item_at<'a>(items: &'a [Item], path: &[usize]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let item = items.get(*first)?;
    if rest.is_empty() {
        Some(item)
    } else {
        item_at(&item.items, rest)
    }
}

/// A folder's variables, or a request's.
fn item_variables(item: &Item) -> &[Variable] {
    match &item.request {
        Some(request) => &request.variables,
        None => &item.variables,
    }
}

/// Saves a collection's variables without touching its tree; the collection is
/// reloaded from disk, so only the fields the edit owns are carried over.
fn save_collection(root: &std::path::Path, collection: &Collection) -> Result<(), String> {
    let slug = store::slugify(&collection.name);
    let edit = Edit::Collection {
        name: collection.name.clone(),
        description: collection.description.clone(),
        variables: collection.variables.clone(),
        auth: collection.auth.clone(),
        scripts: collection.scripts.clone(),
    };
    store::save(root, &slug, &edit).map(|_| ())
}

#[tauri::command]
fn curl_preview(
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<PreviewDto, String> {
    let root = workspace_path(&state)?;
    let preview = curl_for(
        &root,
        &collection,
        &path,
        draft,
        environment.as_deref(),
        false,
    )?;
    Ok(PreviewDto {
        text: preview.text,
        masked: preview.masked,
        problem: preview.problem,
    })
}

/// Secret-bearing exports stay in Rust and use the same renderer as the preview.
fn curl_for(
    root: &std::path::Path,
    collection: &str,
    path: &[usize],
    draft: Option<RequestDraft>,
    environment: Option<&str>,
    include_secrets: bool,
) -> Result<collection::curl::Preview, String> {
    let located = locate(root, collection, environment)?;
    let collection = &located.collection;
    let resolved = collection
        .request_at(path)
        .ok_or("that request is no longer in the file")?;
    let request = request_for(draft, &resolved);
    let auth = auth_for(collection, path, &request);

    let variables = if include_secrets {
        collection::send::variables(
            collection,
            located.environment.as_ref(),
            &located.globals,
            path,
        )
    } else {
        collection::curl::masked_variables(
            collection,
            located.environment.as_ref(),
            &located.globals,
            path,
        )
    };
    Ok(collection::curl::render(&request, auth, &variables))
}

#[tauri::command]
fn copy_curl(
    collection: String,
    path: Vec<usize>,
    draft: Option<RequestDraft>,
    environment: Option<String>,
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
        include_secrets,
    )?;
    app.clipboard()
        .write_text(preview.text)
        .map_err(|_| "Could not write to the system clipboard".to_string())
}

#[tauri::command]
fn project_body(
    id: u64,
    query: String,
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

    if query.trim().is_empty() {
        let started = std::time::Instant::now();
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
            let text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
            return Ok(ProjectionDto {
                text,
                bytes: body.len(),
                elapsed_ms: started.elapsed().as_micros() as u64 / 1000,
                truncated: false,
                is_raw: false,
            });
        }
        let truncated = body.len() > RAW_VIEW_LIMIT;
        let slice = &body[..body.len().min(RAW_VIEW_LIMIT)];
        return Ok(ProjectionDto {
            text: String::from_utf8_lossy(slice).to_string(),
            bytes: body.len(),
            elapsed_ms: 0,
            truncated,
            is_raw: true,
        });
    }

    let selection = parse(&query).map_err(|diag| diag.render(&query))?;

    let started = std::time::Instant::now();
    let projected = project(&body, &selection).map_err(|diag| diag.render(&query))?;
    let elapsed = started.elapsed();

    let bytes = projected.len();
    let text = match serde_json::from_slice::<serde_json::Value>(&projected) {
        Ok(value) => serde_json::to_string_pretty(&value)
            .unwrap_or_else(|_| String::from_utf8_lossy(&projected).to_string()),
        Err(_) => String::from_utf8_lossy(&projected).to_string(),
    };

    Ok(ProjectionDto {
        text,
        bytes,
        elapsed_ms: elapsed.as_micros() as u64 / 1000,
        truncated: false,
        is_raw: false,
    })
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
            create_environment,
            update_environment,
            delete_environment,
            update_globals,
            set_variable,
            curl_preview,
            copy_curl,
            project_body,
            suggest_query
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
        let protected = curl_for(&root, "export", &[0], Some(draft()), Some("Dev"), false).unwrap();
        assert!(protected.masked);
        assert!(protected.text.contains("https://dev.test/draft"));
        assert!(!protected.text.contains("collection-secret-value"));
        assert!(!protected.text.contains("global-secret-value"));

        let revealed = curl_for(&root, "export", &[0], Some(draft()), Some("Dev"), true).unwrap();
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
            curl_for(&root, "export", &[0], Some(draft()), Some("Dev"), false)
                .unwrap()
                .text,
            protected.text
        );
        assert_eq!(std::fs::read(&yaml).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
