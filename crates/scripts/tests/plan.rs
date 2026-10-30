use collection::context::RequestContext;
use collection::live::Scope;
use collection::model::{Collection, Environment, RequestDef, Scripts};
use collection::store::Workspace;
use std::collections::BTreeMap;

fn collection() -> Collection {
    serde_norway::from_str(
        "name: Demo\nvariables:\n - {name: token, value: old, secret: true}\nitems:\n - name: Folder\n   items:\n    - name: Get\n      request:\n       method: GET\n       url: /get\n",
    )
    .unwrap()
}

fn environment() -> Environment {
    serde_norway::from_str("name: dev\n").unwrap()
}

fn pre_request(
    script: &str,
    environment: Option<&Environment>,
) -> Result<rq_scripts::Writes, String> {
    let collection = collection();
    let context = RequestContext::new(&collection, environment, &[], None).unwrap();
    let mut request = RequestDef {
        method: "GET".into(),
        url: "/get".into(),
        ..Default::default()
    };
    request.scripts.pre_request = script.into();
    let plan = rq_scripts::Plan::new(&context, &[0, 0], &request, &BTreeMap::new())?;
    plan.pre_request(&mut request, &mut BTreeMap::new())
}

#[test]
fn each_setter_writes_to_a_typed_scope() {
    let writes = pre_request(
        "pm.variables.set('a','1'); pm.collectionVariables.set('b','2'); \
         pm.environment.set('c','3'); pm.globals.set('d','4');",
        Some(&environment()),
    )
    .unwrap();
    let scope_of = |name: &str| {
        writes
            .iter()
            .find(|(_, values)| values.contains_key(name))
            .map(|(scope, _)| scope.clone())
            .unwrap()
    };
    assert_eq!(
        scope_of("a"),
        Scope::Item {
            collection: "demo".into(),
            path: vec![0, 0]
        }
    );
    assert_eq!(
        scope_of("b"),
        Scope::Collection {
            collection: "demo".into()
        }
    );
    assert_eq!(
        scope_of("c"),
        Scope::Environment {
            environment: "dev".into()
        }
    );
    assert_eq!(scope_of("d"), Scope::Globals);
}

#[test]
fn an_environment_write_needs_a_selected_environment() {
    let error = pre_request("pm.environment.set('c','3');", None).unwrap_err();
    assert!(error.contains("selected, saved environment"), "{error}");
}

#[test]
fn edits_keep_the_secret_flag_of_the_variable_they_replace() {
    let mut workspace = Workspace {
        collections: vec![collection()],
        environments: Vec::new(),
        globals: Vec::new(),
        scripts: BTreeMap::new(),
    };
    let writes = pre_request("pm.collectionVariables.set('token','new');", None).unwrap();
    let edits = rq_scripts::edits(&mut workspace, "demo", writes).unwrap();
    assert_eq!(edits.len(), 1);
    assert!(edits[0].variable.secret);
    assert_eq!(edits[0].variable.value.as_deref(), Some("new"));
}

#[test]
fn without_a_deployment_reserved_names_are_ordinary_variables() {
    let collection = collection();
    let context = RequestContext::new(&collection, None, &[], None).unwrap();
    let plan_with = |script: &str| {
        let mut request = RequestDef::default();
        request.scripts.pre_request = script.into();
        (
            rq_scripts::Plan::new(&context, &[0, 0], &request, &BTreeMap::new()).unwrap(),
            request,
        )
    };
    // Without a deployment nothing is reserved.
    let (plan, mut request) = plan_with("pm.variables.set('baseUrl', 'https://ordinary.test');");
    let mut variables = BTreeMap::new();
    plan.pre_request(&mut request, &mut variables).unwrap();
    assert_eq!(variables["baseUrl"], "https://ordinary.test");
}

#[test]
fn shared_scripts_run_before_the_layer_that_includes_them_and_write_to_its_scope() {
    let mut collection = collection();
    collection.scripts.include = vec!["milestones".into()];
    collection.scripts.pre_request =
        "pm.variables.set('order', pm.variables.get('order') + ',own');".into();
    let shared = BTreeMap::from([(
        "milestones".to_string(),
        Scripts {
            pre_request:
                "pm.variables.set('order', 'shared'); pm.collectionVariables.set('m', '7');".into(),
            ..Default::default()
        },
    )]);
    let context = RequestContext::new(&collection, None, &[], None).unwrap();
    let mut request = RequestDef::default();
    let plan = rq_scripts::Plan::new(&context, &[0, 0], &request, &shared).unwrap();
    let mut variables = BTreeMap::new();
    let writes = plan.pre_request(&mut request, &mut variables).unwrap();
    assert_eq!(variables["order"], "shared,own");
    assert!(writes.iter().any(|(scope, values)| *scope
        == Scope::Collection {
            collection: "demo".into()
        }
        && values.get("m").map(String::as_str) == Some("7")));

    let error = rq_scripts::Plan::new(&context, &[0, 0], &request, &BTreeMap::new())
        .err()
        .unwrap();
    assert!(error.contains("scripts/milestones.yml"), "{error}");

    let broken = BTreeMap::from([(
        "milestones".to_string(),
        Scripts {
            pre_request: "throw new Error('boom')".into(),
            ..Default::default()
        },
    )]);
    let plan = rq_scripts::Plan::new(&context, &[0, 0], &request, &broken).unwrap();
    let error = plan
        .pre_request(&mut request, &mut BTreeMap::new())
        .unwrap_err();
    assert!(error.contains("shared script `milestones`"), "{error}");
}
