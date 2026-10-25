use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use response_query::openapi::Spec;
use response_query::{
    fields_at, infer, parse, project, scaffold, to_json_schema, validate, Schema, SchemaKind,
    Selection,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "rq", version, about = "Query and project JSON API responses")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse a query and report diagnostics.
    Check {
        #[arg(short, long)]
        query: PathBuf,
        /// OpenAPI document (JSON). Point at a spec to validate field names.
        #[arg(long)]
        spec: Option<PathBuf>,
        /// operationId inside the spec to validate against.
        #[arg(long)]
        operation: Option<String>,
    },
    /// Project a response through a query.
    Project {
        #[arg(short, long)]
        query: PathBuf,
        /// Response file. Use `-` to read stdin.
        response: PathBuf,
        #[arg(long)]
        pretty: bool,
    },
    /// Infer a schema from one or more observed responses.
    Infer {
        responses: Vec<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        pretty: bool,
    },
    /// List the fields available at a path, for autocomplete and the picker.
    Fields {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        operation: String,
        /// Dot-separated path into the response, e.g. `data.items`.
        #[arg(long, value_delimiter = '.')]
        path: Vec<String>,
    },
    /// Generate a starter query from an operation's response schema.
    Scaffold {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        operation: String,
        /// How many object levels to expand before emitting subtrees verbatim.
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
    /// List operations in an OpenAPI spec.
    Ops {
        #[arg(long)]
        spec: PathBuf,
    },
    /// Import a saved collection into a git-friendly workspace.
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },
    /// Send a stored request and print the response body.
    Send {
        /// Workspace directory (the one containing `collections/`).
        workspace: PathBuf,
        /// Request name, as shown in the tree.
        #[arg(long)]
        request: String,
        /// Collection name; only needed when the workspace holds several.
        #[arg(long)]
        collection: Option<String>,
        /// Environment name, e.g. `dev`.
        #[arg(long)]
        environment: Option<String>,
        /// Write the body here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Seconds to wait for the response.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Skip certificate verification, even if the environment does not.
        #[arg(long)]
        insecure: bool,
    },
    /// Generate a large synthetic response for benchmarking.
    Gen {
        #[arg(long, default_value_t = 500_000)]
        lines: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
}

#[derive(Subcommand)]
enum ImportSource {
    /// Postman collection v2.1, plus optional environment exports.
    Postman {
        /// The collection export.
        collection: PathBuf,
        /// Directory to write the workspace into.
        #[arg(long)]
        out: PathBuf,
        /// Environment export(s) to import alongside the collection.
        #[arg(long = "environment")]
        environments: Vec<PathBuf>,
        /// Treat this variable name as secret even if Postman does not mark it.
        #[arg(long = "secret")]
        secret: Vec<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Check {
            query,
            spec,
            operation,
        } => cmd_check(query, spec, operation),

        Cmd::Project {
            query,
            response,
            pretty,
        } => cmd_project(query, response, pretty),

        Cmd::Infer {
            responses,
            out,
            pretty,
        } => cmd_infer(responses, out, pretty),

        Cmd::Ops { spec } => cmd_ops(spec),

        Cmd::Fields {
            spec,
            operation,
            path,
        } => cmd_fields(spec, operation, path),

        Cmd::Scaffold {
            spec,
            operation,
            depth,
        } => cmd_scaffold(spec, operation, depth),

        Cmd::Gen { lines, seed } => cmd_gen(lines, seed),

        Cmd::Send {
            workspace,
            request,
            collection,
            environment,
            out,
            timeout,
            insecure,
        } => cmd_send(
            workspace,
            request,
            collection,
            environment,
            out,
            timeout,
            insecure,
        ),

        Cmd::Import { source } => match source {
            ImportSource::Postman {
                collection,
                out,
                environments,
                secret,
            } => cmd_import_postman(collection, out, environments, secret),
        },
    }
}

fn load_query(path: &PathBuf) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn count_fields(selection: &Selection) -> usize {
    selection
        .fields
        .iter()
        .map(|field| 1 + field.selection.as_ref().map(count_fields).unwrap_or(0))
        .sum()
}

fn cmd_check(query: PathBuf, spec: Option<PathBuf>, operation: Option<String>) -> Result<ExitCode> {
    let source = load_query(&query)?;

    let selection = match parse(&source) {
        Ok(selection) => selection,
        Err(diag) => {
            eprintln!("{}", diag.render(&source));
            return Ok(ExitCode::FAILURE);
        }
    };

    let mut schema: Option<Schema> = None;
    match (spec, operation) {
        (Some(path), Some(operation_id)) => {
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let spec = Spec::parse(&bytes)?;
            schema = spec.response_schema(&operation_id)?;
            if schema.is_none() {
                eprintln!("warning: operation `{operation_id}` not found in spec");
            }
        }
        (Some(_), None) => {
            eprintln!("note: pass --operation <operationId> to validate against the spec");
        }
        _ => {}
    }

    let total = count_fields(&selection);

    match &schema {
        Some(schema) => {
            let diags = validate(&selection, schema);
            if diags.is_empty() {
                let noun = if total == 1 { "field" } else { "fields" };
                println!("ok: {total} {noun}, all resolve against the schema");
                Ok(ExitCode::SUCCESS)
            } else {
                for diag in &diags {
                    eprintln!("{}", diag.render(&source));
                }
                Ok(ExitCode::FAILURE)
            }
        }
        None => {
            let noun = if total == 1 { "field" } else { "fields" };
            println!("ok: parsed {total} {noun} (no schema to validate against)");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn cmd_project(query: PathBuf, response: PathBuf, pretty: bool) -> Result<ExitCode> {
    let source = load_query(&query)?;

    let selection = match parse(&source) {
        Ok(selection) => selection,
        Err(diag) => {
            eprintln!("{}", diag.render(&source));
            return Ok(ExitCode::FAILURE);
        }
    };

    let bytes = if response.as_os_str().to_str() == Some("-") {
        read_stdin()?
    } else {
        std::fs::read(&response).with_context(|| format!("reading {}", response.display()))?
    };

    match project(&bytes, &selection) {
        Ok(out) => {
            write_output(&out, pretty)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(diag) => {
            eprintln!("{diag}");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn cmd_infer(responses: Vec<PathBuf>, out: Option<PathBuf>, pretty: bool) -> Result<ExitCode> {
    if responses.is_empty() {
        bail!("pass at least one response file");
    }

    let mut buffers = Vec::new();
    for path in &responses {
        buffers.push(std::fs::read(path).with_context(|| format!("reading {}", path.display()))?);
    }

    let samples: Vec<&[u8]> = buffers.iter().map(|buf| buf.as_slice()).collect();
    let schema = infer(&samples)?;
    let value = to_json_schema(&schema);
    let text = if pretty {
        serde_json::to_string_pretty(&value)?
    } else {
        serde_json::to_string(&value)?
    };

    match out {
        Some(path) => {
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_fields(spec_path: PathBuf, operation: String, path: Vec<String>) -> Result<ExitCode> {
    let bytes =
        std::fs::read(&spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec = Spec::parse(&bytes)?;
    let root = spec
        .response_schema(&operation)?
        .ok_or_else(|| anyhow!("operation `{operation}` not found in the spec"))?;

    let segments: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    let target = if segments.is_empty() {
        &root
    } else {
        fields_at(&root, &segments)
            .ok_or_else(|| anyhow!("path `{}` not found in the schema", path.join(".")))?
    };

    let mut node = target;
    while let SchemaKind::Array(items) = &node.kind {
        node = items.as_ref();
    }

    match &node.kind {
        SchemaKind::Object(fields) => {
            for (name, field) in fields {
                println!(
                    "{:<28} {:<12} {:<9} {}",
                    name,
                    describe(&field.schema),
                    if field.required { "required" } else { "" },
                    field.desc.as_deref().unwrap_or("")
                );
            }
            if node.open {
                println!("(additional properties allowed)");
            }
        }
        SchemaKind::Scalar(kind) => println!("(scalar {})", kind.json_name()),
        SchemaKind::Any => println!("(untyped — no fields known here)"),
        SchemaKind::Array(_) => println!("(array)"),
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_scaffold(spec_path: PathBuf, operation: String, depth: usize) -> Result<ExitCode> {
    let bytes =
        std::fs::read(&spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec = Spec::parse(&bytes)?;
    let schema = spec
        .response_schema(&operation)?
        .ok_or_else(|| anyhow!("operation `{operation}` not found in the spec"))?;

    print!("{}", scaffold(&schema, depth));
    Ok(ExitCode::SUCCESS)
}

fn describe(schema: &Schema) -> String {
    match &schema.kind {
        SchemaKind::Object(_) => "object".to_string(),
        SchemaKind::Array(items) => format!("[{}]", describe(items)),
        SchemaKind::Scalar(kind) => kind.json_name().to_string(),
        SchemaKind::Any => "any".to_string(),
    }
}

fn cmd_send(
    workspace: PathBuf,
    request_name: String,
    collection_name: Option<String>,
    environment_name: Option<String>,
    out: Option<PathBuf>,
    timeout: u64,
    insecure_flag: bool,
) -> Result<ExitCode> {
    use std::io::Write as _;

    let loaded = collection::store::load(&workspace).map_err(anyhow::Error::msg)?;
    if loaded.collections.is_empty() {
        bail!("no collections found in {}", workspace.display());
    }

    let collection = match &collection_name {
        Some(name) => loaded
            .collections
            .iter()
            .find(|candidate| {
                collection::store::slugify(&candidate.name) == collection::store::slugify(name)
            })
            .ok_or_else(|| anyhow!("no collection called `{name}`"))?,
        None if loaded.collections.len() == 1 => &loaded.collections[0],
        None => bail!("several collections here; pass --collection"),
    };

    let environment = match &environment_name {
        Some(name) => Some(
            loaded
                .environments
                .iter()
                .find(|candidate| {
                    collection::store::slugify(&candidate.name) == collection::store::slugify(name)
                })
                .ok_or_else(|| anyhow!("no environment called `{name}`"))?,
        ),
        None => None,
    };

    let requests = collection.requests();
    let wanted = request_name.to_ascii_lowercase();
    let exact: Vec<_> = requests
        .iter()
        .filter(|candidate| candidate.name.to_ascii_lowercase() == wanted)
        .collect();
    let matches: Vec<_> = if exact.is_empty() {
        requests
            .iter()
            .filter(|candidate| candidate.name.to_ascii_lowercase().contains(&wanted))
            .collect()
    } else {
        exact
    };

    let found = match matches.as_slice() {
        [only] => *only,
        [] => {
            let names: Vec<_> = requests.iter().map(|r| r.name).collect();
            bail!(
                "no request called `{request_name}`. Available: {}",
                names.join(", ")
            )
        }
        many => bail!(
            "`{request_name}` matches {} requests: {}",
            many.len(),
            many.iter().map(|r| r.name).collect::<Vec<_>>().join(", ")
        ),
    };

    let vars = collection::send::variables(collection, environment, &loaded.globals, &found.path);
    let insecure = insecure_flag || environment.is_some_and(|environment| environment.insecure);
    let sent = collection::send::send(
        found.request,
        found.auth,
        &vars,
        &collection::send::SendOptions {
            insecure,
            timeout: std::time::Duration::from_secs(timeout),
        },
    )
    .map_err(anyhow::Error::msg)?;

    if insecure {
        eprintln!("warning: certificate verification is off for this request");
    }

    for name in &sent.missing {
        eprintln!("warning: `{{{{{name}}}}}` is not defined in any scope; left as written");
    }
    eprintln!(
        "{} {} -> HTTP {} in {} ms, {} bytes",
        found.request.method,
        found.request.url,
        sent.status,
        sent.elapsed.as_millis(),
        sent.body.len()
    );

    match out {
        Some(path) => std::fs::write(&path, &sent.body)
            .with_context(|| format!("writing {}", path.display()))?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(&sent.body)?;
            stdout.flush()?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_import_postman(
    collection_path: PathBuf,
    out: PathBuf,
    environments: Vec<PathBuf>,
    secret: Vec<String>,
) -> Result<ExitCode> {
    let bytes = std::fs::read(&collection_path)
        .with_context(|| format!("reading {}", collection_path.display()))?;
    let parsed = collection::postman::parse_collection(&bytes, &secret)
        .map_err(|e| anyhow!("{}: {e}", collection_path.display()))?;

    let mut warnings = parsed.warnings;
    let mut parsed_environments = Vec::new();

    for path in &environments {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let parsed_environment = collection::postman::parse_environment(&bytes, &secret)
            .map_err(|e| anyhow!("{}: {e}", path.display()))?;
        warnings.extend(parsed_environment.warnings);
        parsed_environments.push(parsed_environment.environment);
    }

    let rendered = collection::store::render(&parsed.collection, &parsed_environments);

    let collection_dir = out.join(&rendered.report.collection_dir);
    if collection_dir.exists() {
        bail!(
            "{} already exists; remove it or choose another --out",
            collection_dir.display()
        );
    }

    collection::store::write(&out, &rendered)
        .with_context(|| format!("writing {}", out.display()))?;

    println!("imported `{}`", parsed.collection.name);
    println!(
        "  {} requests in {} folders",
        rendered.report.requests, rendered.report.folders
    );

    if rendered.report.secret_variables.is_empty() {
        println!("  no secret variables");
    } else {
        println!(
            "  {} secret variables moved to .secrets/ (gitignored): {}",
            rendered.report.secret_variables.len(),
            rendered.report.secret_variables.join(", ")
        );
    }

    for file in rendered.files.keys() {
        println!("  wrote {file}");
    }
    println!(
        "  ensured `{}` in .gitignore",
        collection::store::GITIGNORE_LINE
    );

    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_ops(spec: PathBuf) -> Result<ExitCode> {
    let bytes = std::fs::read(&spec).with_context(|| format!("reading {}", spec.display()))?;
    let spec = Spec::parse(&bytes)?;
    for op in spec.operations() {
        println!("{:<7} {:<40} {}", op.method, op.display_id(), op.path);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_gen(lines: usize, seed: u64) -> Result<ExitCode> {
    let text = generate(lines, seed);
    eprintln!("generated {} lines", text.lines().count());
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

fn read_stdin() -> Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

fn write_output(bytes: &[u8], pretty: bool) -> Result<()> {
    use std::io::Write;

    if pretty {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).context("projection produced invalid JSON")?;
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(bytes)?;
        lock.write_all(b"\n")?;
    }

    Ok(())
}

fn generate(target_lines: usize, seed: u64) -> String {
    use serde_json::json;

    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    // Roughly how many pretty-printed lines one item occupies.
    const LINES_PER_ITEM: usize = 24;
    let count = (target_lines / LINES_PER_ITEM).max(1);

    let mut items = Vec::with_capacity(count);
    for i in 0..count {
        let s = next();
        let status = ["pending", "paid", "shipped", "cancelled"][(s % 4) as usize];
        items.push(json!({
            "id": i,
            "orderId": format!("ord-{:08x}", (s >> 16) as u32),
            "total": ((s % 100_000) as f64) / 100.0,
            "status": status,
            "customer": {
                "id": (s % 5_000) as u32,
                "name": format!("Customer {:04}", s % 10_000),
                "email": format!("c{}@example.test", s % 10_000),
                "tier": "gold"
            },
            "tags": ["a", "b"],
            "meta": {
                "traceId": format!("{:016x}", s),
                "retries": 0,
                "flags": { "gift": false, "priority": true }
            }
        }));
    }

    let doc = json!({
        "data": {
            "items": items,
            "page": 1,
            "pageSize": count,
            "totalPages": 42
        },
        "meta": {
            "requestId": format!("{:016x}", seed),
            "elapsedMs": 37
        }
    });

    serde_json::to_string_pretty(&doc).unwrap_or_default()
}
