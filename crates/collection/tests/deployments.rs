use collection::deployment::{self, resolve};
use collection::model::{Collection, Deployment, Environment, Variable};
use collection::{curl, send, store};

fn service() -> Collection {
    serde_norway::from_str(
        "name: Navigation Core\nservice_name: navsvc\ndeployments:\n  - {tenant: acme, platform: stage-a, slot: stable}\n  - {tenant: acme, platform: stage-a, slot: candidate}\n  - {tenant: globex, platform: stage-b, slot: stable}\n  - {tenant: acme, platform: prod-a, slot: stable}\nitems:\n  - name: Menu\n    request:\n      method: GET\n      url: '{{baseUrl}}/menu'\n",
    ).unwrap()
}

fn environments() -> Vec<Environment> {
    serde_norway::from_str(
        "- name: stage\n  platforms:\n    - name: stage-a\n      url_template: 'https://{{serviceName}}-{{slot}}.stage-a.test/{{tenant}}/'\n    - name: stage-b\n      url_template: 'https://{{tenant}}.stage-b.test/{{serviceName}}/{{slot}}'\n- name: production\n  platforms:\n    - name: prod-a\n      url_template: 'https://{{serviceName}}.{{platform}}.test/{{tenant}}/{{slot}}'\n",
    ).unwrap()
}

fn path_service() -> Collection {
    let mut service = service();
    service.service_name = Some("shopsvc".into());
    service.base_path = Some("/{{tenant == 'north' ? serviceName : 'legacysvc'}}-{{slot}}".into());
    service.deployments = ["north", "south", "east"]
        .into_iter()
        .map(|tenant| Deployment {
            tenant: tenant.into(),
            platform: "stage-a".into(),
            slot: "stage".into(),
        })
        .collect();
    service
}

fn path_environment() -> Environment {
    let mut env = environments().remove(0);
    env.platforms[0].url_template =
        "https://{{serviceName}}.{{tenant}}.{{platform}}.example.test{{basePath}}".into();
    env
}

#[test]
fn base_path_changes_only_the_path_occurrence_of_the_service_name() {
    let mut service = path_service();
    let env = path_environment();
    deployment::validate(&[service.clone()], std::slice::from_ref(&env)).unwrap();
    service.variables.push(Variable {
        name: "basePath".into(),
        value: Some("/wrong".into()),
        secret: true,
    });
    for (index, expected) in [(0, "shopsvc"), (1, "legacysvc"), (2, "legacysvc")] {
        let choice = &service.deployments[index];
        let target = resolve(&service, Some(&env), Some(choice))
            .unwrap()
            .unwrap();
        assert_eq!(target.service_name, "shopsvc");
        assert_eq!(target.tenant, choice.tenant);
        assert_eq!(target.base_path, format!("/{expected}-stage"));
        assert_eq!(
            target.base_url,
            format!(
                "https://shopsvc.{}.stage-a.example.test/{expected}-stage",
                choice.tenant
            )
        );
        let actual = send::variables_for_target(&service, Some(&env), &[], &[0], Some(&target));
        let masked =
            curl::masked_variables_for_target(&service, Some(&env), &[], &[0], Some(&target));
        assert_eq!(actual["basePath"], target.base_path);
        assert_eq!(masked["basePath"], target.base_path);
        assert_eq!(actual["serviceName"], "shopsvc");
        let preview = curl::render(service.requests()[0].request, None, &masked);
        assert!(preview.text.contains(&format!("{}/menu", target.base_url)));
        assert!(!preview.text.contains("wrong"));
    }
}

#[test]
fn base_paths_cannot_change_the_origin_or_introduce_query_parameters() {
    let env = path_environment();
    for source in [
        "relative",
        "//other.test/path",
        "https://other.test",
        "/path?query=1",
        "/path#fragment",
        "/path\\other",
        "/white space",
        "/{{unknown}}",
        "/{{basePath}}",
        "/{{tenant == 'north' ? 'ok' : secret}}",
    ] {
        let mut service = path_service();
        service.base_path = Some(source.into());
        assert!(
            deployment::validate(&[service.clone()], std::slice::from_ref(&env)).is_err(),
            "{source}"
        );
        assert!(
            resolve(&service, Some(&env), Some(&service.deployments[0])).is_err(),
            "{source}"
        );
    }
    let service = path_service();
    assert!(resolve(
        &service,
        Some(&environments()[0]),
        Some(&service.deployments[0])
    )
    .unwrap_err()
    .contains("{{basePath}}"));
}

#[test]
fn deployment_example_loads_and_previews_each_configured_target_without_network() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/deployments");
    let workspace = store::load(&root).unwrap();
    assert_eq!(workspace.collections.len(), 2);
    assert_eq!(workspace.environments.len(), 2);
    let service = workspace
        .collections
        .iter()
        .find(|collection| collection.name == "Navigation Demo")
        .unwrap();
    assert_eq!(service.deployments.len(), 5);
    let request = service.requests()[0].request;
    for (index, environment, expected) in [
        (
            0,
            "stage",
            "https://navigation-demo-stable.stage-a.example.test/acme",
        ),
        (
            1,
            "stage",
            "https://navigation-demo-candidate.stage-a.example.test/acme",
        ),
        (
            2,
            "stage",
            "https://globex.stage-b.example.test/navigation-demo/stable",
        ),
        (
            3,
            "production",
            "https://navigation-demo.prod-a.example.test/acme/stable",
        ),
        (
            4,
            "production",
            "https://globex-navigation-demo.prod-b.example.test/stable",
        ),
    ] {
        let env = workspace
            .environments
            .iter()
            .find(|env| env.name == environment)
            .unwrap();
        let target = resolve(service, Some(env), Some(&service.deployments[index]))
            .unwrap()
            .unwrap();
        assert_eq!(target.base_url, expected);
        let variables = curl::masked_variables_for_target(
            service,
            Some(env),
            &workspace.globals,
            &[0],
            Some(&target),
        );
        let preview = curl::render(request, None, &variables);
        assert!(preview
            .text
            .contains(&format!("{expected}/navigation/menu")));
        assert!(preview.problem.is_none());
        assert!(!preview.text.contains("{{"));
    }
}

#[test]
fn conditional_path_example_keeps_the_host_service_and_request_endpoint_unchanged() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/deployments");
    let workspace = store::load(&root).unwrap();
    let collection = workspace
        .collections
        .iter()
        .find(|collection| collection.name == "Catalog Demo")
        .unwrap();
    let environment = workspace
        .environments
        .iter()
        .find(|env| env.name == "stage")
        .unwrap();
    for (choice, path_name) in collection.deployments.iter().zip(["shopsvc", "legacysvc"]) {
        let target = resolve(collection, Some(environment), Some(choice))
            .unwrap()
            .unwrap();
        let variables = curl::masked_variables_for_target(
            collection,
            Some(environment),
            &[],
            &[0],
            Some(&target),
        );
        let request = collection.requests()[0].request;
        let preview = curl::render(request, None, &variables);
        assert!(preview.text.contains(&format!("https://shopsvc.{}.stage-c.example.test/{path_name}-stage/catalogs/demo-catalog/categories", choice.tenant)));
        assert!(preview.problem.is_none());
        assert_eq!(request.url, "{{baseUrl}}/catalogs/:catalogId/categories");
    }
}

#[test]
fn templates_follow_the_selected_tenant_platform_and_slot() {
    let service = service();
    let envs = environments();
    deployment::validate(std::slice::from_ref(&service), &envs).unwrap();
    for (index, env, expected) in [
        (0, 0, "https://navsvc-stable.stage-a.test/acme"),
        (1, 0, "https://navsvc-candidate.stage-a.test/acme"),
        (2, 0, "https://globex.stage-b.test/navsvc/stable"),
        (3, 1, "https://navsvc.prod-a.test/acme/stable"),
    ] {
        let target = resolve(
            &service,
            Some(&envs[env]),
            Some(&service.deployments[index]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(target.base_url, expected);
        assert_eq!(target.environment, envs[env].name);
    }
}

#[test]
fn missing_stale_and_cross_environment_choices_never_fall_back() {
    let service = service();
    let envs = environments();
    assert!(resolve(&service, None, Some(&service.deployments[0])).is_err());
    assert!(resolve(&service, Some(&envs[0]), None).is_err());
    assert!(resolve(&service, Some(&envs[1]), Some(&service.deployments[0])).is_err());
    let other = Deployment {
        tenant: "globex".into(),
        ..service.deployments[0].clone()
    };
    assert!(resolve(&service, Some(&envs[0]), Some(&other)).is_err());
    let mut missing = service.deployments[0].clone();
    missing.platform = "removed-platform".into();
    let mut service = service;
    service.deployments.push(missing.clone());
    assert!(resolve(&service, Some(&envs[0]), Some(&missing)).is_err());
}

#[test]
fn invalid_placements_and_ambiguous_platform_names_are_rejected() {
    let mut service = service();
    let mut envs = environments();
    let duplicate = envs[0].platforms[0].clone();
    envs[1].platforms.push(duplicate);
    assert!(deployment::validate(&[service.clone()], &envs)
        .unwrap_err()
        .contains("platform"));
    envs = environments();
    service.deployments.push(service.deployments[0].clone());
    assert!(deployment::validate(&[service.clone()], &envs)
        .unwrap_err()
        .contains("duplicate"));
    service.deployments.pop();
    service.service_name = None;
    assert!(deployment::validate(&[service.clone()], &envs)
        .unwrap_err()
        .contains("service_name"));
    service.service_name = Some("navsvc".into());
    for name in ["", "acme/path", "acme?query", "{{tenant}}", "acme@host"] {
        service.deployments[0].tenant = name.into();
        assert!(
            deployment::validate(&[service.clone()], &envs).is_err(),
            "{name}"
        );
    }
}

#[test]
fn templates_accept_only_builtin_inputs_and_http_base_urls() {
    for template in [
        "https://{{unknown}}.test",
        "https://{{baseUrl}}",
        "https://{{serviceName}.test",
        "file:///tmp/test",
        "https://user:password@host.test",
        "https://host.test?q=1",
        "https://host.test/#fragment",
        "not a URL",
    ] {
        let mut envs = environments();
        envs[0].platforms[0].url_template = template.into();
        assert!(
            deployment::validate(&[service()], &envs).is_err(),
            "{template}"
        );
    }
}

#[test]
fn generated_inputs_override_all_variable_scopes_before_expansion_and_masking() {
    let mut service = service();
    let envs = environments();
    let variable = |name: &str, value: &str, secret| Variable {
        name: name.into(),
        value: Some(value.into()),
        secret,
    };
    service.variables = vec![
        variable("baseUrl", "https://wrong.test", true),
        variable("endpoint", "{{baseUrl}}/menu", false),
        variable("token", "private-token", true),
    ];
    service.items[0].request.as_mut().unwrap().variables = vec![
        variable("tenant", "wrong", false),
        variable("baseUrl", "https://wrong-request.test", false),
    ];
    let target = resolve(&service, Some(&envs[0]), Some(&service.deployments[1]))
        .unwrap()
        .unwrap();
    let actual = send::variables_for_target(&service, Some(&envs[0]), &[], &[0], Some(&target));
    let masked =
        curl::masked_variables_for_target(&service, Some(&envs[0]), &[], &[0], Some(&target));
    assert_eq!(actual["baseUrl"], target.base_url);
    assert_eq!(masked["baseUrl"], target.base_url);
    assert_eq!(actual["endpoint"], format!("{}/menu", target.base_url));
    assert_eq!(actual["tenant"], "acme");
    assert_eq!(masked["token"], "***");
    let request = service.requests()[0].request;
    let preview = curl::render(request, None, &masked);
    assert!(preview.text.contains(&format!("{}/menu", target.base_url)));
    assert!(!preview.text.contains("wrong"));
}

#[test]
fn ordinary_collections_keep_their_existing_variable_behavior() {
    let service: Collection = serde_norway::from_str(
        "name: Ordinary\nvariables:\n - name: baseUrl\n   value: https://ordinary.test",
    )
    .unwrap();
    assert!(!deployment::enabled(&service));
    assert_eq!(resolve(&service, None, None).unwrap(), None);
    assert_eq!(
        send::variables(&service, None, &[], &[])["baseUrl"],
        "https://ordinary.test"
    );
}

#[test]
fn storage_separates_placement_and_preserves_it_through_saves_and_renames() {
    let root = std::env::temp_dir().join("opencode").join(format!(
        "rq-deployments-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut service = service();
    service.base_path = Some("/{{tenant == 'acme' ? serviceName : 'shared'}}-{{slot}}".into());
    let mut envs = environments();
    for env in &mut envs {
        for platform in &mut env.platforms {
            platform.url_template.push_str("{{basePath}}");
        }
    }
    envs.sort_by(|a, b| a.name.cmp(&b.name));
    let rendered = store::render(&service, &envs);
    let placements = &rendered.files["collections/navigation-core/deployments.yml"];
    assert!(!placements.contains("environment"));
    assert!(!placements.contains("url_template"));
    assert!(!rendered.files["collections/navigation-core/collection.yml"].contains("deployments"));
    store::write(&root, &rendered).unwrap();
    let loaded = store::load(&root).unwrap();
    assert_eq!(loaded.collections[0], service);
    assert_eq!(loaded.environments, envs);
    store::save_environment(&root, "stage", Vec::new()).unwrap();
    store::save(
        &root,
        "navigation-core",
        &store::Edit::Collection {
            name: "Navigation".into(),
            description: None,
            auth: Default::default(),
            variables: Vec::new(),
            scripts: Default::default(),
        },
    )
    .unwrap();
    let loaded = store::load(&root).unwrap();
    assert_eq!(loaded.collections[0].service_name, service.service_name);
    assert_eq!(loaded.collections[0].base_path, service.base_path);
    assert_eq!(loaded.collections[0].deployments, service.deployments);
    assert_eq!(loaded.environments, envs);
    assert!(root
        .join("collections/navigation/deployments.yml")
        .is_file());
    assert!(!root
        .join("collections/navigation-core/deployments.yml")
        .exists());
    // Unknown files are still not owned or removed by a save.
    std::fs::write(root.join("collections/navigation/notes.txt"), "keep").unwrap();
    store::save(
        &root,
        "navigation",
        &store::Edit::Variables {
            path: vec![0],
            variables: vec![Variable {
                name: "limit".into(),
                value: Some("20".into()),
                secret: false,
            }],
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("collections/navigation/notes.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        store::load(&root).unwrap().collections[0].requests().len(),
        1
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn templates_and_placements_still_pass_through_the_secret_leak_guard() {
    let mut service = service();
    service.variables.push(Variable {
        name: "token".into(),
        value: Some("private-secret-token".into()),
        secret: true,
    });
    let mut envs = environments();
    envs[0].platforms[0].url_template = "https://private-secret-token.test".into();
    assert!(!store::render(&service, &envs).report.leaks.is_empty());
    envs = environments();
    service.deployments[0].slot = "private-secret-token".into();
    assert!(!store::render(&service, &envs).report.leaks.is_empty());
    service.deployments[0].slot = "stable".into();
    service.base_path = Some("/private-secret-token".into());
    assert!(!store::render(&service, &envs).report.leaks.is_empty());
}
