//! Desktop query variables are bound to parsed string literals, never query text.

use collection::send::Variables;
use response_query::literal::ScalarLiteral;
use response_query::{Condition, Diag, Field, Selection, Step};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Context {
    pub collection: String,
    pub path: Vec<usize>,
    pub environment: Option<String>,
    pub deployment: Option<collection::model::Deployment>,
}

impl Context {
    pub fn variables(&self, root: &std::path::Path) -> Result<Variables, String> {
        let located = super::locate(root, &self.collection, self.environment.as_deref())?;
        if located.collection.request_at(&self.path).is_none() {
            return Err("that query's request is no longer in the collection".into());
        }
        Ok(located
            .context(self.deployment.as_ref())?
            .variables(&self.path))
    }
}

pub(crate) fn parse(
    source: &str,
    load: impl FnOnce() -> Result<Variables, String>,
) -> Result<Selection, String> {
    let mut selection = response_query::parse(source).map_err(|diag| diag.render(source))?;
    let mut load = Some(load);
    let mut variables = None;
    let mut resolve = |text: &str| {
        if !text.contains("{{") {
            return Ok(text.to_string());
        }
        if variables.is_none() {
            variables = Some(load.take().expect("variables are loaded once")()?);
        }
        let variables = variables.as_ref().unwrap();
        let mut missing = Vec::new();
        let value = collection::send::substitute(text, variables, &mut missing);
        if !missing.is_empty() || !variables.problems.is_empty() {
            // Neither expanded values nor tokens found inside secret values
            // belong in diagnostics. Render against the original query only.
            return Err("query variable is undefined or cannot be resolved".into());
        }
        Ok(value)
    };
    bind_selection(&mut selection, &mut resolve).map_err(|diag| diag.render(source))?;
    Ok(selection)
}

fn bind_selection(
    selection: &mut Selection,
    resolve: &mut impl FnMut(&str) -> Result<String, String>,
) -> Result<(), Diag> {
    for field in &mut selection.fields {
        bind_field(field, resolve)?;
    }
    Ok(())
}

fn bind_field(
    field: &mut Field,
    resolve: &mut impl FnMut(&str) -> Result<String, String>,
) -> Result<(), Diag> {
    if let Some(condition) = &mut field.condition {
        let values = match condition {
            Condition::Equal(value) | Condition::NotEqual(value) => std::slice::from_mut(value),
            Condition::In(values) | Condition::NotIn(values) => values.as_mut_slice(),
        };
        for value in values {
            if let ScalarLiteral::String(text) = value {
                *text = resolve(text).map_err(|message| Diag::new(message, field.span))?;
            }
        }
    }
    for step in &mut field.steps {
        match step {
            Step::Filter(predicate) => bind_field(predicate, resolve)?,
            Step::Collect(selection) => bind_selection(selection, resolve)?,
            Step::Slice(_) => {}
        }
    }
    if let Some(selection) = &mut field.selection {
        bind_selection(selection, resolve)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn variables(values: &[(&str, &str)]) -> Variables {
        values
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>()
            .into()
    }

    fn run(query: &str, body: &str, values: &[(&str, &str)]) -> String {
        let selection = parse(query, || Ok(variables(values))).unwrap();
        response_query::project_to_string(body.as_bytes(), &selection).unwrap()
    }

    #[test]
    fn variables_bind_nested_predicates_and_membership_as_strings() {
        let query = r#"{ items[? definition { item_id { value == "{{myVar}}" } }] { definition { item_id attributes } } }"#;
        let body = r#"{"items":[{"definition":{"item_id":{"value":"2762467"},"attributes":[1]}},{"definition":{"item_id":{"value":2762467}}}]}"#;
        assert_eq!(
            run(query, body, &[("myVar", "2762467")]),
            r#"{"items":[{"definition":{"item_id":{"value":"2762467"},"attributes":[1]}}]}"#
        );
        let body = r#"{"items":[{"id":"Mike"},{"id":"X"},{"id":"other"}]}"#;
        assert_eq!(
            run(
                r#"{ items[? id in ["{{first}}", "{{second}}"]][id] }"#,
                body,
                &[("first", "Mike"), ("second", "X")]
            ),
            r#"{"items":["Mike","X"]}"#
        );
        assert_eq!(
            run(
                r#"{ items { id not in ["{{first}}"] } }"#,
                body,
                &[("first", "Mike")]
            ),
            r#"{"items":[{},{"id":"X"},{"id":"other"}]}"#
        );
    }

    #[test]
    fn variable_values_cannot_inject_query_syntax_and_support_escaped_strings() {
        let value = "Mike\" } }] { * !secret... } #\\\n雪";
        let body =
            serde_json::json!({"items":[{"name":value,"id":1,"secret":"hidden"}]}).to_string();
        assert_eq!(
            run(
                r#"{ items[? name == "{{name}}"] { id } }"#,
                &body,
                &[("name", value)]
            ),
            r#"{"items":[{"id":1}]}"#
        );
        assert_eq!(
            run(
                r#"{ name == "prefix-{{ name }}-suffix" }"#,
                r#"{"name":"prefix-Mike-suffix"}"#,
                &[("name", "Mike")]
            ),
            r#"{"name":"prefix-Mike-suffix"}"#
        );
        assert_eq!(
            run(
                r#"{ name == "{{empty}}" }"#,
                r#"{"name":""}"#,
                &[("empty", "")]
            ),
            r#"{"name":""}"#
        );
    }

    #[test]
    fn variables_filter_recursive_children_in_flat_and_tree_views() {
        let body = r#"{"items":[{"definition":{"item_id":{"value":"other"}},"children":[{"definition":{"item_id":{"value":"2762467"},"attributes":[1]}}]}]}"#;
        for (mode, expected) in [
            (
                "...",
                r#"{"items":[{"definition":{"item_id":{"value":"2762467"},"attributes":[1]}}]}"#,
            ),
            (
                "**",
                r#"{"items":[{"children":[{"definition":{"item_id":{"value":"2762467"},"attributes":[1]}}]}]}"#,
            ),
        ] {
            let query = format!("{{ items{mode}children[? definition {{ item_id {{ value == \"{{{{myVar}}}}\" }} }}] {{ definition {{ item_id attributes }} }} }}");
            assert_eq!(run(&query, body, &[("myVar", "2762467")]), expected);
        }
    }

    #[test]
    fn comments_and_placeholder_free_queries_do_not_load_variables() {
        assert!(parse("{ # {{ignored}}\n id }", || panic!(
            "must not load variables"
        ))
        .is_ok());
        assert!(parse("{ items[? id == 1] { name } }", || panic!(
            "must not load variables"
        ))
        .is_ok());
        assert!(parse("invalid", || panic!(
            "parse errors must precede variable loading"
        ))
        .is_err());
    }

    #[test]
    fn unresolved_variables_report_original_locations_without_exposing_values() {
        let source = "{ items[? profile {\n name == \"{{secret}}\" }] { id } }";
        let error = parse(source, || {
            Ok(variables(&[("secret", "private-{{do-not-disclose}}")]))
        })
        .unwrap_err();
        assert!(error.contains("2:"));
        assert!(error.contains("undefined"));
        assert!(!error.contains("private-"));
        assert!(!error.contains("do-not-disclose"));
        assert!(parse(r#"{ name == "{{unknown}}" }"#, || Ok(variables(&[]))).is_err());
    }

    #[test]
    fn variables_are_loaded_once_for_all_condition_shapes() {
        let loads = std::cell::Cell::new(0);
        let source =
            r#"{ name != "{{id}}" items[name in ["{{id}}"]] children** { name == "{{id}}" } }"#;
        parse(source, || {
            loads.set(loads.get() + 1);
            Ok(variables(&[("id", "Mike")]))
        })
        .unwrap();
        assert_eq!(loads.get(), 1);
    }

    #[test]
    fn context_uses_current_scopes_composition_and_rust_only_secrets() {
        let root = std::env::temp_dir().join("opencode").join(format!(
            "rq-query-vars-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let collection = serde_norway::from_str(
            "name: Queries\nvariables:\n - {name: id, value: collection}\n - {name: composed, value: '{{origin}}/path'}\n - {name: token, value: rust-only-secret, secret: true}\nitems:\n - name: Folder\n   variables:\n    - {name: id, value: folder}\n   items:\n    - name: Request\n      request:\n       method: GET\n       url: https://api.example.test\n       variables:\n        - {name: id, value: request}\n").unwrap();
        let dev = serde_norway::from_str("name: dev\nvariables:\n - {name: origin, value: dev}\n")
            .unwrap();
        let stage =
            serde_norway::from_str("name: stage\nvariables:\n - {name: origin, value: stage}\n")
                .unwrap();
        collection::store::write(
            &root,
            &collection::store::render(&collection, &[dev, stage]),
        )
        .unwrap();
        collection::store::save_globals(
            &root,
            vec![collection::model::Variable {
                name: "globalOnly".into(),
                value: Some("global".into()),
                secret: false,
            }],
        )
        .unwrap();
        let mut context = Context {
            collection: "queries".into(),
            path: vec![0, 0],
            environment: Some("dev".into()),
            deployment: None,
        };
        let vars = context.variables(&root).unwrap();
        assert_eq!(vars["globalOnly"], "global");
        assert_eq!(vars["id"], "request");
        assert_eq!(vars["composed"], "dev/path");
        assert_eq!(vars["token"], "rust-only-secret");
        context.environment = Some("stage".into());
        assert_eq!(context.variables(&root).unwrap()["composed"], "stage/path");
        assert!(
            !std::fs::read_to_string(root.join("collections/queries/collection.yml"))
                .unwrap()
                .contains("rust-only-secret")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn query_context_requires_an_explicit_deployment_and_uses_generated_inputs() {
        let root = std::env::temp_dir().join("opencode").join(format!(
            "rq-query-target-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let collection: collection::Collection = serde_norway::from_str(
            "name: Queries\nservice_name: demo\ndeployments:\n - {tenant: tenant-a, platform: stage-a, slot: stable}\nvariables:\n - {name: tenant, value: wrong}\nitems:\n - name: Request\n   request:\n    method: GET\n    url: '{{baseUrl}}/items'\n").unwrap();
        let environment = serde_norway::from_str(
            "name: stage\nplatforms:\n - name: stage-a\n   url_template: 'https://{{serviceName}}.example.test/{{tenant}}/{{slot}}'\n").unwrap();
        collection::store::write(
            &root,
            &collection::store::render(&collection, &[environment]),
        )
        .unwrap();
        let mut context = Context {
            collection: "queries".into(),
            path: vec![0],
            environment: Some("stage".into()),
            deployment: None,
        };
        assert!(context.variables(&root).is_err());
        context.deployment = Some(collection.deployments[0].clone());
        let vars = context.variables(&root).unwrap();
        assert_eq!(vars["tenant"], "tenant-a");
        assert_eq!(vars["baseUrl"], "https://demo.example.test/tenant-a/stable");
        std::fs::remove_dir_all(root).unwrap();
    }
}
