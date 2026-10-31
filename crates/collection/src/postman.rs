//! Postman v2.1 import.
//!
//! Parsed leniently from `serde_json::Value`: real exports carry fields that no
//! published schema mentions, and a strict struct would reject collections over
//! trivia.

use crate::model::*;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct Parsed {
    pub collection: Collection,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct ParsedEnvironment {
    pub environment: Environment,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct ParsedGlobals {
    pub variables: Vec<Variable>,
    pub warnings: Vec<String>,
}

pub fn parse_globals(bytes: &[u8], extra_secrets: &[String]) -> Result<ParsedGlobals, String> {
    let doc: Value =
        serde_json::from_slice(strip_bom(bytes)).map_err(|e| format!("not valid JSON: {e}"))?;
    if doc.get("_postman_variable_scope").and_then(Value::as_str) != Some("globals")
        || !doc.get("values").is_some_and(Value::is_array)
    {
        return Err("this does not look like a Postman globals export".to_string());
    }
    let mut warnings = Vec::new();
    let variables = parse_variables(doc.get("values"), extra_secrets, &mut warnings);
    Ok(ParsedGlobals {
        variables,
        warnings,
    })
}

pub fn parse_collection(bytes: &[u8], extra_secrets: &[String]) -> Result<Parsed, String> {
    let doc: Value =
        serde_json::from_slice(strip_bom(bytes)).map_err(|e| format!("not valid JSON: {e}"))?;

    let mut warnings = Vec::new();

    let info = doc.get("info");
    let name = info
        .and_then(|i| i.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("imported")
        .to_string();

    if let Some(schema) = info.and_then(|i| i.get("schema")).and_then(|v| v.as_str()) {
        if schema.contains("v1.0.0") {
            warnings.push(
                "this looks like a Postman v1 collection; re-export as v2.1 to keep folders and auth"
                    .to_string(),
            );
        }
    } else if doc.get("item").is_none() {
        return Err(
            "this does not look like a Postman collection (no `info` or `item`)".to_string(),
        );
    }

    let variables = parse_variables(doc.get("variable"), extra_secrets, &mut warnings);
    let items = parse_items(doc.get("item"), extra_secrets, &mut warnings);

    let mut collection = Collection {
        id: String::new(),
        name,
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        description: text_of(info.and_then(|i| i.get("description"))),
        auth: parse_auth(doc.get("auth"), &mut warnings),
        variables,
        scripts: Default::default(),
        items,
    };
    collection.assign_ids();
    Ok(Parsed {
        collection,
        warnings,
    })
}

pub fn parse_environment(
    bytes: &[u8],
    extra_secrets: &[String],
) -> Result<ParsedEnvironment, String> {
    let doc: Value =
        serde_json::from_slice(strip_bom(bytes)).map_err(|e| format!("not valid JSON: {e}"))?;

    let Some(name) = doc.get("name").and_then(|v| v.as_str()) else {
        return Err("this does not look like a Postman environment (no `name`)".to_string());
    };
    if !doc.get("values").is_some_and(Value::is_array)
        || doc
            .get("_postman_variable_scope")
            .and_then(Value::as_str)
            .is_some_and(|scope| scope != "environment")
    {
        return Err("this does not look like a Postman environment export".to_string());
    }

    let mut warnings = Vec::new();
    let variables = parse_variables(doc.get("values"), extra_secrets, &mut warnings);

    Ok(ParsedEnvironment {
        environment: Environment {
            name: name.to_string(),
            main_slot: None,
            platforms: Vec::new(),
            // Nothing in a Postman export records this; it was an app-level
            // setting there, so it defaults to verifying.
            insecure: false,
            variables,
        },
        warnings,
    })
}

fn parse_items(
    value: Option<&Value>,
    extra_secrets: &[String],
    warnings: &mut Vec<String>,
) -> Vec<Item> {
    let Some(list) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in list {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled")
            .to_string();

        // Postman puts a folder's own auth on the item and leaves it out of the
        // requests underneath, so this is a setting to keep, not a value to copy
        // down onto each request.
        let auth = parse_auth(entry.get("auth"), warnings);
        // Same for a folder's or a request's own variables: they live on the
        // item, not inside `request`, and they are the narrowest layer.
        let variables = parse_variables(entry.get("variable"), extra_secrets, warnings);

        if let Some(request) = entry.get("request") {
            let mut definition = match request {
                // A bare string URL is legal in v2.0 exports.
                Value::String(url) => {
                    warnings.push(format!(
                        "`{name}` has a bare URL as its request; assumed GET"
                    ));
                    RequestDef {
                        method: "GET".to_string(),
                        url: url.clone(),
                        ..RequestDef::default()
                    }
                }
                other => build_request(other, warnings, &name),
            };
            definition.variables = variables;
            out.push(Item {
                id: String::new(),
                name,
                auth: AuthSetting::Inherit,
                variables: Vec::new(),
                scripts: Default::default(),
                request: Some(definition),
                items: Vec::new(),
            });
        } else if entry.get("item").is_some() {
            out.push(Item {
                id: String::new(),
                name,
                auth,
                variables,
                scripts: Default::default(),
                request: None,
                items: parse_items(entry.get("item"), extra_secrets, warnings),
            });
        } else {
            warnings.push(format!(
                "`{name}` has neither a request nor children; skipped"
            ));
        }
    }
    out
}

fn build_request(request: &Value, warnings: &mut Vec<String>, name: &str) -> RequestDef {
    let method = request
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_ascii_uppercase();

    let mut headers = Vec::new();
    if let Some(list) = request.get("header").and_then(|v| v.as_array()) {
        for entry in list {
            if let Some(key) = entry.get("key").and_then(|v| v.as_str()) {
                if entry
                    .get("disabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    headers.push(KeyValue {
                        name: key.to_string(),
                        value: scalar_string(entry.get("value")).unwrap_or_default(),
                        disabled: true,
                    });
                } else {
                    headers.push(KeyValue {
                        name: key.to_string(),
                        value: scalar_string(entry.get("value")).unwrap_or_default(),
                        disabled: false,
                    });
                }
            }
        }
    }

    // Postman stores the URL both as an object and as `raw`. `raw` is the one
    // the user actually sees, and it already contains the query string, so
    // trusting it avoids sending every parameter twice.
    let url = url_string(request.get("url"));
    if url.is_empty() {
        warnings.push(format!("`{name}` has no URL"));
    }

    let auth = parse_auth(request.get("auth"), warnings);

    let mut definition = RequestDef {
        method,
        url,
        headers,
        body: request.get("body").and_then(|b| parse_body(b, warnings)),
        auth,
        description: text_of(request.get("description")),
        ..RequestDef::default()
    };
    // The inline query is valid but the window edits parameters as a list, so
    // imported files are written in the canonical form from the start.
    definition.normalize_query();
    definition
}

fn parse_body(value: &Value, warnings: &mut Vec<String>) -> Option<Body> {
    let mode = value.get("mode").and_then(|v| v.as_str())?;
    match mode {
        "none" => None,
        "raw" => Some(Body {
            mode: mode.to_string(),
            language: value
                .get("options")
                .and_then(|o| o.get("raw"))
                .and_then(|r| r.get("language"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            text: value
                .get("raw")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            ..Body::default()
        }),
        "graphql" => {
            let graphql = value.get("graphql");
            let field = |name: &str| {
                graphql
                    .and_then(|g| g.get(name))
                    .and_then(|v| v.as_str())
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
            };
            Some(Body {
                mode: mode.to_string(),
                text: field("query"),
                variables: field("variables"),
                ..Body::default()
            })
        }
        other => {
            warnings.push(format!(
                "body mode `{other}` was not converted; the mode is recorded but the fields are not"
            ));
            Some(Body {
                mode: other.to_string(),
                ..Body::default()
            })
        }
    }
}

fn parse_auth(value: Option<&Value>, warnings: &mut Vec<String>) -> AuthSetting {
    let Some(value) = value else {
        return AuthSetting::Inherit;
    };
    let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
        return AuthSetting::Inherit;
    };

    // Both are states, not methods: `inherit` defers upwards, `noauth` stops the
    // chain so a folder's token is not sent to an endpoint that rejects it.
    match kind {
        "inherit" => return AuthSetting::Inherit,
        "noauth" | "none" => return AuthSetting::None,
        _ => {}
    }

    let mut fields = BTreeMap::new();
    if let Some(entries) = value.get(kind).and_then(|v| v.as_array()) {
        for entry in entries {
            let key = entry.get("key").and_then(|v| v.as_str());
            if let (Some(key), Some(value)) = (key, scalar_string(entry.get("value"))) {
                if !value.is_empty() {
                    fields.insert(key.to_string(), value);
                }
            }
        }
    }

    if !matches!(kind, "bearer" | "basic" | "apikey") {
        warnings.push(format!(
            "auth type `{kind}` is kept but not understood yet; check it before sending"
        ));
    }

    AuthSetting::Method(Auth {
        kind: kind.to_string(),
        fields,
    })
}

fn parse_variables(
    value: Option<&Value>,
    extra_secrets: &[String],
    warnings: &mut Vec<String>,
) -> Vec<Variable> {
    let Some(list) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in list {
        let Some(name) = entry.get("key").and_then(|v| v.as_str()) else {
            continue;
        };

        // Collections mark a variable `disabled`; environments mark it
        // `enabled`. Absent means enabled in both.
        let off = entry
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || !entry
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
        if off {
            continue;
        }

        let secret = entry.get("type").and_then(|v| v.as_str()) == Some("secret")
            || extra_secrets.iter().any(|extra| extra == name);

        if entry
            .get("value")
            .is_some_and(|v| v.is_object() || v.is_array())
        {
            warnings.push(format!(
                "variable `{name}` has a structured value; kept as JSON text"
            ));
        }

        out.push(Variable {
            name: name.to_string(),
            value: scalar_string(entry.get("value")),
            secret,
        });
    }
    out
}

fn url_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(raw)) => raw.clone(),
        Some(Value::Object(map)) => {
            if let Some(raw) = map.get("raw").and_then(|v| v.as_str()) {
                return raw.to_string();
            }

            let mut out = String::new();
            if let Some(protocol) = map.get("protocol").and_then(|v| v.as_str()) {
                out.push_str(protocol);
                out.push_str("://");
            }
            out.push_str(&joined(map.get("host"), "."));

            let path = joined(map.get("path"), "/");
            if !path.is_empty() {
                if !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&path);
            }

            let query = query_string(map.get("query"));
            if !query.is_empty() {
                out.push('?');
                out.push_str(&query);
            }
            out
        }
        _ => String::new(),
    }
}

fn joined(value: Option<&Value>, sep: &str) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.as_str())
            .collect::<Vec<_>>()
            .join(sep),
        _ => String::new(),
    }
}

fn query_string(value: Option<&Value>) -> String {
    let Some(list) = value.and_then(|v| v.as_array()) else {
        return String::new();
    };

    list.iter()
        .filter(|entry| {
            !entry
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|entry| {
            let key = entry.get("key").and_then(|v| v.as_str())?;
            let value = scalar_string(entry.get("value")).unwrap_or_default();
            if value.is_empty() {
                Some(key.to_string())
            } else {
                Some(format!("{key}={value}"))
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn text_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => map
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    }
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}
