use collection::{deployment, model::RequestDef, store, Collection, Environment};

fn collection(service: &str) -> Collection {
    serde_norway::from_str(&format!(
        "name: {service}\nservice_name: {service}\ndeployments:\n - {{tenant: brand, platform: stage-a, slot: default}}\n - {{tenant: brand, platform: stage-a, slot: worker}}\n"
    )).unwrap()
}

fn environment() -> Environment {
    serde_norway::from_str(
        "name: stage\nmain_slot: default\nplatforms:\n - name: stage-a\n   url_template: 'https://grid-{{tenant}}{{slotSuffix}}.example.test/{{serviceName}}{{slotSuffix}}'\n"
    ).unwrap()
}

#[test]
fn main_slot_omits_every_suffix_without_changing_slot_or_tenant_identity() {
    let environment = environment();
    for service in ["catalog", "navigation"] {
        let collection = collection(service);
        deployment::validate(
            std::slice::from_ref(&collection),
            std::slice::from_ref(&environment),
        )
        .unwrap();
        for (index, suffix) in [(0, ""), (1, "-worker")] {
            let placement = &collection.deployments[index];
            let target = deployment::resolve(&collection, Some(&environment), Some(placement))
                .unwrap()
                .unwrap();
            assert_eq!(target.slot, placement.slot);
            assert_eq!(target.tenant, "brand");
            assert_eq!(target.slot_suffix, suffix);
            assert_eq!(
                target.base_url,
                format!("https://grid-brand{suffix}.example.test/{service}{suffix}")
            );
            let vars = collection::send::variables_for_target(
                &collection,
                Some(&environment),
                &[],
                &[],
                Some(&target),
            );
            assert_eq!(vars["slot"], placement.slot);
            assert_eq!(vars["slotSuffix"], suffix);
            let preview = collection::curl::render(
                &RequestDef {
                    method: "GET".into(),
                    url: "{{baseUrl}}/items".into(),
                    ..Default::default()
                },
                None,
                &vars,
            );
            assert!(preview.problem.is_none(), "{:?}", preview.problem);
            assert!(preview.text.contains(&format!("{}/items", target.base_url)));
        }
    }
}

#[test]
fn base_path_can_use_optional_suffix_and_conditions_still_see_the_named_slot() {
    let mut collection = collection("catalog");
    collection.base_path =
        Some("/{{slot == 'default' ? serviceName : 'shared'}}{{slotSuffix}}".into());
    let mut environment = environment();
    environment.platforms[0].url_template =
        "https://{{serviceName}}{{slotSuffix}}.example.test{{basePath}}".into();
    for (index, expected) in [
        (0, "https://catalog.example.test/catalog"),
        (1, "https://catalog-worker.example.test/shared-worker"),
    ] {
        let target = deployment::resolve(
            &collection,
            Some(&environment),
            Some(&collection.deployments[index]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(target.base_url, expected);
    }
}

#[test]
fn the_main_slot_name_is_configurable_and_unset_preserves_literal_slot_templates() {
    let collection = collection("catalog");
    let mut environment = environment();
    environment.main_slot = Some("worker".into());
    let target = deployment::resolve(
        &collection,
        Some(&environment),
        Some(&collection.deployments[1]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(target.slot, "worker");
    assert_eq!(target.slot_suffix, "");
    environment.main_slot = None;
    environment.platforms[0].url_template = "https://{{serviceName}}-{{slot}}.example.test".into();
    let target = deployment::resolve(
        &collection,
        Some(&environment),
        Some(&collection.deployments[0]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(target.base_url, "https://catalog-default.example.test");
    assert_eq!(target.slot_suffix, "-default");
}

#[test]
fn malformed_main_slot_names_are_rejected() {
    let collection = collection("catalog");
    for name in ["", "contains space", "path/segment", "{{secret}}"] {
        let mut environment = environment();
        environment.main_slot = Some(name.into());
        assert!(
            deployment::validate(
                std::slice::from_ref(&collection),
                std::slice::from_ref(&environment)
            )
            .is_err(),
            "{name}"
        );
        assert!(
            deployment::resolve(
                &collection,
                Some(&environment),
                Some(&collection.deployments[0])
            )
            .is_err(),
            "{name}"
        );
    }
}

#[test]
fn environment_saves_and_renames_preserve_main_slot_configuration() {
    let root = std::env::temp_dir().join("opencode").join(format!(
        "rq-main-slot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let collection = collection("catalog");
    store::write(&root, &store::render(&collection, &[environment()])).unwrap();
    store::save_environment(&root, "stage", Vec::new()).unwrap();
    store::update_environment(&root, "stage", "renamed".into(), false, Vec::new()).unwrap();
    let loaded = store::load(&root).unwrap();
    assert_eq!(loaded.environments[0].main_slot.as_deref(), Some("default"));
    let target = deployment::resolve(
        &loaded.collections[0],
        Some(&loaded.environments[0]),
        Some(&loaded.collections[0].deployments[0]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(target.base_url, "https://grid-brand.example.test/catalog");
    assert!(
        std::fs::read_to_string(root.join("environments/renamed.yml"))
            .unwrap()
            .contains("main_slot: default")
    );
    std::fs::remove_dir_all(root).unwrap();
}
