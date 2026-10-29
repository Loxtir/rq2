use collection::model::{AuthSetting, Collection, Item, KeyValue, RequestDef, Variable};
use collection::postman::{parse_collection, parse_environment, parse_globals};
use collection::store::{
    ensure_gitattributes, ensure_gitignore, import_postman, load, parse_env, quote_env, render,
    slugify, write,
};
use std::path::PathBuf;

const POSTMAN: &str = r##"{
  "info": {
    "name": "Acme API",
    "description": "Internal services",
    "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
  },
  "auth": {
    "type": "bearer",
    "bearer": [{ "key": "token", "value": "{{apiToken}}", "type": "string" }]
  },
  "variable": [
    { "key": "baseUrl", "value": "https://api.acme.test" },
    { "key": "apiToken", "value": "tok_live_9f8e7d6c5b4a", "type": "secret" },
    { "key": "legacy", "value": "unused", "disabled": true }
  ],
  "item": [
    {
      "name": "List orders",
      "request": {
        "method": "GET",
        "header": [
          { "key": "Accept", "value": "application/json" },
          { "key": "X-Trace", "value": "1", "disabled": true }
        ],
        "url": {
          "raw": "{{baseUrl}}/v1/orders?page=2",
          "host": ["{{baseUrl}}"],
          "path": ["v1", "orders"],
          "query": [{ "key": "page", "value": "2" }]
        }
      }
    },
    {
      "name": "Admin",
      "auth": {
        "type": "apikey",
        "apikey": [
          { "key": "key", "value": "X-Api-Key" },
          { "key": "value", "value": "{{apiToken}}" },
          { "key": "in", "value": "header" }
        ]
      },
      "item": [
        {
          "name": "Create order",
          "request": {
            "method": "post",
            "body": {
              "mode": "raw",
              "raw": "{\"sku\":\"A-1\"}",
              "options": { "raw": { "language": "json" } }
            },
            "url": { "raw": "{{baseUrl}}/v1/orders" }
          }
        }
      ]
    },
    {
      "name": "Ping",
      "request": {
        "method": "GET",
        "auth": { "type": "noauth" },
        "url": "{{baseUrl}}/v1/ping"
      }
    }
  ]
}"##;

const ENVIRONMENT: &str = r#"{
  "name": "dev",
  "values": [
    { "key": "baseUrl", "value": "https://dev.acme.test", "enabled": true },
    { "key": "apiKey", "value": "env-secret-value-123", "type": "secret", "enabled": true },
    { "key": "off", "value": "x", "enabled": false }
  ],
  "_postman_variable_scope": "environment"
}"#;

const TOKEN: &str = "tok_live_9f8e7d6c5b4a";

fn parsed() -> Collection {
    parse_collection(POSTMAN.as_bytes(), &[])
        .expect("collection should parse")
        .collection
}

fn parsed_environment() -> collection::Environment {
    parse_environment(ENVIRONMENT.as_bytes(), &[])
        .expect("environment should parse")
        .environment
}

/// The auth each request will actually send, in request order.
fn effective_auth(collection: &Collection) -> Vec<Option<String>> {
    collection
        .requests()
        .iter()
        .map(|r| r.auth.map(|auth| auth.kind.clone()))
        .collect()
}

// ---------------------------------------------------------------- parsing

#[test]
fn reads_folders_headers_requests_and_bodies() {
    let collection = parsed();

    assert_eq!(collection.name, "Acme API");
    assert_eq!(collection.description.as_deref(), Some("Internal services"));
    assert_eq!(collection.folder_count(), 1);

    let requests = collection.requests();
    assert_eq!(requests.len(), 3);

    let list = &requests[0];
    assert!(list.trail.is_empty());
    assert_eq!(list.request.method, "GET");
    assert_eq!(list.request.url, "{{baseUrl}}/v1/orders");
    assert_eq!(list.request.params.len(), 1);
    assert_eq!(list.request.params[0].name, "page");
    assert_eq!(list.request.params[0].value, "2");
    assert_eq!(list.request.headers.len(), 2);
    assert_eq!(list.request.headers[0].value, "application/json");

    let create = &requests[1];
    assert_eq!(create.trail, vec!["Admin"]);
    assert_eq!(create.request.method, "POST", "methods are normalised");
    assert_eq!(
        create.request.body.as_ref().unwrap().text.as_deref(),
        Some(r#"{"sku":"A-1"}"#)
    );
    assert_eq!(
        create.request.body.as_ref().unwrap().language.as_deref(),
        Some("json")
    );

    let ping = &requests[2];
    assert_eq!(ping.request.method, "GET");
    assert_eq!(ping.request.url, "{{baseUrl}}/v1/ping");
}

#[test]
fn auth_is_inherited_rather_than_copied_onto_each_request() {
    let collection = parsed();

    // The collection's auth is stored once, on the collection.
    assert_eq!(collection.auth.method().unwrap().kind, "bearer");

    // The requests that inherit say so, and resolve to the collection's bearer.
    let requests = collection.requests();
    assert_eq!(requests[0].request.auth, AuthSetting::Inherit);
    assert_eq!(requests[0].auth.unwrap().fields["token"], "{{apiToken}}");
}

#[test]
fn a_folder_auth_overrides_the_collection() {
    let collection = parsed();
    let requests = collection.requests();
    let create = &requests[1];

    assert_eq!(create.request.auth, AuthSetting::Inherit, "no copy-down");
    assert_eq!(&create.trail, &["Admin"]);

    let auth = create.auth.expect("folder auth applies");
    assert_eq!(auth.kind, "apikey", "the folder wins over the collection");
    assert_eq!(auth.fields["key"], "X-Api-Key");
}

#[test]
fn noauth_stops_the_chain() {
    let collection = parsed();
    let requests = collection.requests();
    let ping = &requests[2];

    assert_eq!(ping.request.auth, AuthSetting::None);
    assert!(
        ping.auth.is_none(),
        "an explicit no-auth must not fall back to the collection token"
    );

    assert_eq!(
        effective_auth(&collection),
        vec![Some("bearer".to_string()), Some("apikey".to_string()), None]
    );
}

#[test]
fn disabled_headers_are_kept_not_dropped() {
    let collection = parsed();
    let list = &collection.requests()[0];

    let trace = list
        .request
        .headers
        .iter()
        .find(|h| h.name == "X-Trace")
        .unwrap();
    assert!(trace.disabled, "a disabled header is still intent");
}

#[test]
fn disabled_variables_are_dropped() {
    let collection = parsed();
    assert!(
        collection.variables.iter().all(|v| v.name != "legacy"),
        "a disabled Postman variable should not become live config"
    );
}

#[test]
fn item_variables_land_on_the_folder_and_the_request() {
    let json = r#"{
      "info": { "name": "x", "schema": "v2.1.0" },
      "item": [{
        "name": "Admin",
        "variable": [{ "key": "limit", "value": "5" }],
        "item": [{
          "name": "r",
          "variable": [{ "key": "postNumber", "value": "1" }],
          "request": { "method": "GET", "url": "https://api.test/posts/:postId" }
        }]
      }]
    }"#;

    let parsed = parse_collection(json.as_bytes(), &[]).unwrap();
    let folder = &parsed.collection.items[0];
    assert!(folder.request.is_none());
    assert_eq!(folder.variables[0].name, "limit");

    let item = &folder.items[0];
    assert!(
        item.variables.is_empty(),
        "a request's own scope is not its item"
    );
    let request = item.request.as_ref().expect("request");
    assert_eq!(request.variables[0].name, "postNumber");
    assert_eq!(request.variables[0].value.as_deref(), Some("1"));
}

#[test]
fn an_unconverted_body_mode_is_reported_not_lost() {
    let json = r#"{
      "info": { "name": "x", "schema": "v2.1.0" },
      "item": [{
        "name": "upload",
        "request": {
          "method": "POST",
          "url": "https://api.test/upload",
          "body": { "mode": "formdata", "formdata": [{ "key": "file", "type": "file" }] }
        }
      }]
    }"#;

    let parsed = parse_collection(json.as_bytes(), &[]).unwrap();
    let request = &parsed.collection.requests()[0].request;

    assert_eq!(request.body.as_ref().unwrap().mode, "formdata");
    assert!(request.body.as_ref().unwrap().text.is_none());
    assert!(
        parsed.warnings.iter().any(|w| w.contains("formdata")),
        "{:?}",
        parsed.warnings
    );
}

#[test]
fn a_bare_url_request_says_it_assumed_get() {
    let json = r#"{
      "info": { "name": "x", "schema": "v2.1.0" },
      "item": [{ "name": "Ping", "request": "{{baseUrl}}/v1/ping" }]
    }"#;

    let parsed = parse_collection(json.as_bytes(), &[]).unwrap();
    let request = &parsed.collection.requests()[0].request;

    assert_eq!(request.method, "GET");
    assert_eq!(request.url, "{{baseUrl}}/v1/ping");
    assert!(parsed.warnings.iter().any(|w| w.contains("assumed GET")));
}

#[test]
fn a_file_that_is_not_a_collection_is_rejected() {
    let error = parse_collection(br#"{"hello":"world"}"#, &[]).unwrap_err();
    assert!(error.contains("Postman"), "{error}");
}

// ---------------------------------------------------------------- the layout

#[test]
fn a_collection_becomes_one_file_per_request() {
    let rendered = render(&parsed(), &[]);
    let root = "collections/acme-api";

    assert!(
        rendered
            .files
            .contains_key(&format!("{root}/collection.yml")),
        "{:?}",
        rendered.files.keys().collect::<Vec<_>>()
    );
    // Numbering carries the interleaved order of folders and requests.
    assert!(rendered
        .files
        .contains_key(&format!("{root}/01-list-orders.yml")));
    assert!(rendered
        .files
        .contains_key(&format!("{root}/02-admin/folder.yml")));
    assert!(rendered
        .files
        .contains_key(&format!("{root}/02-admin/01-create-order.yml")));
    assert!(rendered.files.contains_key(&format!("{root}/03-ping.yml")));

    assert_eq!(rendered.report.collection_dir, root);
    assert_eq!(rendered.report.requests, 3);
    assert_eq!(rendered.report.folders, 1);
}

#[test]
fn a_request_file_is_readable_yaml_with_the_url_intact() {
    let rendered = render(&parsed(), &[]);
    let text = &rendered.files["collections/acme-api/01-list-orders.yml"];

    // An unquoted `{{baseUrl}}/...` is a YAML flow mapping, so if the emitter
    // ever stops quoting it, this parse fails rather than silently mangling it.
    let value: serde_json::Value = serde_norway::from_str(text).expect("valid YAML");
    assert_eq!(value["name"], "List orders");
    assert_eq!(value["method"], "GET");
    assert_eq!(value["url"], "{{baseUrl}}/v1/orders");
    // The query is a list so the window can toggle one parameter off.
    assert_eq!(value["params"][0]["name"], "page");
    assert_eq!(value["params"][0]["value"], "2");
    assert_eq!(value["headers"][1]["disabled"], true);
}

#[test]
fn auth_is_written_only_where_it_is_set() {
    let rendered = render(&parsed(), &[]);

    let list = &rendered.files["collections/acme-api/01-list-orders.yml"];
    assert!(
        !list.contains("auth"),
        "inheriting must not be materialised into the file: {list}"
    );

    let folder = &rendered.files["collections/acme-api/02-admin/folder.yml"];
    assert!(folder.contains("kind: apikey"), "{folder}");

    let create = &rendered.files["collections/acme-api/02-admin/01-create-order.yml"];
    assert!(!create.contains("auth"), "{create}");

    let ping = &rendered.files["collections/acme-api/03-ping.yml"];
    assert!(ping.contains("auth: none"), "{ping}");

    let meta = &rendered.files["collections/acme-api/collection.yml"];
    assert!(meta.contains("kind: bearer"), "{meta}");
}

#[test]
fn two_requests_with_the_same_name_get_different_files() {
    let mut collection = parsed();
    collection.items.push(Item {
        scripts: Default::default(),
        name: "List orders".to_string(),
        auth: AuthSetting::Inherit,
        variables: Vec::new(),
        request: Some(RequestDef {
            method: "GET".to_string(),
            url: "https://api.acme.test/v1/orders/second".to_string(),
            ..RequestDef::default()
        }),
        items: Vec::new(),
    });

    let rendered = render(&collection, &[]);

    assert!(rendered
        .files
        .contains_key("collections/acme-api/01-list-orders.yml"));
    assert!(rendered
        .files
        .contains_key("collections/acme-api/04-list-orders-2.yml"));
    assert_eq!(rendered.report.requests, 4);
}

#[test]
fn a_name_with_no_usable_characters_still_gets_a_directory() {
    let collection = Collection {
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        scripts: Default::default(),
        name: "///".to_string(),
        description: None,
        auth: AuthSetting::Inherit,
        variables: Vec::new(),
        items: Vec::new(),
    };
    let rendered = render(&collection, &[]);
    assert!(rendered
        .files
        .contains_key("collections/collection/collection.yml"));
}

// ---------------------------------------------------------------- secrets

#[test]
fn a_secret_value_never_reaches_a_committed_file() {
    let rendered = render(&parsed(), &[]);
    let committed = &rendered.files["collections/acme-api/collection.yml"];

    assert!(!committed.contains(TOKEN));
    // The name and the fact that it is secret must survive, otherwise the value
    // cannot be re-attached from `.secrets/` later.
    assert!(committed.contains("name: apiToken"));
    assert!(committed.contains("secret: true"));

    assert_eq!(
        rendered.files[".secrets/acme-api.env"].trim(),
        format!("apiToken={TOKEN}")
    );
    assert!(rendered.report.leaks.is_empty());
    assert_eq!(rendered.report.secret_variables, vec!["apiToken"]);
}

#[test]
fn the_example_file_lists_names_without_values() {
    let rendered = render(&parsed(), &[]);
    let example = &rendered.files["secrets.example.env"];

    assert!(example.contains("apiToken="), "{example}");
    assert!(!example.contains("tok_live"), "{example}");
    assert!(
        !example.contains("baseUrl"),
        "only secrets belong here: {example}"
    );
}

#[test]
fn extra_secret_names_can_be_marked_on_the_command_line() {
    let parsed = parse_collection(POSTMAN.as_bytes(), &["baseUrl".to_string()]).unwrap();
    let rendered = render(&parsed.collection, &[]);

    assert!(
        !rendered.files["collections/acme-api/collection.yml"].contains("https://api.acme.test")
    );
    assert!(rendered
        .report
        .secret_variables
        .contains(&"baseUrl".to_string()));
}

#[test]
fn environment_secrets_get_their_own_file() {
    let rendered = render(&parsed(), &[parsed_environment()]);

    let committed = &rendered.files["environments/dev.yml"];
    assert!(
        committed.contains("https://dev.acme.test"),
        "plain values stay"
    );
    assert!(!committed.contains("env-secret-value-123"));
    assert!(committed.contains("secret: true"));
    assert!(
        !committed.contains("name: off"),
        "disabled values are dropped"
    );

    assert_eq!(
        rendered.files[".secrets/dev.env"].trim(),
        "apiKey=env-secret-value-123"
    );
    assert!(rendered.files["secrets.example.env"].contains("# environment: dev"));
}

#[test]
fn writing_refuses_when_a_secret_would_leak() {
    // A secret value that also appears in a committed file is exactly the bug
    // the guard exists for.
    let collection = Collection {
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        scripts: Default::default(),
        name: "leaky".to_string(),
        description: None,
        auth: AuthSetting::Inherit,
        variables: vec![Variable {
            name: "token".to_string(),
            value: Some("longsecretvalue".to_string()),
            secret: true,
        }],
        items: vec![Item {
            name: "r".to_string(),
            scripts: Default::default(),
            auth: AuthSetting::Inherit,
            variables: Vec::new(),
            request: Some(RequestDef {
                method: "GET".to_string(),
                url: "https://api.test".to_string(),
                headers: vec![KeyValue {
                    name: "Authorization".to_string(),
                    value: "longsecretvalue".to_string(),
                    disabled: false,
                }],
                ..RequestDef::default()
            }),
            items: Vec::new(),
        }],
    };

    let rendered = render(&collection, &[]);
    assert_eq!(rendered.report.leaks.len(), 1);
    assert!(rendered.report.leaks[0].contains("01-r.yml"));

    let dir = temp_dir("leak");
    let error = write(&dir, &rendered).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!dir.join("collections/leaky/01-r.yml").exists());

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------- round trip

#[test]
fn a_workspace_round_trips_through_the_filesystem() {
    let dir = temp_dir("roundtrip");
    let original = parsed();
    let environments = vec![parsed_environment()];

    let rendered = render(&original, &environments);
    write(&dir, &rendered).expect("write");

    let loaded = load(&dir).expect("load");

    assert_eq!(loaded.collections.len(), 1);
    assert_eq!(
        loaded.collections[0], original,
        "collection must survive the split, auth settings included"
    );
    assert_eq!(loaded.environments.len(), 1);
    assert_eq!(loaded.environments[0], environments[0]);

    // Inheritance must still resolve to the same thing after a round trip.
    assert_eq!(
        effective_auth(&loaded.collections[0]),
        vec![Some("bearer".to_string()), Some("apikey".to_string()), None]
    );

    // The value came back from `.secrets/`, not from a committed file.
    let token = loaded.collections[0]
        .variables
        .iter()
        .find(|v| v.name == "apiToken")
        .unwrap();
    assert_eq!(token.value.as_deref(), Some(TOKEN));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_a_missing_workspace_yields_nothing() {
    let dir = temp_dir("empty");
    let loaded = load(&dir).expect("load");
    assert!(loaded.collections.is_empty());
    assert!(loaded.environments.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn writing_creates_the_tree_and_ignores_the_secrets() {
    let dir = temp_dir("workspace");
    let rendered = render(&parsed(), &[]);
    write(&dir, &rendered).expect("write");

    assert!(dir.join("collections/acme-api/collection.yml").exists());
    assert!(dir
        .join("collections/acme-api/02-admin/folder.yml")
        .exists());
    assert!(dir.join(".secrets/acme-api.env").exists());

    let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert_eq!(gitignore.trim(), "/.secrets/");

    // Nothing a commit would pick up may contain the token.
    for relative in rendered
        .files
        .keys()
        .filter(|p| !p.starts_with(".secrets/"))
    {
        let text = std::fs::read_to_string(dir.join(relative)).unwrap();
        assert!(!text.contains(TOKEN), "{relative}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_existing_gitignore_is_kept_and_not_duplicated() {
    let dir = temp_dir("gitignore");
    std::fs::write(dir.join(".gitignore"), "/target\nbig.json").unwrap();

    assert!(ensure_gitignore(&dir).expect("first"));
    let after = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(after.starts_with("/target\nbig.json\n"), "{after}");
    assert!(after.contains("/.secrets/"));

    assert!(
        !ensure_gitignore(&dir).expect("second"),
        "must be idempotent"
    );
    let again = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert_eq!(again.matches("/.secrets/").count(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_existing_gitattributes_is_merged() {
    let dir = temp_dir("gitattributes");
    std::fs::write(dir.join(".gitattributes"), "*.png binary").unwrap();

    assert!(ensure_gitattributes(&dir).expect("first"));
    let after = std::fs::read_to_string(dir.join(".gitattributes")).unwrap();
    assert!(after.starts_with("*.png binary\n"), "{after}");
    assert!(after.contains("* text=auto eol=lf"));

    assert!(
        !ensure_gitattributes(&dir).expect("second"),
        "must be idempotent"
    );
    let again = std::fs::read_to_string(dir.join(".gitattributes")).unwrap();
    assert_eq!(again.matches("* text=auto eol=lf").count(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------- helpers

#[test]
fn env_values_survive_quoting() {
    for value in [
        "simple",
        "with spaces",
        "quote\"inside",
        "new\nline",
        "hash#comment",
        "back\\slash",
        "unicode-→-ok",
        "",
    ] {
        let line = format!("KEY={}\n", quote_env(value));
        let parsed = parse_env(&line);
        assert_eq!(
            parsed,
            vec![("KEY".to_string(), value.to_string())],
            "{line}"
        );
    }
}

#[test]
fn env_comments_and_blank_lines_are_skipped() {
    let parsed = parse_env("# comment\n\nA=1\nB=\"two words\"\n");
    assert_eq!(
        parsed,
        vec![
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "two words".to_string())
        ]
    );
}

#[test]
fn names_become_file_safe_slugs() {
    assert_eq!(slugify("dev"), "dev");
    assert_eq!(slugify("Staging EU (west)"), "staging-eu-west");
    assert_eq!(slugify("///"), "");
}

#[test]
fn postman_globals_parse_secrets_disabled_and_structured_values() {
    let parsed = parse_globals(
        br#"{"_postman_variable_scope":"globals","values":[
          {"key":"origin","value":"https://api.test"},
          {"key":"token","value":"global-secret-value","type":"secret"},
          {"key":"password","value":"extra-secret-value"},
          {"key":"off","value":"ignored","enabled":false},
          {"key":"object","value":{"x":1}}
        ]}"#,
        &["password".to_string()],
    )
    .unwrap();
    assert_eq!(parsed.variables.len(), 4);
    assert!(parsed.variables[1].secret);
    assert!(parsed.variables[2].secret);
    assert_eq!(parsed.variables[3].value.as_deref(), Some(r#"{"x":1}"#));
    assert_eq!(parsed.warnings.len(), 1);
}

#[test]
fn variable_exports_reject_the_wrong_type_or_missing_values() {
    assert!(parse_globals(ENVIRONMENT.as_bytes(), &[]).is_err());
    assert!(parse_globals(br#"{"_postman_variable_scope":"globals"}"#, &[]).is_err());
    assert!(parse_environment(br#"{"name":"not an environment"}"#, &[]).is_err());
    assert!(parse_environment(
        br#"{"name":"globals","values":[],"_postman_variable_scope":"globals"}"#,
        &[]
    )
    .is_err());
}

#[test]
fn desktop_import_creates_yaml_and_preserves_other_workspace_files() {
    let dir = temp_dir("desktop-import");
    std::fs::write(dir.join("notes.txt"), "user file").unwrap();
    std::fs::write(dir.join("secrets.example.env"), "# existing examples\n").unwrap();
    import_postman(&dir, "collection", POSTMAN.as_bytes(), &[]).unwrap();
    import_postman(&dir, "environment", ENVIRONMENT.as_bytes(), &[]).unwrap();
    import_postman(
        &dir,
        "globals",
        br#"{"_postman_variable_scope":"globals","values":[
        {"key":"origin","value":"https://api.test"},
        {"key":"globalToken","value":"global-secret-value","type":"secret"}
    ]}"#,
        &[],
    )
    .unwrap();

    let workspace = load(&dir).unwrap();
    assert_eq!(workspace.collections[0], parsed());
    assert_eq!(workspace.environments[0], parsed_environment());
    assert_eq!(
        workspace.globals[1].value.as_deref(),
        Some("global-secret-value")
    );
    assert!(dir
        .join("collections/acme-api/02-admin/01-create-order.yml")
        .is_file());
    assert!(dir.join("environments/dev.yml").is_file());
    assert!(!std::fs::read_to_string(dir.join("globals.yml"))
        .unwrap()
        .contains("global-secret-value"));
    assert!(std::fs::read_to_string(dir.join(".secrets/globals.env"))
        .unwrap()
        .contains("global-secret-value"));
    assert!(std::fs::read_to_string(dir.join(".gitignore"))
        .unwrap()
        .contains("/.secrets/"));
    assert!(dir.join(".gitattributes").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
        "user file"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("secrets.example.env")).unwrap(),
        "# existing examples\n"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn desktop_import_refuses_existing_collections_and_environments() {
    let dir = temp_dir("desktop-collisions");
    import_postman(&dir, "collection", POSTMAN.as_bytes(), &[]).unwrap();
    import_postman(&dir, "environment", ENVIRONMENT.as_bytes(), &[]).unwrap();
    let before = load(&dir).unwrap();
    assert!(import_postman(&dir, "collection", POSTMAN.as_bytes(), &[])
        .unwrap_err()
        .contains("overwrite"));
    assert!(
        import_postman(&dir, "environment", ENVIRONMENT.as_bytes(), &[])
            .unwrap_err()
            .contains("already exists")
    );
    let after = load(&dir).unwrap();
    assert_eq!(before.collections, after.collections);
    assert_eq!(before.environments, after.environments);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn desktop_globals_import_merges_new_names_and_rejects_duplicates_without_changes() {
    let dir = temp_dir("desktop-globals");
    let first = br#"{"_postman_variable_scope":"globals","values":[{"key":"token","value":"stored-global-secret","type":"secret"}]}"#;
    import_postman(&dir, "globals", first, &[]).unwrap();
    let secret_path = dir.join(".secrets/globals.env");
    let modified = std::fs::metadata(&secret_path).unwrap().modified().unwrap();
    import_postman(&dir, "globals", br#"{"_postman_variable_scope":"globals","values":[{"key":"origin","value":"https://api.test"}]}"#, &[]).unwrap();
    assert_eq!(
        std::fs::metadata(&secret_path).unwrap().modified().unwrap(),
        modified
    );
    let before = std::fs::read(dir.join("globals.yml")).unwrap();
    assert!(import_postman(&dir, "globals", first, &[])
        .unwrap_err()
        .contains("already exists"));
    assert_eq!(std::fs::read(dir.join("globals.yml")).unwrap(), before);
    assert_eq!(load(&dir).unwrap().globals.len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn desktop_import_refuses_secret_leaks_before_writing() {
    let dir = temp_dir("desktop-leak");
    let unsafe_export = POSTMAN.replace("{{baseUrl}}/v1/ping", TOKEN);
    assert!(
        import_postman(&dir, "collection", unsafe_export.as_bytes(), &[])
            .unwrap_err()
            .contains("secret value")
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn desktop_import_refuses_shared_secret_file_collisions() {
    let dir = temp_dir("desktop-secret-collision");
    import_postman(&dir, "collection", POSTMAN.as_bytes(), &[]).unwrap();
    let environment = ENVIRONMENT.replace("\"dev\"", "\"Acme API\"");
    assert!(
        import_postman(&dir, "environment", environment.as_bytes(), &[])
            .unwrap_err()
            .contains(".secrets/acme-api.env")
    );
    assert!(!dir.join("environments").exists());
    assert_eq!(load(&dir).unwrap().collections[0], parsed());
    std::fs::remove_dir_all(dir).unwrap();
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "rq-collection-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}
