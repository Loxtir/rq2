//! Variable resolution and the `insecure` switch, tested without a network.
//!
//! The HTTP call itself is exercised by hand against a local server; what is
//! worth pinning down here is the substitution rules, because getting them
//! wrong silently changes which endpoint a request hits.

use collection::model::{Collection, Environment, Item, RequestDef, Variable};
use collection::send::{substitute, variables};
use std::collections::BTreeMap;

fn var(name: &str, value: Option<&str>) -> Variable {
    Variable {
        name: name.to_string(),
        value: value.map(str::to_string),
        secret: false,
    }
}

fn collection(variables: Vec<Variable>) -> Collection {
    Collection {
        id: String::new(),
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        scripts: Default::default(),
        name: "Test".to_string(),
        description: None,
        auth: Default::default(),
        variables,
        items: vec![Item {
            id: String::new(),
            name: "Request".to_string(),
            auth: Default::default(),
            variables: Vec::new(),
            scripts: Default::default(),
            request: Some(RequestDef {
                method: "GET".to_string(),
                url: "{{baseUrl}}/posts".to_string(),
                ..RequestDef::default()
            }),
            items: Vec::new(),
        }],
    }
}

fn environment(name: &str, insecure: bool, variables: Vec<Variable>) -> Environment {
    Environment {
        main_slot: None,
        platforms: Vec::new(),
        name: name.to_string(),
        insecure,
        variables,
    }
}

#[test]
fn substitutes_every_occurrence() {
    let mut vars = BTreeMap::new();
    vars.insert("host".to_string(), "example.test".to_string());

    let mut missing = Vec::new();
    assert_eq!(
        substitute("{{host}}/a/{{host}}", &vars, &mut missing),
        "example.test/a/example.test"
    );
    assert!(missing.is_empty());
}

#[test]
fn unknown_variable_is_left_verbatim_and_reported_once() {
    let vars = BTreeMap::new();
    let mut missing = Vec::new();

    // Blanking it would send a request to a different URL than the one shown.
    assert_eq!(
        substitute("{{baseUrl}}/a/{{baseUrl}}", &vars, &mut missing),
        "{{baseUrl}}/a/{{baseUrl}}"
    );
    assert_eq!(missing, vec!["baseUrl".to_string()]);
}

#[test]
fn whitespace_inside_braces_is_ignored() {
    let mut vars = BTreeMap::new();
    vars.insert("host".to_string(), "h".to_string());

    let mut missing = Vec::new();
    assert_eq!(substitute("{{ host }}", &vars, &mut missing), "h");
}

#[test]
fn unterminated_braces_are_kept() {
    let vars = BTreeMap::new();
    let mut missing = Vec::new();

    assert_eq!(substitute("{{oops", &vars, &mut missing), "{{oops");
    assert!(missing.is_empty());
}

#[test]
fn a_collection_value_overrides_the_environment_but_leaves_the_rest() {
    let collection = collection(vec![
        var("baseUrl", Some("https://collection.test")),
        var("onlyCollection", Some("c")),
    ]);
    let environment = environment(
        "dev",
        false,
        vec![
            var("baseUrl", Some("http://localhost:1")),
            var("onlyEnvironment", Some("e")),
        ],
    );

    let resolved = variables(&collection, Some(&environment), &[], &[]);
    assert_eq!(
        resolved.get("baseUrl").map(String::as_str),
        Some("https://collection.test")
    );
    // A value the collection does not mention still comes from the environment.
    assert_eq!(
        resolved.get("onlyEnvironment").map(String::as_str),
        Some("e")
    );
    assert_eq!(
        resolved.get("onlyCollection").map(String::as_str),
        Some("c")
    );
}

/// A collection value may be built from a wider layer: `baseUrl` composes
/// `origin`, which only `globals.yml` defines.
#[test]
fn a_collection_value_can_compose_a_global() {
    let collection = collection(vec![var("baseUrl", Some("{{origin}}/v1"))]);
    let globals = vec![var("origin", Some("https://api.test"))];

    let resolved = variables(&collection, None, &globals, &[0]);
    assert_eq!(
        resolved.get("baseUrl").map(String::as_str),
        Some("https://api.test/v1")
    );
}

/// Layers nest as well as override: the request's reference resolves against
/// the folder's value, which itself resolves against the environment's.
#[test]
fn composition_follows_the_scope_chain() {
    let mut collection = collection(vec![var("host", Some("https://collection.test"))]);
    collection.items = vec![Item {
        id: String::new(),
        name: "Folder".to_string(),
        auth: Default::default(),
        variables: vec![var("host", Some("https://{{region}}.test"))],
        scripts: Default::default(),
        request: None,
        items: vec![Item {
            id: String::new(),
            name: "Request".to_string(),
            auth: Default::default(),
            variables: Vec::new(),
            scripts: Default::default(),
            request: Some(RequestDef {
                method: "GET".to_string(),
                url: "{{host}}/orders".to_string(),
                ..RequestDef::default()
            }),
            items: Vec::new(),
        }],
    }];
    let environment = environment("dev", false, vec![var("region", Some("eu"))]);

    let resolved = variables(&collection, Some(&environment), &[], &[0, 0]);
    assert_eq!(
        resolved.get("host").map(String::as_str),
        Some("https://eu.test")
    );
}

#[test]
fn the_narrowest_layer_wins_at_every_step() {
    let environment = environment("dev", false, vec![var("who", Some("environment"))]);
    let globals = vec![var("who", Some("globals"))];
    let mut collection = collection(vec![var("who", Some("collection"))]);
    collection.items = vec![Item {
        id: String::new(),
        name: "Folder".to_string(),
        auth: Default::default(),
        variables: vec![var("who", Some("folder"))],
        scripts: Default::default(),
        request: None,
        items: vec![Item {
            id: String::new(),
            name: "Request".to_string(),
            auth: Default::default(),
            variables: Vec::new(),
            scripts: Default::default(),
            request: Some(RequestDef {
                method: "GET".to_string(),
                url: "/".to_string(),
                variables: vec![var("who", Some("request"))],
                ..RequestDef::default()
            }),
            items: Vec::new(),
        }],
    }];

    let who = |collection: &Collection, environment: Option<&Environment>| {
        variables(collection, environment, &globals, &[0, 0])
            .get("who")
            .cloned()
            .expect("a value")
    };

    assert_eq!(who(&collection, Some(&environment)), "request");

    collection.items[0].items[0]
        .request
        .as_mut()
        .expect("request")
        .variables
        .clear();
    assert_eq!(who(&collection, Some(&environment)), "folder");

    collection.items[0].variables.clear();
    assert_eq!(who(&collection, Some(&environment)), "collection");

    collection.variables.clear();
    assert_eq!(who(&collection, Some(&environment)), "environment");

    // The lowest layer is the fallback when nothing narrower speaks.
    assert_eq!(who(&collection, None), "globals");
}

#[test]
fn a_loop_is_reported_and_left_in_place() {
    let collection = collection(vec![var("a", Some("{{b}}")), var("b", Some("{{a}}"))]);

    let resolved = variables(&collection, None, &[], &[]);

    assert!(!resolved.problems.is_empty(), "a loop is reported");
    assert_eq!(resolved.get("a").map(String::as_str), Some("{{a}}"));

    let mut missing = Vec::new();
    assert_eq!(substitute("{{a}}", &resolved, &mut missing), "{{a}}");
    assert!(missing.contains(&"a".to_string()));
}

#[test]
fn secrets_and_undefined_values_are_skipped() {
    let collection = collection(vec![var("declared", None)]);
    let resolved = variables(&collection, None, &[], &[]);

    // A secret with no value in memory must not become an empty string.
    assert!(!resolved.contains_key("declared"));
}

#[test]
fn insecure_is_per_environment_and_defaults_off() {
    let verified = environment("prod", false, Vec::new());
    let unverified = environment("dev", true, Vec::new());

    assert!(!verified.insecure);
    assert!(unverified.insecure);
}

#[test]
fn insecure_survives_a_round_trip_through_disk() {
    let root = temp_dir("insecure");
    std::fs::create_dir_all(root.join("environments")).expect("environments dir");

    std::fs::write(
        root.join("environments/dev.yml"),
        "name: dev\ninsecure: true\nvariables:\n- name: token\n  value: abc\n",
    )
    .expect("write env");

    let workspace = collection::store::load(&root).expect("load");
    assert_eq!(workspace.environments.len(), 1);

    let dev = &workspace.environments[0];
    assert!(dev.insecure);
    assert_eq!(dev.variables.len(), 1);

    // And an environment that does not mention it still verifies.
    std::fs::write(root.join("environments/prod.yml"), "name: prod\n").expect("write env");
    let workspace = collection::store::load(&root).expect("load");
    let prod = workspace
        .environments
        .iter()
        .find(|environment| environment.name == "prod")
        .expect("prod environment");
    assert!(!prod.insecure);

    std::fs::remove_dir_all(&root).ok();
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("rq-send-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}
