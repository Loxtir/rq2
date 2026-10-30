//! Postman-style `pm.*` scripts, run in an embedded JavaScript engine.
//!
//! A [`Plan`] holds the script layers that apply to one request (collection,
//! then each folder, then the request) and runs them in order. Variable writes
//! come back as typed [`Scope`]s; turning them into per-user current values is
//! [`edits`]. Scripts never touch the workspace files.

use boa_engine::native_function::NativeFunction;
use boa_engine::string::JsString;
use boa_engine::value::JsValue;
use boa_engine::{Context as JsContext, Source};
use collection::context::RequestContext;
use collection::live::{self, Entry, Scope};
use collection::model::{Body, KeyValue, RequestDef, Scripts, SharedScript, Variable};
use collection::store::{self, Workspace};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// Variable writes in the order scripts made them; a later one for the same
/// scope and name wins.
pub type Writes = Vec<(Scope, BTreeMap<String, String>)>;

/// What a post-response script sees as `pm.response`.
pub struct Response<'a> {
    pub status: u16,
    pub elapsed_ms: u64,
    pub size: usize,
    pub body: &'a str,
}

/// One script run: `key` is the scope it writes to, and `shared` names the
/// shared script it came from, for error messages.
struct Layer {
    key: String,
    shared: Option<String>,
    scripts: Scripts,
}

impl Layer {
    fn blame(&self, error: String) -> String {
        match &self.shared {
            Some(name) => format!("shared script `{name}`: {error}"),
            None => error,
        }
    }

    fn label(&self) -> &str {
        self.shared.as_deref().unwrap_or(&self.key)
    }
}

pub struct Plan {
    /// From the collection down to the request, each layer's included shared
    /// scripts just before its own.
    layers: Vec<Layer>,
    /// Every key a script may target, widest first. The keys are private to
    /// the engine; `scopes` maps them back to typed scopes.
    keys: Vec<String>,
    scopes: Vec<(String, Scope)>,
    insecure: bool,
    read_only: &'static [&'static str],
    /// `console.*` output since the last [`Plan::take_logs`], kept even when a
    /// script fails.
    logs: Mutex<Vec<String>>,
}

impl Plan {
    /// The layers for the request at `path`. `request` is what will be sent,
    /// so an editor's unsaved scripts are the ones that run. `shared` is the
    /// workspace's `scripts/`; including one that does not exist is an error.
    pub fn new(
        context: &RequestContext,
        path: &[usize],
        request: &RequestDef,
        shared: &BTreeMap<String, SharedScript>,
    ) -> Result<Self, String> {
        let collection = context.collection;
        let slug = store::slugify(&collection.name);
        let mut scopes = vec![("global".to_string(), Scope::Globals)];
        if let Some(environment) = context.environment {
            scopes.push((
                format!("environment::{}", environment.name),
                Scope::Environment {
                    environment: environment.name.clone(),
                },
            ));
        }
        let mut layers = vec![(
            "collection::".to_string() + &slug,
            collection.scripts.clone(),
        )];
        scopes.push((
            layers[0].0.clone(),
            Scope::Collection {
                collection: slug.clone(),
            },
        ));

        let mut items = &collection.items;
        for (depth, index) in path.iter().enumerate() {
            let Some(item) = items.get(*index) else { break };
            let prefix = path[..=depth]
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(".");
            let (key, scripts) = match &item.request {
                Some(_) if depth + 1 == path.len() => {
                    (format!("request::{prefix}"), request.scripts.clone())
                }
                _ => (format!("folder::{prefix}"), item.scripts.clone()),
            };
            scopes.push((
                key.clone(),
                Scope::Item {
                    collection: slug.clone(),
                    path: path[..=depth].to_vec(),
                },
            ));
            layers.push((key, scripts));
            items = &item.items;
        }

        let mut expanded = Vec::new();
        for (key, scripts) in layers {
            let phases = [
                (&scripts.include.pre_request, true),
                (&scripts.include.post_response, false),
            ];
            for (ids, pre) in phases {
                for id in ids {
                    let included = shared.get(id).ok_or_else(|| {
                        format!("`{key}` includes shared script `{id}`, but no scripts/*.js has `// @id {id}`")
                    })?;
                    let code = included.code.clone();
                    expanded.push(Layer {
                        key: key.clone(),
                        shared: Some(included.name.clone()),
                        scripts: if pre {
                            Scripts {
                                pre_request: code,
                                ..Default::default()
                            }
                        } else {
                            Scripts {
                                post_response: code,
                                ..Default::default()
                            }
                        },
                    });
                }
            }
            expanded.push(Layer {
                key,
                shared: None,
                scripts,
            });
        }

        Ok(Self {
            keys: scopes.iter().map(|(key, _)| key.clone()).collect(),
            layers: expanded,
            scopes,
            insecure: context.insecure(),
            read_only: if context.target.is_some() {
                &collection::deployment::RESERVED
            } else {
                &[]
            },
            logs: Mutex::default(),
        })
    }

    /// Script console lines, each prefixed with its phase and source.
    pub fn take_logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|mut logs| std::mem::take(&mut *logs))
            .unwrap_or_default()
    }

    fn log(&self, phase: &str, layer: &Layer, lines: Vec<String>) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.extend(
                lines
                    .into_iter()
                    .map(|line| format!("[{phase} · {}] {line}", layer.label())),
            );
        }
    }

    pub fn has_post_response(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| !layer.scripts.post_response.trim().is_empty())
    }

    /// Runs every pre-request script; the first failure stops the send.
    pub fn pre_request(
        &self,
        request: &mut RequestDef,
        variables: &mut BTreeMap<String, String>,
    ) -> Result<Writes, String> {
        let mut writes = Vec::new();
        for layer in &self.layers {
            let mut logs = Vec::new();
            let raw = run(
                &layer.scripts.pre_request,
                request,
                variables,
                &layer.key,
                self,
                None,
                &mut logs,
            );
            self.log("pre-request", layer, logs);
            let raw = raw.map_err(|error| layer.blame(error))?;
            writes.extend(self.typed(raw).map_err(|error| layer.blame(error))?);
        }
        Ok(writes)
    }

    /// Runs every post-response script. The response already arrived, so a
    /// failing layer is reported and the rest still run.
    pub fn post_response(
        &self,
        request: &mut RequestDef,
        variables: &mut BTreeMap<String, String>,
        response: &Response,
    ) -> (Writes, Vec<String>) {
        let mut writes = Vec::new();
        let mut errors = Vec::new();
        for layer in &self.layers {
            let mut logs = Vec::new();
            let raw = run(
                &layer.scripts.post_response,
                request,
                variables,
                &layer.key,
                self,
                Some(response),
                &mut logs,
            );
            self.log("post-response", layer, logs);
            match raw.and_then(|raw| self.typed(raw)) {
                Ok(typed) => writes.extend(typed),
                Err(error) => errors.push(format!("{}: {}", layer.key, layer.blame(error))),
            }
        }
        (writes, errors)
    }

    fn typed(&self, raw: BTreeMap<String, BTreeMap<String, String>>) -> Result<Writes, String> {
        raw.into_iter()
            .map(|(key, values)| {
                let scope = self
                    .scopes
                    .iter()
                    .find(|(candidate, _)| *candidate == key)
                    .map(|(_, scope)| scope.clone())
                    .ok_or_else(|| {
                        if key == "environment" {
                            "pm.environment.set requires a selected, saved environment".into()
                        } else {
                            format!("unsupported script variable scope: {key}")
                        }
                    })?;
                Ok((scope, values))
            })
            .collect()
    }
}

/// Script writes as current values, keeping each variable's secret flag so a
/// refreshed token stays masked.
pub fn edits(
    workspace: &mut Workspace,
    collection: &str,
    writes: Writes,
) -> Result<Vec<Entry>, String> {
    if workspace
        .collection(collection)
        .is_some_and(collection::deployment::enabled)
        && writes.iter().any(|(_, values)| {
            values
                .keys()
                .any(|name| collection::deployment::RESERVED.contains(&name.as_str()))
        })
    {
        return Err("deployment template inputs and baseUrl are read-only".into());
    }
    let mut edits = Vec::new();
    for (scope, values) in writes {
        let existing: &[Variable] = live::variables_mut(workspace, &scope)
            .map(|variables| &**variables)
            .ok_or("script variable target no longer exists")?;
        for (name, value) in values {
            let secret = existing
                .iter()
                .any(|variable| variable.name == name && variable.secret);
            edits.push(Entry {
                scope: scope.clone(),
                variable: Variable {
                    name,
                    value: Some(value),
                    secret,
                },
            });
        }
    }
    Ok(edits)
}

fn run(
    script: &str,
    request: &mut RequestDef,
    variables: &mut BTreeMap<String, String>,
    scope: &str,
    plan: &Plan,
    response: Option<&Response>,
    logs: &mut Vec<String>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    if script.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    // The engine needs more stack than a main thread has on Windows, so every
    // script gets its own, whoever calls.
    std::thread::scope(|threads| {
        std::thread::Builder::new()
            .name("rq-script".into())
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(threads, || {
                run_here(script, request, variables, scope, plan, response, logs)
            })
            .map_err(|error| format!("starting the script engine: {error}"))?
            .join()
            .map_err(|_| "the script engine crashed".to_string())?
    })
}

fn run_here(
    script: &str,
    request: &mut RequestDef,
    variables: &mut BTreeMap<String, String>,
    scope: &str,
    plan: &Plan,
    response: Option<&Response>,
    logs: &mut Vec<String>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let mut initial = serde_json::json!({
        "request": request,
        "variables": variables,
        "scope": scope,
        "scopes": plan.keys,
        "insecure": plan.insecure,
        "writes": {},
        "readOnly": plan.read_only,
        "response": response.map(|response| serde_json::json!({
            "code": response.status,
            "responseTime": response.elapsed_ms,
            "responseSize": response.size,
            "body": response.body,
        })),
    });
    let initial_text = serde_json::to_string(&initial).map_err(|e| e.to_string())?;
    let initial_literal = serde_json::to_string(&initial_text).map_err(|e| e.to_string())?;
    let js = format!(
        r#"
      globalThis.__rqLogs = [];
      globalThis.console = (() => {{
        const show = (v) => {{
          if (typeof v === 'string') return v;
          if (v instanceof Error) return String(v);
          try {{ const s = JSON.stringify(v); return s === undefined ? String(v) : s; }} catch (_) {{ return String(v); }}
        }};
        const write = (level) => (...args) => globalThis.__rqLogs.push((level ? level + ': ' : '') + args.map(show).join(' '));
        return {{ log: write(''), info: write(''), debug: write(''), warn: write('warn'), error: write('error') }};
      }})();
      if (typeof globalThis.btoa !== 'function') {{
        const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
        globalThis.btoa = (input) => {{
          const s = String(input);
          let out = '';
          for (let i = 0; i < s.length; i += 3) {{
            const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
            if (a > 255 || b > 255 || c > 255) throw new Error('btoa: string contains characters outside Latin-1');
            const n = (a << 16) | ((b || 0) << 8) | (c || 0);
            out += chars[n >> 18 & 63] + chars[n >> 12 & 63]
              + (i + 1 < s.length ? chars[n >> 6 & 63] : '=')
              + (i + 2 < s.length ? chars[n & 63] : '=');
          }}
          return out;
        }};
        globalThis.atob = (input) => {{
          const s = String(input).replace(/[\s=]/g, '');
          if (/[^A-Za-z0-9+/]/.test(s) || s.length % 4 === 1) throw new Error('atob: invalid base64');
          let out = '';
          for (let i = 0; i < s.length; i += 4) {{
            const n = (chars.indexOf(s[i]) << 18) | (chars.indexOf(s[i + 1]) << 12)
              | ((i + 2 < s.length ? chars.indexOf(s[i + 2]) : 0) << 6)
              | (i + 3 < s.length ? chars.indexOf(s[i + 3]) : 0);
            out += String.fromCharCode(n >> 16 & 255);
            if (i + 2 < s.length) out += String.fromCharCode(n >> 8 & 255);
            if (i + 3 < s.length) out += String.fromCharCode(n & 255);
          }}
          return out;
        }};
      }}
      globalThis.__rqExecution = (async function() {{
        const __rq = JSON.parse({initial_literal});
        const stringify = (v) => typeof v === 'string' ? v : JSON.stringify(v);
        const set = (name, value, target) => {{
          name = String(name); value = stringify(value);
          if (__rq.readOnly.includes(name)) throw new Error(name + ' is read-only and comes from the selected deployment');
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
                    .to_string(context)?
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
            b"JSON.stringify({result:globalThis.__rqResult,error:globalThis.__rqError,logs:globalThis.__rqLogs})",
        ))
        .map_err(|e| e.to_string())?;
    let output = value
        .to_string(&mut context)
        .map_err(|e| e.to_string())?
        .to_std_string_escaped();
    let output: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| format!("script result: {e}"))?;
    if let Some(lines) = output["logs"].as_array() {
        logs.extend(
            lines
                .iter()
                .filter_map(|line| line.as_str().map(String::from)),
        );
    }
    if let Some(error) = output["error"].as_str() {
        return Err(error.to_string());
    }
    initial = serde_json::from_str(output["result"].as_str().ok_or("script did not complete")?)
        .map_err(|e| format!("script result: {e}"))?;
    *request = serde_json::from_value(initial["request"].clone()).map_err(|e| e.to_string())?;
    *variables = serde_json::from_value(initial["variables"].clone()).map_err(|e| e.to_string())?;
    serde_json::from_value(initial["writes"].clone()).map_err(|e| e.to_string())
}

/// `pm.sendRequest`: accepts a URL string or a Postman request object.
fn send_script_request(payload: &str) -> Result<serde_json::Value, String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let request = script_request(&payload["request"]);
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

/// The request a `pm.sendRequest` spec describes.
fn script_request(spec: &serde_json::Value) -> RequestDef {
    let url = spec
        .as_str()
        .or_else(|| spec["url"].as_str())
        .or_else(|| spec["url"]["raw"].as_str())
        .unwrap_or_default();
    let mut request = RequestDef {
        method: spec["method"].as_str().unwrap_or("GET").to_uppercase(),
        url: url.to_string(),
        ..Default::default()
    };
    let header_spec = spec.get("header").or_else(|| spec.get("headers"));
    if let Some(headers) = header_spec.and_then(|headers| headers.as_array()) {
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
    } else if let Some(headers) = header_spec.and_then(|headers| headers.as_object()) {
        // Postman also accepts `{ "Name": "value" }`.
        request.headers = headers
            .iter()
            .map(|(name, value)| KeyValue {
                name: name.clone(),
                value: value
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| value.to_string()),
                disabled: false,
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
    request
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_request_accepts_header_arrays_and_objects() {
        let array = script_request(&serde_json::json!({
            "url": "https://x.test/a", "header": [{"key": "Authorization", "value": "Basic abc"}]
        }));
        let object = script_request(&serde_json::json!({
            "url": "https://x.test/a", "method": "post", "header": {"Authorization": "Basic abc"}
        }));
        assert_eq!(array.headers, object.headers);
        assert_eq!(object.headers[0].name, "Authorization");
        assert_eq!(object.headers[0].value, "Basic abc");
        assert_eq!(object.method, "POST");
    }

    #[test]
    fn scripts_cannot_assign_generated_target_inputs() {
        for name in collection::deployment::RESERVED {
            let plan = Plan {
                layers: vec![Layer {
                    key: "collection::test".into(),
                    shared: None,
                    scripts: Scripts {
                        pre_request: format!("pm.variables.set('{name}', 'wrong');"),
                        ..Default::default()
                    },
                }],
                keys: vec!["collection::test".into()],
                scopes: vec![(
                    "collection::test".into(),
                    Scope::Collection {
                        collection: "test".into(),
                    },
                )],
                insecure: false,
                read_only: &collection::deployment::RESERVED,
                logs: Mutex::default(),
            };
            let mut variables = BTreeMap::new();
            let error = plan
                .pre_request(&mut RequestDef::default(), &mut variables)
                .unwrap_err();
            assert!(error.contains("read-only"), "{error}");
            assert!(!variables.contains_key(name));
        }
    }
}
