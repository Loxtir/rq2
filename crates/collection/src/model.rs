//! The on-disk model for a workspace.
//!
//! One invariant holds everywhere: a [`Variable`] marked `secret` has its value
//! in memory so the importer can write it to `.secrets/`, but nothing that
//! serialises a collection may look at that value. See
//! [`crate::store::render`], which is the only place that writes files.
//!
//! Auth is a *setting*, not a resolved value. Postman, Bruno and every other
//! client let a request say "same as the folder". Flattening that to an explicit
//! auth on each request would mean changing a token scheme is a diff in every
//! request file — the exact churn one-file-per-request is meant to avoid.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

fn is_false(flag: &bool) -> bool {
    !*flag
}

/// A short random id, not in `taken`.
pub fn fresh_id(taken: &std::collections::BTreeSet<String>) -> String {
    use std::hash::{BuildHasher, Hasher};
    loop {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        let id = format!("{:08x}", hasher.finish() as u32);
        if !taken.contains(&id) {
            return id;
        }
    }
}

/// Clears the ids of an item and everything in it, so a copy gets new ones.
pub fn clear_ids(item: &mut Item) {
    item.id.clear();
    for child in &mut item.items {
        clear_ids(child);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    /// Stable identity for per-user state (current values, history); never
    /// changes with renames or reordering. Empty until first assigned.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deployments: Vec<Deployment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The default for every request that does not say otherwise.
    #[serde(default, skip_serializing_if = "AuthSetting::is_inherit")]
    pub auth: AuthSetting,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Scripts::is_empty")]
    pub scripts: Scripts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
}

/// A folder and a request share a shape: a folder has children, a request has
/// a `request` body. Keeping one type avoids an internally tagged enum, which
/// fights `serde(flatten)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Stable identity within the collection; see [`Collection::id`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    /// Only meaningful for folders: the auth requests underneath inherit.
    /// A request's own auth lives in its [`RequestDef`].
    #[serde(default, skip_serializing_if = "AuthSetting::is_inherit")]
    pub auth: AuthSetting,
    /// Only meaningful for folders: variables requests underneath can read. A
    /// request's own variables live in its [`RequestDef`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Scripts::is_empty")]
    pub scripts: Scripts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestDef {
    pub method: String,
    /// The URL *without* a query string. Query parameters live in [`Self::params`]
    /// so that switching one off is an edit to a value, not a rewrite of text.
    pub url: String,
    /// Every query parameter, enabled or not, in order.
    ///
    /// Old files and hand-written files may still carry `?a=1` inside `url`;
    /// [`Self::normalize_query`] moves it here on load, and the window does the
    /// same when a URL is typed. Both forms send identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<KeyValue>,
    /// Values for `:name` segments in the path. A segment without a value is an
    /// error when sending rather than a literal colon on the wire.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub path_params: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    /// `Inherit` unless this request overrides the folder or the collection.
    #[serde(default, skip_serializing_if = "AuthSetting::is_inherit")]
    pub auth: AuthSetting,
    /// Variables this request alone reads. The narrowest layer, so they win over
    /// a folder, the collection, the environment, and `globals.yml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    #[serde(default, skip_serializing_if = "Scripts::is_empty")]
    pub scripts: Scripts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Named response projections belonging only to this request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub queries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Scripts {
    /// Shared scripts from `scripts/*.js`, by their `// @id`, run in this
    /// order before this layer's own script of the same phase and with this
    /// layer's scope.
    #[serde(default, skip_serializing_if = "Includes::is_empty")]
    pub include: Includes,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pre_request: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub post_response: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Includes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_request: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_response: Vec<String>,
}

impl Includes {
    pub fn is_empty(&self) -> bool {
        self.pre_request.is_empty() && self.post_response.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.pre_request
            .iter()
            .chain(&self.post_response)
            .any(|include| include == id)
    }
}

/// `scripts/<name>.js`: one piece of code with a stable `// @id` on its first
/// line. Layers include it by id, so renaming the file changes nothing else.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedScript {
    pub id: String,
    /// The file stem, for people; never used as a reference.
    pub name: String,
    pub code: String,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.pre_request.is_empty() && self.post_response.is_empty()
    }
}

impl RequestDef {
    /// Names of `:segments` in the path, in order.
    ///
    /// Only a *whole* segment counts, so `https:` and gRPC transcoding's
    /// `v1:batchGet` are not mistaken for parameters. This matters: a plain
    /// "every colon starts a parameter" rule would mangle both.
    pub fn path_tokens(&self) -> Vec<String> {
        self.url
            .split('/')
            .filter_map(|segment| segment.strip_prefix(':'))
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Moves an inline `?a=1&b=2` from `url` into `params`, keeping any
    /// parameters already listed after it. Idempotent.
    pub fn normalize_query(&mut self) {
        let Some(at) = self.url.find('?') else {
            return;
        };
        let query = self.url[at + 1..].to_string();
        self.url.truncate(at);

        let mut parsed: Vec<KeyValue> = query
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (name, value) = part.split_once('=').unwrap_or((part, ""));
                KeyValue {
                    name: name.to_string(),
                    value: value.to_string(),
                    disabled: false,
                }
            })
            .collect();

        parsed.append(&mut self.params);
        self.params = parsed;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValue {
    pub name: String,
    pub value: String,
    /// Kept rather than dropped: "off right now" is information.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Body {
    /// `raw`, `urlencoded`, `formdata`, `graphql`, `file`.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Auth {
    /// `bearer`, `basic`, `apikey`, ... but never `inherit` or `none`; those are
    /// states of [`AuthSetting`], not methods.
    pub kind: String,
    #[serde(default, flatten)]
    pub fields: BTreeMap<String, String>,
}

/// Three states, because "no auth" and "whatever the folder uses" are different
/// intentions: a health check inside an authenticated folder must be able to
/// *stop* inheriting, and that has to survive a round trip.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AuthSetting {
    #[default]
    Inherit,
    /// Explicitly unauthenticated. Written as `auth: none`.
    None,
    Method(Auth),
}

impl AuthSetting {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }

    /// The auth method, if this setting names one.
    pub fn method(&self) -> Option<&Auth> {
        match self {
            Self::Method(auth) => Some(auth),
            _ => None,
        }
    }

    /// Resolves this setting against what the folder chain already provides.
    pub fn resolve<'a>(&'a self, inherited: Option<&'a Auth>) -> Option<&'a Auth> {
        match self {
            Self::Inherit => inherited,
            Self::None => None,
            Self::Method(auth) => Some(auth),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AuthRepr {
    Word(String),
    Method(Auth),
}

impl Serialize for AuthSetting {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Never reached at a field level (it is skipped), but a bare
            // `AuthSetting` still has to serialise somewhere.
            Self::Inherit => serializer.serialize_str("inherit"),
            Self::None => serializer.serialize_str("none"),
            Self::Method(auth) => auth.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AuthSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match AuthRepr::deserialize(deserializer)? {
            AuthRepr::Word(word) => match word.as_str() {
                "inherit" => Ok(Self::Inherit),
                "none" | "noauth" => Ok(Self::None),
                other => Err(D::Error::custom(format!(
                    "unknown auth `{other}`: expected `none`, `inherit`, or a mapping with `kind`"
                ))),
            },
            AuthRepr::Method(auth) => Ok(Self::Method(auth)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    /// Never written for secrets. In memory it holds the real value so the
    /// importer can move it to `.secrets/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    /// Named primary slot, omitted by the optional URL suffix for every service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_slot: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<Platform>,
    /// Skip certificate verification for this environment.
    ///
    /// Postman exposes this as a global "SSL certificate verification: off";
    /// curl as `--insecure`. Scoped to the environment on purpose: a flag set
    /// while pointed at an internal dev cluster must not follow you to prod.
    /// Absent means verify.
    #[serde(default, skip_serializing_if = "is_false")]
    pub insecure: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub name: String,
    pub url_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub tenant: String,
    pub platform: String,
    pub slot: String,
}

/// A request plus the folder trail that leads to it, with its auth already
/// resolved through the chain.
pub struct ResolvedRequest<'a> {
    /// The request's own name, as shown in the tree.
    pub name: &'a str,
    /// Folder names from the collection root down.
    pub trail: Vec<&'a str>,
    /// Index within each level, for anything that addresses requests by path.
    pub path: Vec<usize>,
    pub request: &'a RequestDef,
    /// `None` means the request is unauthenticated, whether because nothing
    /// above it sets auth or because it says `auth: none`.
    pub auth: Option<&'a Auth>,
}

impl Collection {
    /// The tree position of the folder or request with this id.
    pub fn path_of(&self, id: &str) -> Option<Vec<usize>> {
        fn walk(items: &[Item], id: &str, trail: &mut Vec<usize>) -> bool {
            for (index, item) in items.iter().enumerate() {
                trail.push(index);
                if item.id == id || walk(&item.items, id, trail) {
                    return true;
                }
                trail.pop();
            }
            false
        }
        let mut trail = Vec::new();
        (!id.is_empty() && walk(&self.items, id, &mut trail)).then_some(trail)
    }

    /// The id of the folder or request at `path`, if it has one.
    pub fn id_at(&self, path: &[usize]) -> Option<&str> {
        let (last, parents) = path.split_last()?;
        let mut items = &self.items;
        for index in parents {
            items = &items.get(*index)?.items;
        }
        Some(items.get(*last)?.id.as_str()).filter(|id| !id.is_empty())
    }

    /// Gives the collection and every folder and request without an id (or
    /// with one already used earlier in the tree) a fresh one. Returns whether
    /// anything changed.
    pub fn assign_ids(&mut self) -> bool {
        fn walk(items: &mut [Item], seen: &mut std::collections::BTreeSet<String>) -> bool {
            let mut changed = false;
            for item in items {
                if item.id.is_empty() || !seen.insert(item.id.clone()) {
                    item.id = fresh_id(seen);
                    seen.insert(item.id.clone());
                    changed = true;
                }
                changed |= walk(&mut item.items, seen);
            }
            changed
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut changed = false;
        if self.id.is_empty() {
            self.id = fresh_id(&seen);
            changed = true;
        }
        seen.insert(self.id.clone());
        walk(&mut self.items, &mut seen) || changed
    }

    /// Depth-first walk over every request, with the folder trail and the
    /// effective auth. Used for reporting and, later, for the request list.
    pub fn requests(&self) -> Vec<ResolvedRequest<'_>> {
        let mut out = Vec::new();
        walk(
            &self.items,
            &mut Vec::new(),
            &mut Vec::new(),
            self.auth.resolve(None),
            &mut out,
        );
        out
    }

    /// The request at a [`ResolvedRequest::path`], with its auth resolved.
    pub fn request_at(&self, path: &[usize]) -> Option<ResolvedRequest<'_>> {
        self.requests().into_iter().find(|found| found.path == path)
    }

    /// A request by name, case-insensitively: an exact match first, then a
    /// unique substring. The error lists the candidates.
    pub fn find_request(&self, name: &str) -> Result<ResolvedRequest<'_>, String> {
        let wanted = name.to_ascii_lowercase();
        let mut requests = self.requests();
        let exact = |candidate: &ResolvedRequest| candidate.name.to_ascii_lowercase() == wanted;
        let partial =
            |candidate: &ResolvedRequest| candidate.name.to_ascii_lowercase().contains(&wanted);
        let matches: Vec<usize> = if requests.iter().any(exact) {
            (0..requests.len())
                .filter(|&i| exact(&requests[i]))
                .collect()
        } else {
            (0..requests.len())
                .filter(|&i| partial(&requests[i]))
                .collect()
        };
        match matches.as_slice() {
            [only] => Ok(requests.swap_remove(*only)),
            [] => Err(format!(
                "no request called `{name}`. Available: {}",
                requests
                    .iter()
                    .map(|r| r.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            many => Err(format!(
                "`{name}` matches {} requests: {}",
                many.len(),
                many.iter()
                    .map(|&i| requests[i].name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The auth a folder or request at `path` would get from everything above
    /// it, excluding its own setting. Used to show "inherits bearer" in the
    /// window without copying the value down.
    pub fn inherited_auth_at(&self, path: &[usize]) -> Option<&Auth> {
        let mut inherited = self.auth.resolve(None);
        let mut items: &[Item] = &self.items;

        for (depth, index) in path.iter().enumerate() {
            let item = items.get(*index)?;
            if depth + 1 == path.len() {
                return inherited;
            }
            inherited = item.auth.resolve(inherited);
            items = &item.items;
        }

        inherited
    }

    /// Moves inline `?a=1` query strings into `params` across the tree. Called
    /// on load so the window only ever sees the canonical form, whether or not
    /// the file on disk has been through an editor yet.
    pub fn normalize(&mut self) {
        fn walk(items: &mut [Item]) {
            for item in items {
                if let Some(request) = &mut item.request {
                    request.normalize_query();
                }
                walk(&mut item.items);
            }
        }
        walk(&mut self.items);
    }

    pub fn folder_count(&self) -> usize {
        fn count(items: &[Item]) -> usize {
            items
                .iter()
                .map(|item| usize::from(item.request.is_none()) + count(&item.items))
                .sum()
        }
        count(&self.items)
    }

    /// Variable layers from the collection down to the request at `path`,
    /// widest first.
    ///
    /// The send order is `globals -> environment -> collection -> folders ->
    /// request`, so a later layer here overrides an earlier one. Empty scopes
    /// still appear, which keeps the caller from special-casing a folder that
    /// declares nothing.
    pub fn variable_layers(&self, path: &[usize]) -> Vec<&[Variable]> {
        let mut layers = vec![self.variables.as_slice()];
        let mut items: &[Item] = &self.items;

        for index in path {
            let Some(item) = items.get(*index) else { break };
            if let Some(request) = &item.request {
                layers.push(request.variables.as_slice());
                break;
            }
            layers.push(item.variables.as_slice());
            items = &item.items;
        }

        layers
    }

    /// Every variable in the collection, at any depth.
    ///
    /// Secrets are stored in one file per collection directory, so saving has to
    /// find them wherever they were declared, not only on the collection.
    pub fn all_variables(&self) -> Vec<&Variable> {
        fn walk<'a>(items: &'a [Item], out: &mut Vec<&'a Variable>) {
            for item in items {
                out.extend(item.variables.iter());
                if let Some(request) = &item.request {
                    out.extend(request.variables.iter());
                }
                walk(&item.items, out);
            }
        }

        let mut out: Vec<&Variable> = self.variables.iter().collect();
        walk(&self.items, &mut out);
        out
    }

    /// The mutable counterpart of [`Self::all_variables`], used when reading
    /// secret values back from `.secrets/`.
    pub fn all_variables_mut(&mut self) -> Vec<&mut Variable> {
        fn walk<'a>(items: &'a mut [Item], out: &mut Vec<&'a mut Variable>) {
            for item in items {
                out.extend(item.variables.iter_mut());
                if let Some(request) = &mut item.request {
                    out.extend(request.variables.iter_mut());
                }
                walk(&mut item.items, out);
            }
        }

        let mut out: Vec<&mut Variable> = self.variables.iter_mut().collect();
        walk(&mut self.items, &mut out);
        out
    }

    /// Whether the collection, a folder or a request includes the shared
    /// script with this id, in either phase.
    pub fn includes(&self, id: &str) -> bool {
        fn walk(items: &[Item], id: &str) -> bool {
            items.iter().any(|item| {
                item.scripts.include.contains(id)
                    || item
                        .request
                        .as_ref()
                        .is_some_and(|request| request.scripts.include.contains(id))
                    || walk(&item.items, id)
            })
        }
        self.scripts.include.contains(id) || walk(&self.items, id)
    }

    /// The collection's scripts, then every folder's and request's.
    pub fn all_scripts_mut(&mut self) -> Vec<&mut Scripts> {
        fn walk<'a>(items: &'a mut [Item], out: &mut Vec<&'a mut Scripts>) {
            for item in items {
                out.push(&mut item.scripts);
                if let Some(request) = &mut item.request {
                    out.push(&mut request.scripts);
                }
                walk(&mut item.items, out);
            }
        }

        let mut out = vec![&mut self.scripts];
        walk(&mut self.items, &mut out);
        out
    }

    pub fn secrets(&self) -> Vec<&Variable> {
        self.all_variables()
            .into_iter()
            .filter(|variable| variable.secret)
            .collect()
    }
}

fn walk<'a>(
    items: &'a [Item],
    trail: &mut Vec<&'a str>,
    path: &mut Vec<usize>,
    inherited: Option<&'a Auth>,
    out: &mut Vec<ResolvedRequest<'a>>,
) {
    for (index, item) in items.iter().enumerate() {
        path.push(index);

        // A folder's setting applies to everything below it; a request's own
        // setting is resolved where it is emitted.
        let from_folder = item.auth.resolve(inherited);

        if let Some(request) = &item.request {
            out.push(ResolvedRequest {
                name: &item.name,
                trail: trail.clone(),
                path: path.clone(),
                request,
                auth: request.auth.resolve(from_folder),
            });
        }

        if !item.items.is_empty() {
            trail.push(&item.name);
            walk(&item.items, trail, path, from_folder, out);
            trail.pop();
        }

        path.pop();
    }
}

impl Environment {
    pub fn secrets(&self) -> Vec<&Variable> {
        self.variables.iter().filter(|v| v.secret).collect()
    }
}
