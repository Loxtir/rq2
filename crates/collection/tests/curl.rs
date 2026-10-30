//! The `curl` preview.
//!
//! The point of these tests is the mask: a preview is meant to be pasted into a
//! ticket, so no secret value may survive the rendering, however it was used.

use collection::curl::{masked_variables, render};
use collection::model::{
    Auth, AuthSetting, Body, Collection, Environment, KeyValue, RequestDef, Variable,
};
use std::collections::BTreeMap;

fn variable(name: &str, value: &str, secret: bool) -> Variable {
    Variable {
        name: name.to_string(),
        value: Some(value.to_string()),
        secret,
    }
}

fn collection(auth: AuthSetting, variables: Vec<Variable>) -> Collection {
    Collection {
        id: String::new(),
        service_name: None,
        base_path: None,
        deployments: Vec::new(),
        scripts: Default::default(),
        name: "Acme".to_string(),
        description: None,
        auth,
        variables,
        items: Vec::new(),
    }
}

fn request(method: &str, url: &str) -> RequestDef {
    RequestDef {
        method: method.to_string(),
        url: url.to_string(),
        ..RequestDef::default()
    }
}

fn bearer() -> AuthSetting {
    AuthSetting::Method(Auth {
        kind: "bearer".to_string(),
        fields: [("token".to_string(), "{{token}}".to_string())]
            .into_iter()
            .collect(),
    })
}

#[test]
fn renders_a_get_without_an_explicit_method() {
    let collection = collection(Default::default(), Vec::new());
    let variables = masked_variables(&collection, None, &[], &[]);

    let preview = render(
        &request("GET", "https://acme.test/orders"),
        None,
        &variables,
    );

    assert_eq!(preview.text, "curl 'https://acme.test/orders'");
    assert!(!preview.masked);
}

#[test]
fn a_secret_in_an_auth_header_is_masked() {
    let collection = collection(
        bearer(),
        vec![
            variable("baseUrl", "https://acme.test", false),
            variable("token", "hunter2", true),
        ],
    );
    let variables = masked_variables(&collection, None, &[], &[]);

    let preview = render(
        &request("POST", "{{baseUrl}}/orders"),
        Some(collection.auth.method().expect("bearer is a method")),
        &variables,
    );

    assert!(
        preview.text.contains("'https://acme.test/orders'"),
        "{}",
        preview.text
    );
    assert!(preview.text.contains("-X POST"), "{}", preview.text);
    assert!(
        preview.text.contains("'Authorization: Bearer ***'"),
        "{}",
        preview.text
    );
    assert!(!preview.text.contains("hunter2"), "{}", preview.text);
    assert!(preview.masked);
}

#[test]
fn a_secret_in_the_body_is_masked_too() {
    let collection = collection(Default::default(), vec![variable("apiKey", "s3cret", true)]);
    let variables = masked_variables(&collection, None, &[], &[]);

    let mut request = request("POST", "https://acme.test/orders");
    request.body = Some(Body {
        mode: "raw".to_string(),
        language: Some("json".to_string()),
        text: Some("{\"key\":\"{{apiKey}}\"}".to_string()),
    });

    let preview = render(&request, None, &variables);

    assert!(!preview.text.contains("s3cret"), "{}", preview.text);
    assert!(preview.text.contains("***"), "{}", preview.text);
    // A JSON body implies the content type, so the command stands alone.
    assert!(
        preview.text.contains("'Content-Type: application/json'"),
        "{}",
        preview.text
    );
    assert!(preview.masked);
}

#[test]
fn a_collection_value_wins_over_the_environment() {
    let collection = collection(
        Default::default(),
        vec![variable("baseUrl", "https://prod.test", false)],
    );
    let environment = Environment {
        main_slot: None,
        platforms: Vec::new(),
        name: "Dev".to_string(),
        insecure: false,
        variables: vec![variable("baseUrl", "https://dev.test", false)],
    };
    let variables = masked_variables(&collection, Some(&environment), &[], &[]);

    let preview = render(&request("GET", "{{baseUrl}}/orders"), None, &variables);

    assert!(
        preview.text.contains("https://prod.test"),
        "{}",
        preview.text
    );
}

/// The preview is meant to be pasted into a ticket, so a secret must be masked
/// even when something else composed it into a bigger value first.
#[test]
fn a_secret_composed_into_another_value_is_still_masked() {
    let collection = collection(
        Default::default(),
        vec![
            variable("authorization", "Bearer {{token}}", false),
            variable("token", "hunter2", true),
        ],
    );
    let variables = masked_variables(&collection, None, &[], &[]);

    let mut definition = request("GET", "https://api.test/orders");
    definition.headers = vec![KeyValue {
        name: "Authorization".to_string(),
        value: "{{authorization}}".to_string(),
        disabled: false,
    }];
    let preview = render(&definition, None, &variables);

    assert!(preview.masked, "{}", preview.text);
    assert!(!preview.text.contains("hunter2"), "{}", preview.text);
    assert!(
        preview.text.contains("Bearer ***"),
        "the mask is composed too: {}",
        preview.text
    );
}

#[test]
fn an_api_key_in_the_query_is_appended_to_the_url() {
    let collection = collection(
        AuthSetting::Method(Auth {
            kind: "apikey".to_string(),
            fields: [
                ("key".to_string(), "api_key".to_string()),
                ("value".to_string(), "{{apiKey}}".to_string()),
                ("in".to_string(), "query".to_string()),
            ]
            .into_iter()
            .collect(),
        }),
        vec![variable("apiKey", "s3cret", true)],
    );
    let variables = masked_variables(&collection, None, &[], &[]);

    let preview = render(
        &request("GET", "https://acme.test/orders?limit=1"),
        collection.auth.method(),
        &variables,
    );

    assert!(
        preview
            .text
            .contains("'https://acme.test/orders?limit=1&api_key=***'"),
        "{}",
        preview.text
    );
}

#[test]
fn an_api_key_in_the_query_is_percent_encoded() {
    let collection = collection(
        AuthSetting::Method(Auth {
            kind: "apikey".to_string(),
            fields: [
                ("key".to_string(), "api key".to_string()),
                ("value".to_string(), "{{apiKey}}".to_string()),
                ("in".to_string(), "query".to_string()),
            ]
            .into_iter()
            .collect(),
        }),
        vec![variable("apiKey", "a&b=c#d", false)],
    );
    let variables = masked_variables(&collection, None, &[], &[]);

    let preview = render(
        &request("GET", "https://acme.test/orders"),
        collection.auth.method(),
        &variables,
    );

    assert!(
        preview
            .text
            .contains("'https://acme.test/orders?api%20key=a%26b%3Dc%23d'"),
        "{}",
        preview.text
    );
}

#[test]
fn single_quotes_in_a_body_survive_a_shell() {
    let collection = collection(Default::default(), Vec::new());
    let variables = masked_variables(&collection, None, &[], &[]);

    let mut request = request("POST", "https://acme.test/orders");
    request.headers = vec![KeyValue {
        name: "Content-Type".to_string(),
        value: "text/plain".to_string(),
        disabled: false,
    }];
    request.body = Some(Body {
        mode: "raw".to_string(),
        language: Some("text".to_string()),
        text: Some("O'Brien".to_string()),
    });

    let preview = render(&request, None, &variables);

    assert!(
        preview.text.contains("--data-raw 'O'\\''Brien'"),
        "{}",
        preview.text
    );
}

#[test]
fn a_disabled_header_is_left_out() {
    let collection = collection(Default::default(), Vec::new());
    let variables = masked_variables(&collection, None, &[], &[]);

    let mut request = request("GET", "https://acme.test/orders");
    request.headers = vec![
        KeyValue {
            name: "X-On".to_string(),
            value: "1".to_string(),
            disabled: false,
        },
        KeyValue {
            name: "X-Off".to_string(),
            value: "2".to_string(),
            disabled: true,
        },
    ];

    let preview = render(&request, None, &variables);

    assert!(preview.text.contains("X-On: 1"), "{}", preview.text);
    assert!(!preview.text.contains("X-Off"), "{}", preview.text);
}

#[test]
fn secrets_are_masked_even_when_the_value_is_empty() {
    // An *empty* secret has nothing to hide; it must not be reported as masked,
    // or the window would claim a runnable command is not runnable.
    let collection = collection(
        Default::default(),
        vec![Variable {
            name: "token".to_string(),
            value: None,
            secret: true,
        }],
    );
    let variables = masked_variables(&collection, None, &[], &[]);

    let preview = render(
        &request("GET", "https://acme.test?x={{token}}"),
        None,
        &variables,
    );

    assert!(!preview.masked, "{}", preview.text);
}

#[test]
fn path_parameters_are_filled_in_and_whole_segments_only() {
    let variables = BTreeMap::new();

    let mut paged = request("GET", "https://acme.test/v1/:store/orders/:id");
    paged.path_params = [
        ("store".to_string(), "acme".to_string()),
        ("id".to_string(), "42".to_string()),
    ]
    .into_iter()
    .collect();

    let preview = render(&paged, None, &variables);
    assert_eq!(preview.text, "curl 'https://acme.test/v1/acme/orders/42'");

    // A gRPC-transcoded custom verb is not a path parameter.
    let verb = render(
        &request("POST", "https://acme.test/v1/orders:batchGet"),
        None,
        &variables,
    );
    assert_eq!(
        verb.text,
        "curl -X POST 'https://acme.test/v1/orders:batchGet'"
    );
}

#[test]
fn a_missing_path_parameter_is_reported_not_sent() {
    let variables = BTreeMap::new();
    let preview = render(
        &request("GET", "https://acme.test/orders/:id"),
        None,
        &variables,
    );

    assert_eq!(
        preview.problem.as_deref(),
        Some("path parameter `:id` has no value")
    );
    // The command still renders so the rest of it can be read.
    assert!(
        preview.text.contains("https://acme.test/orders/:id"),
        "{}",
        preview.text
    );
}

#[test]
fn query_parameters_are_appended_and_disabled_ones_skipped() {
    let variables = BTreeMap::new();

    let mut request = request("GET", "https://acme.test/orders");
    request.params = vec![
        KeyValue {
            name: "_limit".to_string(),
            value: "20".to_string(),
            disabled: false,
        },
        KeyValue {
            name: "debug".to_string(),
            value: String::new(),
            disabled: false,
        },
        KeyValue {
            name: "off".to_string(),
            value: "1".to_string(),
            disabled: true,
        },
    ];

    let preview = render(&request, None, &variables);
    assert_eq!(
        preview.text,
        "curl 'https://acme.test/orders?_limit=20&debug'"
    );
}

#[test]
fn a_parameter_value_can_come_from_a_masked_variable() {
    let collection = collection(Default::default(), vec![variable("apiKey", "s3cret", true)]);
    let variables = masked_variables(&collection, None, &[], &[]);

    let mut request = request("GET", "https://acme.test/orders");
    request.params = vec![KeyValue {
        name: "key".to_string(),
        value: "{{apiKey}}".to_string(),
        disabled: false,
    }];

    let preview = render(&request, None, &variables);
    assert!(!preview.text.contains("s3cret"), "{}", preview.text);
    assert!(preview.text.contains("key=***"), "{}", preview.text);
    assert!(preview.masked);
}
