//! Reading a pasted `curl` command into a request.
//!
//! Accepts what browsers and API docs copy: bash quoting with `\` line breaks,
//! Windows `cmd` (`^`) and PowerShell (`` ` ``) line breaks. The command is not
//! stored as-is; its parts land where the editor keeps them. Query strings
//! become `params`, a JSON body is marked and indented, and credentials move
//! out of headers into request auth whose value is a secret request variable,
//! so saving the request cannot put a token in a committed file.

use crate::model::{Auth, AuthSetting, Body, KeyValue, RequestDef, Variable};
use crate::send::encode;

pub struct Imported {
    pub request: RequestDef,
    /// Options that were understood but not carried over, such as `-k` or a
    /// `@file` body. The window lists them so nothing is dropped silently.
    pub warnings: Vec<String>,
}

/// Flags that take no value and change nothing a stored request records.
const IGNORED_FLAGS: &[&str] = &[
    "-L",
    "--location",
    "-s",
    "--silent",
    "-S",
    "--show-error",
    "-v",
    "--verbose",
    "--compressed",
    "-i",
    "--include",
    "-f",
    "--fail",
    "--http1.0",
    "--http1.1",
    "--http2",
    "--http2-prior-knowledge",
    "--http3",
    "-#",
    "--progress-bar",
    "-N",
    "--no-buffer",
    "-g",
    "--globoff",
    "--path-as-is",
    "-O",
    "--remote-name",
];

/// Options whose value is consumed and ignored.
const IGNORED_WITH_VALUE: &[&str] = &[
    "-o",
    "--output",
    "-m",
    "--max-time",
    "--connect-timeout",
    "-x",
    "--proxy",
    "--retry",
    "-w",
    "--write-out",
    "--cacert",
    "--capath",
    "-E",
    "--cert",
    "--key",
    "-c",
    "--cookie-jar",
    "--resolve",
    "--max-redirs",
    "--limit-rate",
    "-U",
    "--proxy-user",
    "--interface",
];

/// Short flags that may be bundled, as in `-sSL`.
const BUNDLED: &str = "LsSvifgNOkIG#";

pub fn parse(text: &str) -> Result<Imported, String> {
    let words = split(&join_lines(text))?;
    let mut words = words.into_iter();
    match words.next().as_deref() {
        Some("curl") | Some("curl.exe") => {}
        _ => return Err("a cURL command starts with `curl`".to_string()),
    }

    let mut warnings = Vec::new();
    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<KeyValue> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut json = false;
    let mut get = false;
    let mut head = false;
    let mut user: Option<String> = None;

    while let Some(word) = words.next() {
        let (flag, attached) = split_flag(&word);
        let mut value = |name: &str| -> Result<String, String> {
            match &attached {
                Some(value) => Ok(value.clone()),
                None => words
                    .next()
                    .ok_or_else(|| format!("`{name}` needs a value")),
            }
        };
        match flag.as_str() {
            "-X" | "--request" => method = Some(value(&flag)?.to_uppercase()),
            "-H" | "--header" => {
                let line = value(&flag)?;
                if let Some((name, value)) = line.split_once(':') {
                    headers.push(header(name.trim(), value.trim()));
                } else if let Some(name) = line.strip_suffix(';') {
                    headers.push(header(name.trim(), ""));
                }
            }
            "-d" | "--data" | "--data-ascii" | "--data-binary" => {
                let body = value(&flag)?;
                if body.starts_with('@') {
                    warnings.push(format!("`{flag} {body}` reads a file; the body was left as the file name"));
                }
                data.push(body);
            }
            "--data-raw" => data.push(value(&flag)?),
            "--data-urlencode" => data.push(urlencode_data(&value(&flag)?)),
            "--json" => {
                data.push(value(&flag)?);
                json = true;
            }
            "-F" | "--form" | "--form-string" => {
                value(&flag)?;
                warnings.push("multipart form fields (`-F`) are not imported".to_string());
            }
            "-u" | "--user" => user = Some(value(&flag)?),
            "-A" | "--user-agent" => headers.push(header("User-Agent", &value(&flag)?)),
            "-e" | "--referer" => headers.push(header("Referer", &value(&flag)?)),
            "-b" | "--cookie" => headers.push(header("Cookie", &value(&flag)?)),
            "--url" => url = Some(value(&flag)?),
            "-G" | "--get" => get = true,
            "-I" | "--head" => head = true,
            "-k" | "--insecure" => warnings.push(
                "`--insecure` is an environment setting here; turn it on for the environment instead"
                    .to_string(),
            ),
            flag if IGNORED_FLAGS.contains(&flag) => {}
            flag if IGNORED_WITH_VALUE.contains(&flag) => {
                value(flag)?;
            }
            flag if is_bundle(flag) => {
                for short in flag[1..].chars() {
                    match short {
                        'G' => get = true,
                        'I' => head = true,
                        'k' => warnings.push(
                            "`--insecure` is an environment setting here; turn it on for the environment instead"
                                .to_string(),
                        ),
                        _ => {}
                    }
                }
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                warnings.push(format!("`{flag}` is not supported and was ignored"));
            }
            _ if url.is_none() => url = Some(word),
            _ => warnings.push(format!("extra URL `{word}` was ignored")),
        }
    }

    let url = url.ok_or("the cURL command has no URL")?;
    let mut request = RequestDef {
        url,
        ..Default::default()
    };
    if get && !data.is_empty() {
        let query = data.join("&");
        request
            .url
            .push(if request.url.contains('?') { '&' } else { '?' });
        request.url.push_str(&query);
        data.clear();
    }
    request.method = match (method, head) {
        (Some(method), _) => method,
        (None, true) => "HEAD".to_string(),
        (None, false) if !data.is_empty() => "POST".to_string(),
        (None, false) => "GET".to_string(),
    };
    request.normalize_query();

    if json {
        if !has(&headers, "content-type") {
            headers.push(header("Content-Type", "application/json"));
        }
        if !has(&headers, "accept") {
            headers.push(header("Accept", "application/json"));
        }
    }
    if !data.is_empty() {
        let text = data.join("&");
        let declared = headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-type"))
            .map(|header| header.value.to_ascii_lowercase());
        let is_json = match &declared {
            Some(kind) => kind.contains("json"),
            None => serde_json::from_str::<serde_json::Value>(&text).is_ok(),
        };
        if declared.is_none() && !is_json {
            // What curl itself sends with `-d`; the transport would not add it.
            headers.push(header("Content-Type", "application/x-www-form-urlencoded"));
        }
        let pretty = is_json.then(|| indent_json(&text)).flatten();
        request.body = Some(Body {
            mode: "raw".to_string(),
            language: is_json.then(|| "json".to_string()),
            text: Some(pretty.unwrap_or(text)),
            ..Body::default()
        });
    }

    headers.retain(|header| !header.name.eq_ignore_ascii_case("content-length"));
    if let Some(user) = user {
        let (name, password) = user.split_once(':').unwrap_or((&user, ""));
        request.auth = basic(&mut request.variables, name, password);
    }
    let mut kept = Vec::new();
    for header in headers {
        if header.name.eq_ignore_ascii_case("authorization") {
            if let Some(auth) = authorization(&mut request.variables, &header.value) {
                request.auth = auth;
                continue;
            }
        }
        kept.push(header);
    }
    request.headers = kept;
    Ok(Imported { request, warnings })
}

fn header(name: &str, value: &str) -> KeyValue {
    KeyValue {
        name: name.to_string(),
        value: value.to_string(),
        disabled: false,
    }
}

fn has(headers: &[KeyValue], name: &str) -> bool {
    headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case(name))
}

fn secret(variables: &mut Vec<Variable>, name: &str, value: &str) -> String {
    variables.push(Variable {
        name: name.to_string(),
        value: Some(value.to_string()),
        secret: true,
    });
    format!("{{{{{name}}}}}")
}

fn basic(variables: &mut Vec<Variable>, name: &str, password: &str) -> AuthSetting {
    let password = secret(variables, "password", password);
    AuthSetting::Method(Auth {
        kind: "basic".to_string(),
        fields: [
            ("username".to_string(), name.to_string()),
            ("password".to_string(), password),
        ]
        .into(),
    })
}

/// `Bearer` and `Basic` become request auth; any other scheme stays a header.
fn authorization(variables: &mut Vec<Variable>, value: &str) -> Option<AuthSetting> {
    let (scheme, credentials) = value.split_once(' ')?;
    let credentials = credentials.trim();
    if scheme.eq_ignore_ascii_case("bearer") {
        let token = secret(variables, "token", credentials);
        return Some(AuthSetting::Method(Auth {
            kind: "bearer".to_string(),
            fields: [("token".to_string(), token)].into(),
        }));
    }
    if scheme.eq_ignore_ascii_case("basic") {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(credentials)
            .ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        let (name, password) = decoded.split_once(':')?;
        return Some(basic(variables, name, password));
    }
    None
}

/// `--data-urlencode` forms: `content`, `=content`, `name=content`.
fn urlencode_data(value: &str) -> String {
    match value.split_once('=') {
        Some(("", content)) => encode(content),
        Some((name, content)) => format!("{name}={}", encode(content)),
        None => encode(value),
    }
}

/// `-XPOST`, `--request=POST` and `-H'A: b'` carry their value attached.
fn split_flag(word: &str) -> (String, Option<String>) {
    if let Some(long) = word.strip_prefix("--") {
        return match long.split_once('=') {
            Some((name, value)) => (format!("--{name}"), Some(value.to_string())),
            None => (word.to_string(), None),
        };
    }
    let attached = [
        "-X", "-H", "-d", "-u", "-A", "-e", "-b", "-F", "-o", "-m", "-x", "-w",
    ];
    for short in attached {
        if let Some(value) = word.strip_prefix(short).filter(|value| !value.is_empty()) {
            return (short.to_string(), Some(value.to_string()));
        }
    }
    (word.to_string(), None)
}

fn is_bundle(flag: &str) -> bool {
    flag.len() > 2
        && flag.starts_with('-')
        && !flag.starts_with("--")
        && flag[1..].chars().all(|short| BUNDLED.contains(short))
}

/// Folds `\`, `^` and `` ` `` line continuations, and drops `cmd` caret
/// escapes when the command was copied for `cmd`.
fn join_lines(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let cmd = text.contains("^\n") || text.contains("^\"");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' | '`' if chars.peek() == Some(&'\n') => {
                chars.next();
                out.push(' ');
            }
            '^' if cmd => match chars.next() {
                Some('\n') => out.push(' '),
                Some(next) => out.push(next),
                None => {}
            },
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// POSIX-shell word splitting: single quotes, double quotes with `\`
/// escapes, `$'...'` with C escapes, and bare `\` escapes.
fn split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unclosed `'` quote".to_string()),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => word.push('\n'),
                            Some('t') => word.push('\t'),
                            Some('r') => word.push('\r'),
                            Some(c) => word.push(c),
                            None => return Err("unclosed `$'` quote".to_string()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("unclosed `$'` quote".to_string()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&next @ ('"' | '\\' | '$' | '`')) => {
                                chars.next();
                                word.push(next);
                            }
                            _ => word.push('\\'),
                        },
                        Some(c) => word.push(c),
                        None => return Err("unclosed `\"` quote".to_string()),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// Two-space indentation that keeps key order, which a round trip through
/// `serde_json::Value` would not. `None` if the text is not JSON.
fn indent_json(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.peek().is_some_and(|next| next.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('}' | ']')) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}
