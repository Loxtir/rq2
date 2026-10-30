//! Pasted cURL commands, and creating collections and requests on disk.

use collection::curl_import::parse;
use collection::model::AuthSetting;
use collection::store;

fn field<'a>(auth: &'a AuthSetting, name: &str) -> &'a str {
    match auth {
        AuthSetting::Method(auth) => auth.fields[name].as_str(),
        _ => panic!("expected an auth method"),
    }
}

#[test]
fn a_bash_command_lands_in_the_editor_fields() {
    let imported = parse(
        r#"curl 'https://api.test/users?page=2&q=a%20b' \
  -H 'Authorization: Bearer abc.def' \
  -H "X-Trace: 1" \
  --data-raw '{"name":"Ann","tags":[],"nested":{"z":1,"a":2}}' \
  --compressed -sSL"#,
    )
    .unwrap();
    let request = imported.request;

    assert_eq!(request.method, "POST");
    assert_eq!(request.url, "https://api.test/users");
    let params: Vec<_> = request
        .params
        .iter()
        .map(|p| (p.name.as_str(), p.value.as_str()))
        .collect();
    assert_eq!(params, [("page", "2"), ("q", "a%20b")]);

    let headers: Vec<_> = request.headers.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(headers, ["X-Trace"], "the token left the headers");
    assert_eq!(field(&request.auth, "token"), "{{token}}");
    assert_eq!(request.variables[0].value.as_deref(), Some("abc.def"));
    assert!(request.variables[0].secret);

    let body = request.body.unwrap();
    assert_eq!(body.language.as_deref(), Some("json"));
    assert_eq!(
        body.text.unwrap(),
        "{\n  \"name\": \"Ann\",\n  \"tags\": [],\n  \"nested\": {\n    \"z\": 1,\n    \"a\": 2\n  }\n}",
        "indented, key order kept"
    );
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
}

#[test]
fn a_windows_cmd_copy_is_unescaped() {
    let imported = parse(
        "curl ^\"https://api.test/a^\" ^\r\n  -X PUT ^\r\n  -H ^\"content-type: application/json^\" ^\r\n  --data-raw ^\"^{^\\^\"a^\\^\":1^}^\" ^\r\n  --insecure",
    )
    .unwrap();
    assert_eq!(imported.request.method, "PUT");
    assert_eq!(imported.request.url, "https://api.test/a");
    assert_eq!(
        imported.request.body.unwrap().text.unwrap(),
        "{\n  \"a\": 1\n}"
    );
    assert_eq!(imported.warnings.len(), 1, "--insecure is reported");
}

#[test]
fn form_data_get_and_basic_auth_follow_curl() {
    let posted = parse("curl -u ann:pw -d a=1 -d b=2 https://api.test/form").unwrap();
    assert_eq!(posted.request.method, "POST");
    assert_eq!(posted.request.body.unwrap().text.unwrap(), "a=1&b=2");
    assert_eq!(
        posted.request.headers[0].value,
        "application/x-www-form-urlencoded"
    );
    assert_eq!(field(&posted.request.auth, "username"), "ann");
    assert_eq!(field(&posted.request.auth, "password"), "{{password}}");

    let get = parse("curl -G --data-urlencode 'q=a b' https://api.test/search").unwrap();
    assert_eq!(get.request.method, "GET");
    assert!(get.request.body.is_none());
    assert_eq!(get.request.params[0].value, "a%20b");

    assert!(parse("wget https://x").is_err());
    assert!(parse("curl -H 'a: b'").is_err());
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("rq-create-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn collections_and_requests_are_created_without_leaking_tokens() {
    let root = temp_root("create");
    let slug = store::create_collection(&root, "My API").unwrap();
    assert_eq!(slug, "my-api");
    assert!(store::create_collection(&root, "my api").is_err());

    let folder = store::add_item(&root, &slug, &[], "Users", None).unwrap();
    let first = parse("curl -H 'Authorization: Bearer one' https://api.test/a").unwrap();
    let path = store::add_item(&root, &slug, &folder, "A", Some(first.request)).unwrap();
    assert_eq!(path, [0, 0]);
    let second = parse("curl -H 'Authorization: Bearer two' https://api.test/b").unwrap();
    store::add_item(&root, &slug, &[], "B", Some(second.request)).unwrap();

    let workspace = store::load(&root).unwrap();
    let collection = workspace.collection(&slug).unwrap();
    let a = collection.request_at(&[0, 0]).unwrap();
    let b = collection.request_at(&[1]).unwrap();
    assert_eq!(field(&a.request.auth, "token"), "{{token}}");
    assert_eq!(
        field(&b.request.auth, "token"),
        "{{token2}}",
        "renamed, not overwritten"
    );
    assert_eq!(b.request.variables[0].value.as_deref(), Some("two"));

    let mut committed = String::new();
    for entry in walk(&root.join("collections")) {
        committed.push_str(&std::fs::read_to_string(entry).unwrap());
    }
    assert!(!committed.contains("one") && !committed.contains("two"));
    assert!(std::fs::read_to_string(root.join(".gitignore"))
        .unwrap()
        .contains(".secrets"));
    std::fs::remove_dir_all(root).unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
