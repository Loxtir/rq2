//! Issuing a stored request.
//!
//! Kept out of the CLI so the desktop shell and the CLI send requests the same
//! way. Nothing here knows about files: it takes a request, the resolved auth,
//! and a variable map.

use crate::model::{Auth, Collection, Environment, RequestDef, Variable};
use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

/// Bodies above this are refused rather than swapped to disk. A 500k-line
/// response is ~10 MB, so this is a runaway guard, not a policy.
pub const MAX_BODY: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Skip certificate verification. See [`Environment::insecure`].
    pub insecure: bool,
    pub timeout: Duration,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            insecure: false,
            timeout: Duration::from_secs(30),
        }
    }
}

pub struct SentResponse {
    pub status: u16,
    pub elapsed: Duration,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    /// The URL after variables and auth were applied, i.e. what was actually
    /// requested. Surfaced so a stale `{{baseUrl}}` is visible immediately.
    pub url: String,
    /// Echoed back so the UI can say so next to the status code.
    pub insecure: bool,
    /// Variable names the request asked for that nothing defined. They were
    /// left in place rather than blanked, so the failure is visible in the URL
    /// instead of silently hitting a different endpoint.
    pub missing: Vec<String>,
}

/// Merged, expanded variable values plus the loops found on the way.
///
/// Derefs to the map, so a caller that only wants lookups can treat it as one.
pub struct Variables {
    values: BTreeMap<String, String>,
    /// Loops and chains too deep to expand, worded for a person. A loop leaves
    /// its token in place, which also reports it as unresolved.
    pub problems: Vec<String>,
}

impl std::ops::Deref for Variables {
    type Target = BTreeMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl From<BTreeMap<String, String>> for Variables {
    fn from(values: BTreeMap<String, String>) -> Self {
        Self {
            values,
            problems: Vec::new(),
        }
    }
}

/// A value nested deeper than this is treated as a loop: real configuration
/// nests a handful of levels, and the guard keeps a hostile file from spending
/// time walking an exponential chain.
const MAX_CHAIN: usize = 32;

/// Values in send order: `globals -> environment -> collection -> folders ->
/// request`, where a later layer overrides an earlier one.
///
/// A value may itself contain `{{name}}`, so a collection can declare
/// `baseUrl: '{{origin}}/v1'` and let the environment or `globals.yml` supply
/// `origin`. Expansion resolves those references against the same merged map.
pub fn variables(
    collection: &Collection,
    environment: Option<&Environment>,
    globals: &[Variable],
    path: &[usize],
) -> Variables {
    expand(raw_variables(collection, environment, globals, path))
}

/// The merged layers before expansion, so the cURL renderer can replace a
/// secret's value with the mask *before* anything can compose it into text.
pub(crate) fn raw_variables(
    collection: &Collection,
    environment: Option<&Environment>,
    globals: &[Variable],
    path: &[usize],
) -> BTreeMap<String, String> {
    fn absorb(vars: &mut BTreeMap<String, String>, variables: &[Variable]) {
        for variable in variables {
            if let Some(value) = &variable.value {
                vars.insert(variable.name.clone(), value.clone());
            }
        }
    }

    let mut vars = BTreeMap::new();

    absorb(&mut vars, globals);
    if let Some(environment) = environment {
        absorb(&mut vars, &environment.variables);
    }
    for layer in collection.variable_layers(path) {
        absorb(&mut vars, layer);
    }

    vars
}

pub(crate) fn expand(raw: BTreeMap<String, String>) -> Variables {
    let mut problems = Vec::new();
    let values = raw
        .keys()
        .map(|name| {
            let value = expand_name(&raw, name, &mut Vec::new(), &mut problems)
                .unwrap_or_else(|| raw[name].clone());
            (name.clone(), value)
        })
        .collect();

    problems.sort();
    problems.dedup();

    Variables { values, problems }
}

/// `name`'s value with every reference the map can resolve expanded. `None`
/// means the chain looped or ran too deep; the caller leaves the token verbatim.
fn expand_name(
    raw: &BTreeMap<String, String>,
    name: &str,
    stack: &mut Vec<String>,
    problems: &mut Vec<String>,
) -> Option<String> {
    if stack.iter().any(|seen| seen == name) {
        problems.push(loop_message(stack));
        return None;
    }
    if stack.len() >= MAX_CHAIN {
        problems.push(format!(
            "variable `{name}` nests more than {MAX_CHAIN} levels deep"
        ));
        return None;
    }

    let value = raw.get(name)?;
    stack.push(name.to_string());
    let expanded = replace_tokens(value, |inner| expand_name(raw, inner, stack, problems));
    stack.pop();

    Some(expanded)
}

/// The loop written from its alphabetically first member, so the same cycle is
/// reported once however the traversal entered it.
fn loop_message(stack: &[String]) -> String {
    let mut names: Vec<&str> = stack.iter().map(String::as_str).collect();
    if let Some(at) = names
        .iter()
        .enumerate()
        .min_by_key(|(_, name)| **name)
        .map(|(at, _)| at)
    {
        names.rotate_left(at);
    }
    let first = names[0];
    names.push(first);
    format!("variables loop: {}", names.join(" -> "))
}

/// Replaces every `{{name}}` `lookup` can resolve. Anything it returns `None`
/// for is left exactly as written.
fn replace_tokens<F>(template: &str, mut lookup: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let name = rest[open + 2..open + 2 + close].trim();

        out.push_str(&rest[..open]);
        match lookup(name) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &rest[open + 2 + close + 2..];
    }

    out.push_str(rest);
    out
}

/// Replaces `{{name}}`. A name nothing resolves is left verbatim and recorded —
/// including one that a variable's own value referred to, so a broken chain is
/// visible instead of silently reaching the wire.
pub fn substitute(
    template: &str,
    vars: &BTreeMap<String, String>,
    missing: &mut Vec<String>,
) -> String {
    let out = replace_tokens(template, |name| vars.get(name).cloned());

    let mut rest = out.as_str();
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        let name = rest[open + 2..open + 2 + close].trim().to_string();
        if !missing.contains(&name) {
            missing.push(name);
        }
        rest = &rest[open + 2 + close + 2..];
    }

    out
}

/// The URL that will actually be requested: variables substituted, `:name`
/// path segments filled in, then enabled query parameters appended.
///
/// An unfilled path segment comes back as a `problem` rather than an error: the
/// command preview still wants to show the rest, while [`send`] refuses to run
/// it. A literal `:id` on the wire would hit a different endpoint than the one
/// on screen.
pub fn resolve_url(
    request: &RequestDef,
    vars: &BTreeMap<String, String>,
    missing: &mut Vec<String>,
) -> (String, Option<String>) {
    let mut url = substitute(&request.url, vars, missing);
    let mut problem = None;

    for name in request.path_tokens() {
        let Some(raw) = request.path_params.get(&name) else {
            problem.get_or_insert_with(|| format!("path parameter `:{name}` has no value"));
            continue;
        };
        let value = substitute(raw, vars, missing);
        url = replace_segment(&url, &name, &value);
    }

    let query: Vec<String> = request
        .params
        .iter()
        .filter(|param| !param.disabled)
        .filter(|param| !param.name.is_empty() || !param.value.is_empty())
        .map(|param| {
            let name = substitute(&param.name, vars, missing);
            let value = substitute(&param.value, vars, missing);
            if value.is_empty() {
                name
            } else {
                format!("{name}={value}")
            }
        })
        .collect();

    if !query.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&query.join("&"));
    }

    (url, problem)
}

/// Replaces a whole `:name` segment. Substring replacement would also rewrite
/// `:ids` when asked for `:id`.
fn replace_segment(url: &str, name: &str, value: &str) -> String {
    let token = format!(":{name}");
    url.split('/')
        .map(|segment| if segment == token { value } else { segment })
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Default)]
pub(crate) struct AuthParts {
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) query: Vec<(String, String)>,
}

pub fn send(
    request: &RequestDef,
    auth: Option<&Auth>,
    vars: &Variables,
    options: &SendOptions,
) -> Result<SentResponse, String> {
    let mut missing = vars.problems.clone();

    let (mut url, problem) = resolve_url(request, vars, &mut missing);
    if let Some(problem) = problem {
        return Err(problem);
    }
    if url.is_empty() {
        return Err("request has no URL".to_string());
    }

    let auth = match auth {
        Some(auth) => auth_parts(auth, vars, &mut missing)?,
        None => AuthParts::default(),
    };

    if !auth.query.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(
            &auth
                .query
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }

    // native-tls is never picked up implicitly by ureq, so it has to be handed
    // to the builder. On Windows this is SChannel, which means no C toolchain.
    // The same builder is what lets us stop verifying, for internal endpoints
    // whose certificates no public CA signed.
    let tls = ureq::native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(options.insecure)
        .danger_accept_invalid_hostnames(options.insecure)
        .build()
        .map_err(|e| format!("no TLS backend available: {e}"))?;
    let agent = ureq::AgentBuilder::new()
        .timeout(options.timeout)
        .tls_connector(std::sync::Arc::new(tls))
        .build();
    let mut call = agent.request(&request.method, &url);

    for header in &request.headers {
        if header.disabled {
            continue;
        }
        let name = substitute(&header.name, vars, &mut missing);
        let value = substitute(&header.value, vars, &mut missing);
        call = call.set(&name, &value);
    }
    // Auth last, so a header it sets wins over a stale hand-written one.
    for (name, value) in &auth.headers {
        call = call.set(name, value);
    }

    let started = Instant::now();

    let sent = match &request.body {
        Some(body) => {
            let text = substitute(body.text.as_deref().unwrap_or_default(), vars, &mut missing);
            if body.language.as_deref() == Some("json") && !has_header(request, "content-type") {
                call = call.set("Content-Type", "application/json");
            }
            call.send_string(&text)
        }
        None => call.call(),
    };

    let response = match sent {
        Ok(response) => response,
        // A 404 is a response, not a failure. Show it.
        Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(transport)) => return Err(transport.to_string()),
    };

    let elapsed = started.elapsed();
    let status = response.status();
    let content_type = response.header("content-type").map(str::to_string);

    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading the response body: {e}"))?;

    Ok(SentResponse {
        status,
        elapsed,
        body,
        content_type,
        url,
        insecure: options.insecure,
        missing,
    })
}

/// Async desktop transport. Dropping this future cancels both the connection
/// and response-body read, which lets the UI stop a slow request immediately.
pub async fn send_async(
    request: &RequestDef,
    auth: Option<&Auth>,
    vars: &Variables,
    options: &SendOptions,
) -> Result<SentResponse, String> {
    let mut missing = vars.problems.clone();
    let (mut url, problem) = resolve_url(request, vars, &mut missing);
    if let Some(problem) = problem {
        return Err(problem);
    }
    if url.is_empty() {
        return Err("request has no URL".to_string());
    }

    let auth = match auth {
        Some(auth) => auth_parts(auth, vars, &mut missing)?,
        None => AuthParts::default(),
    };
    if !auth.query.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(
            &auth
                .query
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }

    let client = reqwest::Client::builder()
        .timeout(options.timeout)
        .danger_accept_invalid_certs(options.insecure)
        .danger_accept_invalid_hostnames(options.insecure)
        .build()
        .map_err(|error| format!("no TLS backend available: {error}"))?;
    let mut call = client.request(
        reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| format!("invalid method: {error}"))?,
        &url,
    );
    for header in &request.headers {
        if header.disabled {
            continue;
        }
        let name = substitute(&header.name, vars, &mut missing);
        let value = substitute(&header.value, vars, &mut missing);
        call = call.header(name, value);
    }
    for (name, value) in &auth.headers {
        call = call.header(name, value);
    }
    if let Some(body) = &request.body {
        let text = substitute(body.text.as_deref().unwrap_or_default(), vars, &mut missing);
        if body.language.as_deref() == Some("json") && !has_header(request, "content-type") {
            call = call.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        call = call.body(text);
    }

    let started = Instant::now();
    let mut response = call.send().await.map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("reading the response body: {error}"))?
    {
        let remaining = MAX_BODY.saturating_sub(body.len() as u64) as usize;
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }

    Ok(SentResponse {
        status,
        elapsed: started.elapsed(),
        body,
        content_type,
        url,
        insecure: options.insecure,
        missing,
    })
}

pub(crate) fn auth_parts(
    auth: &Auth,
    vars: &BTreeMap<String, String>,
    missing: &mut Vec<String>,
) -> Result<AuthParts, String> {
    let mut field = |name: &str| {
        substitute(
            auth.fields
                .get(name)
                .map(String::as_str)
                .unwrap_or_default(),
            vars,
            missing,
        )
    };
    let mut parts = AuthParts::default();

    match auth.kind.as_str() {
        "bearer" => {
            let token = field("token");
            if token.is_empty() {
                return Err("bearer auth has no token".to_string());
            }
            parts
                .headers
                .push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        "basic" => {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                field("username"),
                field("password")
            ));
            parts
                .headers
                .push(("Authorization".to_string(), format!("Basic {encoded}")));
        }
        "apikey" => {
            let name = field("key");
            let value = field("value");
            match auth.fields.get("in").map(String::as_str) {
                Some("query") => parts.query.push((name, value)),
                _ => parts.headers.push((name, value)),
            }
        }
        _ => {}
    }

    Ok(parts)
}

fn has_header(request: &RequestDef, name: &str) -> bool {
    request
        .headers
        .iter()
        .any(|header| !header.disabled && header.name.eq_ignore_ascii_case(name))
}
