//! Service placement and platform-owned URL patterns. These are not variable scopes.

use crate::model::{Collection, Deployment, Environment};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod expression;

pub const RESERVED: [&str; 6] = [
    "serviceName",
    "tenant",
    "platform",
    "slot",
    "basePath",
    "baseUrl",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub service_name: String,
    pub environment: String,
    pub tenant: String,
    pub platform: String,
    pub slot: String,
    #[serde(default)]
    pub base_path: String,
    pub base_url: String,
}

impl Target {
    pub fn insert_into(&self, variables: &mut BTreeMap<String, String>) {
        for (name, value) in [
            ("serviceName", &self.service_name),
            ("tenant", &self.tenant),
            ("platform", &self.platform),
            ("slot", &self.slot),
            ("basePath", &self.base_path),
            ("baseUrl", &self.base_url),
        ] {
            variables.insert(name.to_string(), value.clone());
        }
    }
}

pub fn enabled(collection: &Collection) -> bool {
    collection.service_name.is_some()
        || collection.base_path.is_some()
        || !collection.deployments.is_empty()
}

// Names are substituted into host/path components, not interpreted as URL syntax.
fn validate_name(kind: &str, name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "{kind} `{name}` must contain only letters, digits, '-', '_' or '.'"
        ));
    }
    Ok(())
}

pub fn validate(collections: &[Collection], environments: &[Environment]) -> Result<(), String> {
    let mut platforms = BTreeSet::new();
    for environment in environments {
        for platform in &environment.platforms {
            validate_name("platform", &platform.name)?;
            if !platforms.insert(&platform.name) {
                return Err(format!(
                    "platform `{}` belongs to more than one environment or is duplicated",
                    platform.name
                ));
            }
            // Validate the template even before a deployment is selected.
            render_template(
                &platform.url_template,
                "service",
                "tenant",
                &platform.name,
                "slot",
                "/base",
            )?;
        }
    }
    for collection in collections {
        if !enabled(collection) {
            continue;
        }
        let service = collection.service_name.as_deref().ok_or_else(|| {
            format!(
                "collection `{}` needs service_name for its deployments",
                collection.name
            )
        })?;
        validate_name("service_name", service)?;
        if let Some(template) = &collection.base_path {
            render_base_path(template, service, "tenant", "platform", "slot")?;
        }
        let mut placements = BTreeSet::new();
        for deployment in &collection.deployments {
            validate_name("tenant", &deployment.tenant)?;
            validate_name("platform", &deployment.platform)?;
            validate_name("slot", &deployment.slot)?;
            if let Some(template) = &collection.base_path {
                render_base_path(
                    template,
                    service,
                    &deployment.tenant,
                    &deployment.platform,
                    &deployment.slot,
                )?;
            }
            if !placements.insert((&deployment.tenant, &deployment.platform, &deployment.slot)) {
                return Err(format!(
                    "duplicate deployment in `{}`: {} / {} / {}",
                    collection.name, deployment.tenant, deployment.platform, deployment.slot
                ));
            }
        }
    }
    Ok(())
}

fn render_template(
    template: &str,
    service: &str,
    tenant: &str,
    platform: &str,
    slot: &str,
    base_path: &str,
) -> Result<String, String> {
    let inputs = BTreeMap::from([
        ("serviceName".into(), service.into()),
        ("tenant".into(), tenant.into()),
        ("platform".into(), platform.into()),
        ("slot".into(), slot.into()),
        ("basePath".into(), base_path.into()),
    ]);
    let mut missing = Vec::new();
    let url = crate::send::substitute(template, &inputs, &mut missing);
    if !missing.is_empty() || url.contains(['{', '}']) {
        return Err(format!("invalid URL template for platform `{platform}`: use only serviceName, tenant, platform, slot and basePath"));
    }
    let parsed = reqwest::Url::parse(&url)
        .map_err(|e| format!("invalid URL template for platform `{platform}`: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(format!("platform `{platform}` needs an HTTP(S) base URL without credentials, query or fragment"));
    }
    Ok(url.trim_end_matches('/').to_string())
}

fn render_base_path(
    template: &str,
    service: &str,
    tenant: &str,
    platform: &str,
    slot: &str,
) -> Result<String, String> {
    let inputs = BTreeMap::from([
        ("serviceName".into(), service.into()),
        ("tenant".into(), tenant.into()),
        ("platform".into(), platform.into()),
        ("slot".into(), slot.into()),
    ]);
    let path = expression::render(template, &inputs)?;
    if (!path.is_empty() && !path.starts_with('/'))
        || path.starts_with("//")
        || path.contains(['?', '#', '\\', '{', '}'])
        || path.chars().any(char::is_whitespace)
        || path.chars().any(char::is_control)
    {
        return Err("base_path must be empty or an absolute URL path starting with one '/', without a query, fragment, backslash, braces or whitespace".into());
    }
    Ok(path.trim_end_matches('/').to_string())
}

pub fn resolve(
    collection: &Collection,
    environment: Option<&Environment>,
    selection: Option<&Deployment>,
) -> Result<Option<Target>, String> {
    if !enabled(collection) {
        if selection.is_some() {
            return Err("this collection does not define deployments".into());
        }
        return Ok(None);
    }
    let environment = environment.ok_or("Select an environment for this service's deployment")?;
    let selection = selection.ok_or("Select a tenant and deployment before sending")?;
    if !collection.deployments.contains(selection) {
        return Err("that deployment is not configured for this service".into());
    }
    let platform = environment
        .platforms
        .iter()
        .find(|platform| platform.name == selection.platform)
        .ok_or("that deployment's platform does not belong to the selected environment")?;
    let service = collection
        .service_name
        .as_deref()
        .ok_or("this service needs service_name")?;
    validate_name("service_name", service)?;
    validate_name("tenant", &selection.tenant)?;
    validate_name("platform", &selection.platform)?;
    validate_name("slot", &selection.slot)?;
    let base_path = match &collection.base_path {
        Some(template) => render_base_path(
            template,
            service,
            &selection.tenant,
            &selection.platform,
            &selection.slot,
        )?,
        None => String::new(),
    };
    if collection.base_path.is_some()
        && !platform.url_template.split("{{").skip(1).any(|token| {
            token
                .split_once("}}")
                .is_some_and(|(name, _)| name.trim() == "basePath")
        })
    {
        return Err(format!("platform `{}` must include {{{{basePath}}}} in its URL template to use this collection's base_path", platform.name));
    }
    Ok(Some(Target {
        service_name: service.to_string(),
        environment: environment.name.clone(),
        tenant: selection.tenant.clone(),
        platform: selection.platform.clone(),
        slot: selection.slot.clone(),
        base_url: render_template(
            &platform.url_template,
            service,
            &selection.tenant,
            &selection.platform,
            &selection.slot,
            &base_path,
        )?,
        base_path,
    }))
}
