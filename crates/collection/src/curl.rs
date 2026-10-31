//! Rendering a stored request as a `curl` command.
//!
//! Masking happens *before* substitution and before variable expansion: the
//! renderer is handed a variable map whose secret values are already `***`. So a
//! token cannot appear in the preview even if it was used somewhere unexpected —
//! a query parameter, a body field, a header nobody remembered, or a collection
//! variable that composes it. Masking after the fact would miss those.

use crate::model::{Auth, Collection, Environment, RequestDef, Variable};
use crate::send::{self, auth_parts, Variables};
use std::collections::BTreeMap;

pub const MASK: &str = "***";

pub struct Preview {
    pub text: String,
    /// True when at least one secret was referenced and therefore masked. The
    /// window says so, because a masked command is not a runnable one.
    pub masked: bool,
    /// A reason the command is not runnable, such as an unfilled `:path`
    /// parameter. The window shows it instead of letting a wrong URL be pasted.
    pub problem: Option<String>,
}

/// The same precedence as sending, with every secret value replaced before the
/// renderer sees it.
pub fn masked_variables(
    collection: &Collection,
    environment: Option<&Environment>,
    globals: &[Variable],
    path: &[usize],
) -> Variables {
    masked_variables_for_target(collection, environment, globals, path, None)
}

pub fn masked_variables_for_target(
    collection: &Collection,
    environment: Option<&Environment>,
    globals: &[Variable],
    path: &[usize],
    target: Option<&crate::deployment::Target>,
) -> Variables {
    let mut variables = send::raw_variables(collection, environment, globals, path);

    let none: &[Variable] = &[];
    let environment_variables = environment.map_or(none, |environment| &environment.variables);
    let layers = collection
        .variable_layers(path)
        .into_iter()
        .chain(std::iter::once(environment_variables))
        .chain(std::iter::once(globals));

    for variable in layers.flat_map(|layer| layer.iter()) {
        if variable.secret && variable.value.is_some() {
            variables.insert(variable.name.clone(), MASK.to_string());
        }
    }

    if let Some(target) = target {
        target.insert_into(&mut variables);
    }
    send::expand(variables)
}

pub fn render(
    request: &RequestDef,
    auth: Option<&Auth>,
    variables: &BTreeMap<String, String>,
) -> Preview {
    let mut missing = Vec::new();

    let (mut url, problem) = send::resolve_url(request, variables, &mut missing);
    let parts = auth
        .map(|auth| auth_parts(auth, variables, &mut missing))
        .transpose()
        .ok()
        .flatten()
        .unwrap_or_default();

    send::append_auth_query(&mut url, &parts.query);

    let mut args: Vec<String> = Vec::new();
    let method = request.method.trim().to_uppercase();
    if method != "GET" {
        args.push(format!("-X {method}"));
    }
    args.push(single_quote(&url));

    let mut has_content_type = false;
    for header in &request.headers {
        if header.disabled {
            continue;
        }
        let name = send::substitute(&header.name, variables, &mut missing);
        let value = send::substitute(&header.value, variables, &mut missing);
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        args.push(format!("-H {}", single_quote(&format!("{name}: {value}"))));
    }

    for (name, value) in &parts.headers {
        args.push(format!("-H {}", single_quote(&format!("{name}: {value}"))));
    }

    let mut problem = problem;
    if let Some(body) = &request.body {
        if body.text.is_some() {
            match send::body_payload(body, variables, &mut missing) {
                Ok((text, json)) => {
                    if json && !has_content_type {
                        args.push(format!(
                            "-H {}",
                            single_quote("Content-Type: application/json")
                        ));
                    }
                    args.push(format!("--data-raw {}", single_quote(&text)));
                }
                Err(error) => {
                    problem.get_or_insert(error);
                }
            }
        }
    }

    let text = format!("curl {}", args.join(" "));

    Preview {
        // Exact rather than bookkept: if the rendered command contains the mask,
        // it contains the mask.
        masked: text.contains(MASK),
        text,
        problem,
    }
}

/// Single quotes survive every shell except `cmd.exe`; an embedded quote is the
/// only thing that needs escaping, and bash spells it `'\''`.
fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
