# rq — response queries

A desktop API client and CLI built around a projection language for JSON
responses, with git-friendly collections, scoped variables, and desktop
pre-request and post-response scripts.

A query is a **view over a stored response**, not a destructive filter. The raw
body is always kept; the query is re-run against it locally and for free.


```
{
  id!
  profile { displayName avatar }
  recentOrders[:5] { id: orderId total }
  metadata
}
```

| Syntax                      | Meaning                                             |
| --------------------------- | --------------------------------------------------- |
| `id`                      | leaf, copied verbatim                               |
| `profile { a b }`         | projected object                                    |
| `orders { id }`           | array, each element projected                       |
| `orders[:10] { id }`      | slice; negative indices count from the end          |
| `metadata` (no braces)    | whole subtree, verbatim                             |
| `metadata { * }`          | one level of keys, values verbatim                  |
| `owner: author { login }` | alias                                               |
| `id!`                     | fail loudly if the response omits it                |
| `status == "ACTIVE"`      | emit this field only when its value equals the literal |
| `status != "DELETED"`     | emit this field only when its value differs from the literal |
| `name in ["Mike", "X"]`  | emit this field only when its value is in the list |
| `name not in ["Mike", "X"]` | emit this field only when its value is not in the list |
| `profiles[? name == "Mike"] { name age }` | keep only matching array objects, then project |
| `items[id]`               | one value per element, flattened into a list        |
| `items[0:5][id]`          | slice first, then collect                           |
| `children...{ id }`       | walk a repeated key, flattening projected nodes |
| `children**{ id }`        | project every level of a repeated key, keeping the tree |
| `items...children[? id == 1] { id }` | start at items, follow children, flatten matching nodes |
| `items**children[? id == 1] { id }` | follow children, retaining matching nodes and ancestor containers |
| `* !secret`              | keep everything except `secret` at this level |
| `* !secret...`           | keep everything except `secret` at any depth |
| `children...[id]`         | walk it, then flatten to bare values                |
| `# comment`               | ignored                                             |

Field names may contain `-`, `.`, `@`, `$` and unicode, so `content-type` and
`x-amz-date` work without quoting.

### Conditional fields

Use `==` or `!=` to decide whether to include a field based on its original
value:

```graphql
{ id status == "ACTIVE" }
{ id status != "DELETED" }
{ count == 42 enabled == true optional == null }
{ profile != null { displayName } }
```

For `[{"id":1,"status":"ACTIVE"},{"id":2,"status":"DELETED"}]`, the first two
queries both return `[{"id":1,"status":"ACTIVE"},{"id":2}]`. A failed comparison
**omits the field**, not the containing object or array element. Empty objects
and array positions remain. Missing fields are omitted for both operators;
missing is not the same as an explicit JSON `null`.

Comparison literals are JSON double-quoted strings, numbers, booleans or `null`.
Equality is type-sensitive: `42` does not equal `"42"`, and `false` does not
equal `0`. Strings compare decoded JSON content, including Unicode and escapes.
Numbers compare exact decimal values: `1`, `1.0`, and `1e0` are equal, without
rounding large integers through floating point. Matched values keep their
original JSON representation. Objects and arrays cannot be comparison literals;
they are unequal to any scalar literal, so `profile != null { name }` can omit a
null profile while projecting a non-null one. Conditions are checked before
the nested selection is applied.

Aliases work (`{ current: status == "ACTIVE" }`). An explicit conditional field
still wins over `*`, even when it fails its condition; wildcard fallback does
not re-add it. Exclusions still win over both. `status! == "ACTIVE"` requires
the field to exist, but a present non-matching value is simply omitted.

Use comparisons on named fields, not directly on `*`, exclusions, recursive
edges or bracket suffixes. Compare fields **inside** those selections instead:
`{ items[:5] { status != "DELETED" } }` or
`{ children** { id status == "ACTIVE" } }`.
Conditional collects such as `{ items[id == 1] }` produce projected objects
(`[{"id":1},{}]`), not the bare-value shortcut used by unconditional `[id]`.
Only one comparison is supported per field; there are no boolean expressions,
field-to-field comparisons or general-purpose scripting. The desktop can resolve
scoped variables inside comparison strings, as described below.
The `=` character is reserved for comparison syntax in field names.

### Membership and array filters

Use `in` or `not in` with a comma-separated list of JSON scalars. They use the
same exact, type-sensitive equality as `==`:

```graphql
# Keep every profile and its age; emit name only for Mike or X.
{ profiles { name in ["Mike", "X"] age } }

# Keep only matching profile objects, returning their name and age.
{ profiles[? name in ["Mike", "X"]] { name age } }

# Exclude those names instead.
{ profiles[? name not in ["Mike", "X"]] { name age } }
```

For `{"profiles":[{"name":"Mike","age":30},{"name":"Anna","age":25}]}`,
the first query retains both profiles, omitting Anna's `name`. The second returns
`{"profiles":[{"name":"Mike","age":30}]}`. Filters also accept `==` and `!=`:
`{ profiles[? name == "Mike"] { name age } }`.

Missing fields never match any of these operators, including `not in` and `!=`.
Explicit `null` matches `[null]`. `in []` never matches; `not in []` matches every
present value. Object and array field values are not members of any scalar list.
Lists may mix scalar types, but cannot contain objects, arrays, or expressions.
Comments and whitespace are allowed; commas separate values and trailing commas
are rejected.

`[? ...]` filters the immediate **object elements of an array**. Non-object
elements and objects missing the tested key are removed, even for negation.
No matches yields `[]`; a missing array field remains omitted. Applying a filter
to a present non-array value (including `null`) reports an error. Without a
following selection, matching objects retain their complete contents.

Predicates inspect the original object before projection, aliases, or exclusions,
so the tested field need not appear in the output. Names match literal keys;
`profile.name` tests that exact key, not a nested path. Use nested braces to
follow object fields, ending in one condition:

```graphql
{
  payload {
    events {
      items[? definition { item_id { value == "2762467" } }] {
        definition { item_id { value } }
      }
    }
  }
}
```

First the predicate inside `[? ...]` decides which **items** survive; then the
selection after it chooses their output fields. The predicate does not project
or change the item. For example, the same filter can be followed by `{ label }`
without returning `definition` at all. Omit the selection to keep whole matching
items. Use `value == 2762467` without quotes for a numeric ID, or
`value in ["2762467", "2762468"]` to accept multiple string IDs.

Each predicate is one path with one field at each level, ending in `==`, `!=`,
`in`, or `not in`. Missing path fields and null, scalar, or array intermediate
values do not match, including for negated conditions. Nested braces follow
objects only; they do not implicitly search arrays. Paths are limited to 256
nested object steps. Aliases, required markers, exclusions, recursion, bracket
suffixes, and multiple sibling fields are not supported inside predicates.
There is no `===`, `!==`, or general-purpose expression evaluation.

Slices and filters run in written order:

```graphql
{ profiles[? name in ["Mike", "X"]][:5] { name age } } # first five matches
{ profiles[:5][? name in ["Mike", "X"]] { name age } } # matches among first five
{ profiles[? name == "Mike"][age] }                    # ages of matching profiles
```

Multiple filters can narrow the same array; collects must come last. Filters can
also follow `...` and `**` recursive edges, as described under Trees below.
All operations are local views: the stored response remains unchanged.

### Query editor and variables

The desktop query editor colors field names, strings, numbers, operators, and
comments. Braces and brackets share a color with their matching partner at each
depth. Place the caret next to a delimiter to underline its matching pair;
unmatched delimiters get a wavy underline. Native text editing, selection, undo,
multiline sizing, and scrolling are retained.

Use the existing `{{name}}` variables **inside double-quoted comparison strings**:

```graphql
{
  payload {
    events {
      items[? definition { item_id { value == "{{myVar}}" } }] {
        definition { item_id attributes }
      }
    }
  }
}
```

The desktop resolves globals, selected environment, collection,
folder, and request scopes in the same order as requests. Service collections
also require a valid selected deployment when resolving variables, and generated
deployment inputs take precedence. Apply the query again after changing a value
or environment. Local current values take the place of defaults in their own
scope, without changing scope precedence. Changing a local value does **not**
reevaluate an existing response view. Variable tokens use the same hover/edit card as request fields;
undefined names are amber. Named queries retain placeholders when saved.

Only parsed string literals are expanded, including strings in `in` / `not in`
lists and embedded text such as `"prefix-{{name}}"`. A variable containing `42`
remains the string `"42"`, not the number `42`. Unquoted `{{name}}`, substitution
in field names, and executable expressions are not supported. Comments are never
expanded. These bindings are a desktop workspace feature, not CLI substitution.

Resolution and secret values stay in Rust. Values containing quotes, newlines,
backslashes, or braces remain literal data and cannot inject query syntax.
Undefined or unresolved variables produce an error against the original query,
without exposing expanded values. Each successful query evaluation freezes its
bound literals in Rust. Response-file export uses that applied view, not current
variables or unapplied query edits; neither the stored query nor captured
response body is rewritten.
History-only tabs have no current request scope and cannot resolve query
variables; literal queries and empty-query exports remain available there.

#### Comparing the same request in multiple tabs

Click **Duplicate tab** beside **History** to open an independent copy of the
current request draft and query text. No request is sent and no files are copied.
Each tab has its own draft, captured response, query text, and last applied view;
copies are numbered in the tab strip. Saving still updates the same underlying
request YAML, but does not discard another copy's in-app edits or response.
New duplicates start without a response, so a copied draft is not mistaken for
a second completed request.

A response query runs when a new response arrives, when you click **Apply** or
press **Enter**, when you select a saved query, or when you explicitly **Suggest**
and apply a starter query. Editing query text, changing variables or environment,
and switching tabs do not reevaluate an existing view. Explicit Apply uses the
current live values. In-flight projections complete into their own tab's cache,
never into another tab's visible response.

For example, send for store A and apply a query filtering its first category.
Duplicate the tab, change the live store/category variables, then send for store
B. Both results stay intact when switching between them. Live variables remain
shared according to their normal scopes; they are not per-tab variable editors.

**Save to file** exports the last successfully applied view with its original
bindings, even after variables change or the query editor contains unapplied
edits. Apply an empty query first to export original response bytes. A failed
query leaves the last successful view available. Views are in-memory only, with
up to 24 stored response bodies and 96 bound-query snapshots; exporting an
evicted view reports an error instead of silently rebinding it.

### Trees

A category tree is the case a fixed-depth projection gets wrong: you have to
guess the depth, and a deeper response silently truncates.

```
{ ids: children...[id] }
```

```json
{"ids": [2, 4, 7, 5, 3]}
```

Depth-first, document order. Leaves without the key are skipped, never errors.
You do not have to write a fixed number of levels in the query; the scanner's
256-level safety limit still applies and reports an error rather than truncating.

The walked edge is consumed, never emitted. That is why `children...{ * }` is
safe: it gives you every node flattened, minus the nested tree, instead of
re-emitting each subtree at every level.

`children...[0:3][id]` bounds the *output* without bounding the traversal.

Use `**` instead of `...` to **retain the parent–child structure** while selecting
only particular fields at every level:

```graphql
{
  payload {
    events {
      items {
        definition { item_id }
        children** { definition { item_id } }
      }
    }
  }
}
```

The `children` edge is included automatically at each level. Missing edges stay
missing; empty arrays and `null` remain unchanged. An alias renames only the
starting edge, not the nested `children` keys. `**` requires a selection set and
accepts optional filters, but not slices or collects. Existing `children...{ ... }` queries still
flatten descendants into one list.

#### Filtering at any depth

When the initial array is `items` but each item's descendants live in `children`,
write the descendant key immediately after the recursion suffix: `items...children`
or `items**children`, without whitespace between the suffix and the key.
This includes the initial items, then follows `children` at every level. With no
explicit descendant key, `children...` and `children**` continue to follow their
own key as before. Neither form searches arbitrary object paths.

Use **three dots** for a flat list of matches, in depth-first document order:

```graphql
{
  payload {
    events {
      items...children[? definition { item_id { value == "{{myVar}}" } }] {
        definition { item_id attributes }
      }
    }
  }
}
```

Use **two stars** to retain hierarchy instead:

```graphql
{
  payload {
    events {
      items**children[? definition { item_id { value == "{{myVar}}" } }] {
        definition { item_id attributes }
      }
    }
  }
}
```

Both forms evaluate predicates against original node values and continue into
children even when the parent does not match. Flat output contains only matching
nodes, never the traversed `children` subtree. Tree output contains projected
matching nodes and retains non-matching ancestors **only as `children` containers**
when they lead to a match; it removes branches with no matches. A matching parent
does not automatically include non-matching descendants. No matches yields `[]`.
Required output fields are checked only on matching nodes, not ancestor containers.
Excluding the descendant edge stops traversal through that branch.

For example, an unmatched parent above a matching `{ "id": 2 }` produces
`[{"id":2}]` in flat mode and `[{"children":[{"id":2}]}]` in tree mode.
On matching tree nodes, empty child arrays and null/scalar terminal branches
remain; the filtered starting field must be an array.

All existing predicate operators and desktop string variables work here. Recursive
flat filters must precede slices: `{ items...children[? id in [1,2]][:5][id] }`
takes the first five matching values across the complete walk, without stopping
traversal. Collects still come last. Tree mode requires a selection and supports
filters only, not slices or collects. The 256-level nesting limit is unchanged.

### Excluding fields

```graphql
{ * !debug }                       # omit debug at this level
{ * !debug... }                    # omit debug throughout the response
{ * payload { * !debug... } }       # omit debug only within payload's subtree
{ * payload { * !largeField } }     # omit one particular nested field
```

Exclusions match original field names (before aliases), and win over explicit
selections and `*`. `!debug` does not affect deeper `debug` properties; `!debug...`
also filters whole subtrees copied by a wildcard or an unprojected field, including
objects inside arrays. Missing excluded fields are harmless. Exclusions do not
implicitly select other fields: use `*` for “everything except”. Dots remain
literal parts of names, so nest selections to address a particular path.

Prefix `!field` excludes a field; suffix `field!` still requires its presence.
Exclusions are response views, not a security boundary: the original response
and any stored history remain unchanged.

## Layout

```
crates/response-query/   the engine
  src/syntax.rs          parser, diagnostics with line:col
  src/scan.rs            byte scanner, no DOM, no allocation
  src/project.rs         the projector
  src/schema.rs          internal Schema type, merge, fields_at
  src/infer.rs           schema from observed responses
  src/openapi.rs         schema from OpenAPI 3
  src/validate.rs        selection vs schema, with `did you mean`
crates/collection/       the on-disk workspace, importers, and sending
  src/model.rs           collection, item, request, variable
  src/postman.rs         Postman v2.1 collection + environment
  src/store.rs           per-request files, secret splitting, leak guard
  src/send.rs            variable substitution and the HTTP call
crates/desktop/          the window (Tauri + plain HTML/CSS/JS, no bundler)
  src/main.rs            commands, JavaScript scripts, response storage and history
  ui/                    the front end
crates/rq-cli/           thin driver binary
queries/                 saved queries, committed
examples/jsonplaceholder/  a workspace to open in the app
```

An imported workspace is one **YAML file per request**, because a git-synced
collection has to survive review and merge:

```
collections/acme-orders/collection.yml               name, description, auth, variables
collections/acme-orders/deployments.yml              optional tenant/platform/slot placements
collections/acme-orders/01-orders/folder.yml         folder display name, optional auth
collections/acme-orders/01-orders/01-list-orders.yml name, method, url, params, headers, body
collections/acme-orders/02-admin/01-purge-cache.yml
environments/dev.yml
.secrets/dev.env                                     gitignored, values only
globals.yml                                         workspace-wide fallback variables
.secrets/globals.env                                gitignored, global secret values
```

Auth is written only where it is *set*, and inherited downwards like Postman
does — so changing the token scheme is one edit in `collection.yml`, not a diff
in every request:

```yaml
# collections/acme-orders/collection.yml
auth:
  kind: bearer
  token: '{{apiToken}}'

# collections/acme-orders/01-orders/01-list-orders.yml   <- no auth key: inherits
name: List orders
method: GET
url: '{{baseUrl}}/v1/orders/:orderId'
path_params:
  orderId: '7'
params:
  - name: page
    value: '2'
  - name: debug
    value: '1'
    disabled: true
headers:
  - name: Accept
    value: application/json
  - name: X-Debug
    value: '1'
    disabled: true

# collections/acme-orders/02-admin/folder.yml           <- overrides for the folder
auth:
  kind: apikey
  key: X-Api-Key
  value: '{{adminKey}}'

# collections/acme-orders/03-health.yml                 <- stops the chain
auth: none
```

The URL is the **base** only. Every query parameter lives in `params`, enabled or
not, because a parameter that is switched off is intent, not absence — deleting
it loses the value you will want back tomorrow. A `?a=1` typed into the URL box
is normalised into `params` on load (and on blur), so there is one form on disk.
A whole `:name` path *segment* is a path variable whose value lives in
`path_params`; only a segment that starts with `:` counts, so `https:` and gRPC's
`v1:batchGet` are untouched. Sending with an unfilled `:id` is an error that
names the parameter rather than a request to a literal `:id`.

Three states, because "no auth" and "whatever the folder uses" are different
intentions: a health check inside an authenticated folder has to be able to opt
*out*, and that has to survive a round trip. `Collection::requests()` resolves
the chain and hands back each request with the auth it will actually send.

The `NN-` prefix *is* the order, for folders and requests together, so there is
no manifest listing children that can drift from the tree. Reordering renames
files, which git records as renames rather than rewrites. `store::load` reads the
tree back, including re-attaching secret values from `.secrets/`; a round-trip
test asserts the loaded collection equals the imported one.

Identity is separate from order and name. Every `collection.yml`, `folder.yml`
and request file starts with an `id:` (eight hex characters, unique within its
collection). Per-user state outside the workspace, local current values and
request history, refers to collections, folders and requests by these ids, so
it follows them through renames, moves and reordering. The app and `rq send`
add missing ids when they open a workspace, editing only that one line in each
file; a copied file that repeats an id gets a new one, and duplicating in the
app always assigns fresh ids. Nothing else should depend on an id's value.

### Variables

Variables resolve from widest to narrowest scope: `globals.yml`, the selected
environment, collection, each folder from outermost to innermost, then the
request. A later definition overrides an earlier one. Globals provide a useful
fallback even when no environment is selected. Collections, folders, and
requests can compose values from any wider scope, for example:

```yaml
# globals.yml
variables:
  - name: apiOrigin
    value: https://api.example.test

# collections/acme-orders/collection.yml
variables:
  - name: baseUrl
    value: '{{apiOrigin}}/v1'
```

Expansion uses the merged scope map. A cycle or a chain deeper than 32 levels is
reported and left visible as an unresolved `{{variable}}`; unresolved names
nested inside another value are reported too. Secret values are masked before
expansion in cURL previews, so composing a secret into another variable cannot
expose it. Globals and collection, folder, request, and environment secrets are
stored under `.secrets/`, never in committed YAML.

The desktop separates **saved defaults** from **local current values**. Hover a
`{{name}}` token and type to automatically update its current value without
saving a request, collection, or environment. Current values persist beside the
last-workspace setting in the user config directory:

```text
%APPDATA%/rq/state.json       last workspace (Windows)
%APPDATA%/rq/variables.json   local current variable values, grouped by workspace and scope
```

On macOS this directory is `~/Library/Application Support/rq`; on Linux it is
`$XDG_CONFIG_HOME/rq` (or `~/.config/rq`). Nothing is written inside the workspace.
A local override replaces only its own scope's default; a narrower saved
definition still wins over a wider local value. Requests, cURL previews/copy,
deployment URL resolution, and new query evaluations use these current values.
Existing response views and their exports retain their applied bindings instead.
Current values survive refreshes and app restarts, and other workspaces
and environments have independent values. They are stored by collection,
folder and request id, so they stay with their request when it is renamed or
moved; values saved by older versions (by tree position) are read as before and
rewritten by id on the next change. Generated deployment inputs stay
read-only.

**Use saved value** removes the winning local override, restoring the saved
default (or removing a local-only definition). The collection, environment, and
globals editors explicitly edit **saved defaults** instead. Saving unrelated
edits never promotes local current values into YAML. Local config is not an
encrypted credential vault: secret overrides, like existing `.secrets/` files,
are stored locally in plaintext; they remain masked in previews and are never
returned to the webview. Protect the config directory and its backups accordingly.

### Services and deployments

A collection can optionally represent a service deployed across tenants,
platforms and slots. Ordinary collections need no changes. Configuration is
edited in YAML; the window provides target selection, not deployment editors.

Each environment owns its platforms and their URL patterns. Platform names must
be unique across the workspace, including across environments:

```yaml
# environments/stage.yml
name: stage
platforms:
  - name: stage-a
    url_template: 'https://{{serviceName}}-{{slot}}.stage-a.example.test/{{tenant}}'
  - name: stage-b
    url_template: 'https://{{tenant}}.stage-b.example.test/{{serviceName}}/{{slot}}'
```

The collection defines its technical service name, independently of its display
name. Placement records live in a separate file beside `collection.yml`:

```yaml
# collections/navigation-core/collection.yml
name: Navigation Core
service_name: navsvc

# collections/navigation-core/deployments.yml
deployments:
  - tenant: acme
    platform: stage-a
    slot: stable
  - tenant: acme
    platform: stage-a
    slot: candidate
  - tenant: globex
    platform: stage-b
    slot: stable
```

Deployments have no variables or repeated environment names. Their environment
comes from the platform's membership. Tenant/platform/slot combinations must be
unique per service. Technical names use ASCII letters, digits, `-`, `_`, or `.`.
Platform templates accept `{{serviceName}}`, `{{tenant}}`, `{{platform}}`,
`{{slot}}`, `{{slotSuffix}}`, and `{{basePath}}`, and must produce an HTTP(S) base URL without embedded credentials,
query parameters or fragments. A trailing slash is removed.

Requests use `{{baseUrl}}/endpoint`. With Acme / stage-a / candidate selected,
the example resolves to `https://navsvc-candidate.stage-a.example.test/acme/endpoint`.
For deployment-enabled collections, `serviceName`, `tenant`, `platform`, `slot`, `slotSuffix`, `basePath`
and the generated `baseUrl` are read-only built-ins: they override definitions
in every variable scope before other variables expand. Scripts cannot assign
them. Ordinary collections retain their existing variable precedence.

Select an environment, then a tenant and a platform/slot deployment above the
request URL. Only configured placements in that environment are offered. The
resolved base URL is visible before sending. Selections are remembered locally
per workspace and collection; an invalidated selection is cleared, never
replaced with another tenant or target. Missing selections and stale platform
references prevent sending and cURL export. Response badges and history retain
the originating service, environment, tenant, platform, slot and resolved base
URL; switching target leaves the response usable and marks mismatches amber.
Existing collection/environment saves preserve deployment configuration.

#### Named main slot (no URL suffix)

An environment can name its primary slot once for **all services and platforms**
in that environment. The name is configurable, not a hard-coded special case:

```yaml
# environments/stage.yml (set main_slot in production.yml too if applicable)
name: stage
main_slot: default
platforms:
  - name: stage-a
    url_template: 'https://grid-{{tenant}}{{slotSuffix}}.stage-a.example.test/{{serviceName}}{{slotSuffix}}'
```

`{{slotSuffix}}` includes its own hyphen: it is empty for the configured main
slot, and `-<slot>` for any other slot. Use it wherever the optional suffix
belongs, including more than once in a URL or inside a collection's `base_path`:

```text
default → https://grid-brand.stage-a.example.test/catalog
worker  → https://grid-brand-worker.stage-a.example.test/catalog-worker
```

Keep `slot: default` in `deployments.yml`: the selector, response provenance,
scripts, query variables, and conditions still see the real slot name through
`slot`. Only the URL suffix is omitted; tenant and service identity never change.
For custom names such as `main_slot: primary`, the same rule applies to `primary`.
This setting and the URL templates are edited in environment YAML.

Existing `{{slot}}` placeholders remain literal for backwards compatibility.
Replace `-{{slot}}` with `{{slotSuffix}}` where omission is wanted—do not write
`-{{slotSuffix}}`, since that would leave an extra hyphen. Without `main_slot`,
every slot gets a suffix, including `default`; no slot is chosen implicitly.

#### Conditional collection base paths

When a path segment differs by tenant but the service name in the hostname must
stay the same, give the collection an optional `base_path`:

```yaml
# collection.yml
name: Catalog Demo
service_name: shopsvc
base_path: '/{{tenant == "north" ? serviceName : "legacysvc"}}-{{slot}}'

# The selected platform in environments/stage.yml
platforms:
  - name: stage-c
    url_template: 'https://{{serviceName}}.{{tenant}}.{{platform}}.example.test{{basePath}}'
```

With slot `stage`, tenant `north` gets
`https://shopsvc.north.stage-c.example.test/shopsvc-stage`, while tenant `south`
gets `https://shopsvc.south.stage-c.example.test/legacysvc-stage`. The tenant and
service identity are unchanged; only the chosen path segment differs. Requests
still use `{{baseUrl}}/catalogs/:catalogId/categories` without expressions in
their endpoint paths.

Expressions are supported **only inside the collection's `base_path`**, not in
platform templates, service names or ordinary request variables. They accept:

- The inputs `serviceName`, `tenant`, `platform`, `slot`, and `slotSuffix`.
- Single- or double-quoted strings, with escaped quotes and backslashes.
- String comparisons `==` and `!=`.
- `condition ? value : fallback`, including nested conditions and parentheses.

There is no JavaScript execution, function access, secret lookup, assignment,
string concatenation or general-purpose scripting. Adjacent literal text and
`{{…}}` expressions compose the path. Templates are limited to 16 KiB and
expressions to 32 nesting levels.

The result must be empty or start with one `/`, without a query, fragment,
backslash or whitespace. A trailing slash is removed. Platforms used by a
collection with `base_path` must explicitly include `{{basePath}}`; a missing
placeholder is an error, never an ignored override. Without `base_path`, the
built-in `basePath` is empty and existing platform templates work unchanged.
Resolution errors block sending and cURL export. Rust resolves the path once
for each preview/send/export; the desktop displays the backend-resolved URL
and keeps Send disabled while a conditional path is awaiting resolution.

The **Catalog Demo** in `examples/deployments/` demonstrates this without any
live endpoints or credentials.

The CLI uses the same resolution rules and requires an explicit target:

```powershell
cargo run -p rq-cli -- send work --collection "Navigation Core" --request "Menu" --environment stage --tenant acme --platform stage-a --slot candidate
```

The schema is an **internal abstraction** with providers, which is what makes
spec-less APIs first-class:

| Provider | Source             | Works for                       |
| -------- | ------------------ | ------------------------------- |
| OpenAPI  | `--spec`         | documented operations           |
| Inferred | captured responses | everything else, after one call |
| `Any`  | neither            | accepts anything                |

## The window

`cargo run -p rq-desktop -- examples/jsonplaceholder` opens a workspace from disk;
the last one is remembered. Three panes: the tree, the request, the command.

- Search the collection tree by request name or URL. Open requests, folders,
  collections, and historical responses in tabs; **Duplicate tab** opens another
  independent draft, response, and query view of the same request. The side panes resize and collapse.
- **Params** has two sections: path variables derived from the `:name` segments
  in the URL (the name is read-only, the value is not), and query parameters with
  an enable box each. Unchecking a parameter keeps the row and its value but
  leaves it out of the send.
- **Auth** is a mode per level — inherit, none, bearer, basic, API key — settable
  on the collection, a folder, and the request. The badge beside the request shows
  what will actually be sent; its tooltip says whether that was set here or
  inherited from above.
- **`{{variables}}`** are tinted in every field. Amber means the name is not
  defined; hovering a defined one opens a card with its value, editable in place.
   The card shows the winning definition and wider definitions it shadows. Typing
   automatically edits the narrowest existing definition's local current value; for a new name, choose whether to
  create it in the collection, selected environment, or globals. Folder and
  request scopes are edited through the same card. A secret's value never
  crosses into the window, so its box starts empty and empty means "keep the
   stored value". A local badge identifies overrides; **Use saved value** clears one.
- The environment controls add, edit, rename, and delete environments, including
  their TLS verification setting and secret variables. **Globals** opens the
  workspace-wide fallback variable editor.
- **Import** reads Postman collection, environment, or globals JSON exports into
  the open workspace, creating YAML under `collections/`, `environments/`, or
  `globals.yml`. Existing collections and environments are not overwritten;
  globals add new variable names and reject duplicates. Postman secret variables
  and any additional secret names entered in the dialog go only into `.secrets/`.
  Import warnings are shown in the dialog; event scripts are not imported.
- Edits are drafts, marked with a dot in the tree, and they survive switching
  requests. **Save** is explicit: it re-renders in Rust and writes only the
  difference, so a secret file is never rewritten and a file the tool did not
  write is never deleted. The command pane shows the same request as a runnable
  syntax-highlighted cURL, with secrets masked to `***` and a warning when a
  `:path` value is missing. **Copy protected** keeps those masks. **Copy with
  secrets…** asks for confirmation, then resolves the current draft and copies
  the real values directly from Rust to the system clipboard. The preview stays
  masked. Clipboard history, synchronization, or other applications may retain
  the unprotected command; paste it only into a trusted location.
- Collection, folder, and request scripts can run before a request or after its
  response from their **Scripts** editors; see [Scripts](#scripts) below.
- JSON request bodies and JavaScript pre-request/post-response scripts have
  syntax highlighting while retaining native text editing and selection.
  JSON request bodies support `//` line comments: they remain in the editor and
  saved YAML, but are stripped from sent bodies and cURL previews/copies before
  variable substitution. Quoted strings (including URLs and literal `//`) stay
  untouched. Other body languages are sent unchanged.
- **Send** uses the current request draft; it becomes **Cancel** while a request
  is pending. The response shows status, timing, size, unresolved variables, TLS
  warnings, and post-response script errors.
  Its environment badge records where the response came from. Changing, editing,
  or deleting an environment keeps existing responses and queries usable; the
  badge turns amber when the response's environment differs from the selection.
  Older history without a recorded environment is labeled as unknown.
- The response viewer pretty-prints JSON with syntax highlighting, collapsible
  objects and arrays, breadcrumb navigation, and key/value search. Response line
  numbers cover JSON and plain text without becoming part of copied text. Large JSON
  views use virtualized rows. The **Query** bar re-projects locally on **Apply** or Enter;
  **Suggest** infers a starter query from the response without an OpenAPI spec.
  Leave the query empty to inspect the full JSON response. Non-JSON text views
  are capped at 512 KiB; the original bytes remain in Rust.
  **Save to file** opens a native save dialog and exports the last applied query's
  result as formatted JSON, retaining its variable bindings. After applying an empty query it saves the full original body,
  preserving its bytes even when the text preview is truncated. Line numbers and
  folded-view markers are not included. Captured responses may contain sensitive
  data; choose the destination accordingly.
- Save named projections per request: enter a query and a **Query name**, click
  **Keep query**, then **Save** the request. The saved-query selector becomes
  available once a response arrives and immediately applies the chosen view
  without another HTTP call. Freeform edits remain possible; **Keep query** with
  the same name updates it after confirmation. **Remove** stages its deletion;
  **Save** makes query additions, updates, and removals durable. Queries live in
  the request YAML as a `queries` map from names to query text, so they travel
  with the request and survive restarts. History tabs remain read-only.
  The query editor supports multiline text (**Shift+Enter** for a newline;
  **Enter** applies the query), including comments in saved queries.
- **History** records successful and failed sends, with a per-request history
  view and read-only tabs for stored responses. Response bodies persist in the
  app's local data directory, with a configurable storage limit (250 MiB by
  default); older bodies are pruned while request metadata remains. Set the
  limit to zero to retain no response bodies, or use **Clear history** to remove
  the history. Stored URLs and response bodies may contain sensitive data.

### Scripts

The desktop app runs JavaScript at collection, folder, and request level. Both
**Pre-request** and **Post-response** run in this order: collection, enclosing
folders from outermost to innermost, then request. Scripts are saved in YAML as
`scripts.pre_request` and `scripts.post_response`.

- Pre-request scripts run before variable substitution and can change
  `pm.request.method`, `pm.request.url`, headers via `add` / `upsert`, and
  `pm.request.body.raw`. A pre-request error stops the main send.
- Post-response scripts can read `pm.response.code`, `responseTime`,
  `responseSize`, `text()`, and `json()`, for example to capture a response value
  into a variable for the next request. Errors are shown without discarding the
  response; post-response scripts do not run after a transport failure.
- `pm.variables`, `pm.globals`, `pm.environment`, and `pm.collectionVariables`
  provide `get` / `set`. Getters read the effective merged variables.
  `pm.variables.set` defaults to the current script's scope; its optional third
  argument selects `global`, `environment`, `collection`, `folder`, or `request`.
- Variable writes persist automatically as local current values in app config,
  never through shared workspace files. Existing secret flags are retained;
  newly created variables are **not secret**
  by default, so create a secret variable in the editor before assigning tokens
  from a script. Environment writes require a saved environment.
- `pm.sendRequest` supports callbacks and `await`, with a URL string or a request
  object containing a URL, method, header array, and raw body. These extra HTTP
  calls use variable substitution and the environment's TLS setting, but do not
  inherit the main request's authentication.

#### Shared scripts

A script used by several collections lives once in `scripts/<name>.js`, a
plain JavaScript file whose first line is its id:

```js
// scripts/milestones.js
// @id 3f9a12c0
const user = pm.globals.get('user');
// ...
```

Layers include it by id, separately for each phase, so the same file can run
before the request in one place and after the response in another:

```yaml
# collections/acme-orders/collection.yml
scripts:
  include:
    pre_request: [3f9a12c0]
    post_response: [b71e04aa]
  pre_request: |
    // runs after milestones.js
```

Each id runs just before the including layer's own script of the same phase,
in list order, and with that layer's scope, so `pm.collectionVariables.set` in
a shared script writes to the including collection. Folders and requests can
include too. Shared scripts cannot include others. The file name is only for
people: renaming or moving a file between names changes no reference. Ids may
use letters, digits, `-` and `_`, and must be unique; the app generates them.
An include whose id no file declares fails the send with an error naming it.
In the app, tick shared scripts in a collection's or folder's **Scripts**
section and edit them from the ✎ next to each, or **⋯ → Shared scripts…**.
`console.log` / `info` / `warn` / `error` output appears in the **Console**
section of the right-hand panel (and on stderr for `rq send`).
This is a Postman-style subset, not a complete Postman sandbox or test runner:
`pm.test` is a no-op and full assertion APIs are not implemented. Postman import
does not import event scripts. CLI `rq send` runs the same pre-request and
post-response scripts as the app, reads the app's current values from
`<config dir>/rq/variables.json`, and saves script writes back there.
The cURL preview represents the request draft, not the result of running scripts.

See the [scripts and response queries tutorial](docs/scripts-and-queries.md) for
`pm.sendRequest`, variable scopes, and query examples.

## Commands

```powershell
cargo test

# project a response
cargo run -p rq-cli -- gen --lines 5000 > big.json
cargo run -p rq-cli -- project -q queries/orders/brief.query big.json --pretty

# check a query, with schema validation if you have a spec
cargo run -p rq-cli -- check -q queries/orders/brief.query
cargo run -p rq-cli -- check -q queries/orders/brief.query --spec openapi/fixture.json --operation listOrders

# list operations, so you know what --operation accepts
cargo run -p rq-cli -- ops --spec openapi/fixture.json

# what the field picker will call: fields at a path, with types and required flags
cargo run -p rq-cli -- fields --spec openapi/fixture.json --operation listOrders --path data.items
cargo run -p rq-cli -- fields --spec openapi/fixture.json --operation listOrders --path data.items.payment

# generate a starter query from the response schema
cargo run -p rq-cli -- scaffold --spec openapi/fixture.json --operation listOrders --depth 3

# build a schema from real traffic, for the APIs with no spec
cargo run -p rq-cli -- infer response1.json response2.json --out schema.json --pretty

# send a stored request and reshape what comes back, without saving it
cargo run -p rq-cli -- send examples/jsonplaceholder --request "Get post" | `
  cargo run -p rq-cli -- project -q queries/orders/brief.query -

# same request against an environment, and with certificate checks off
cargo run -p rq-cli -- send work --request "List orders" --environment dev --insecure --out response.json

# start from what you already have: Postman collections and environments
cargo run -p rq-cli -- import postman "examples/jsonplaceholder.postman_collection.json" --out work

# the app: opens a workspace, sends, and applies local response queries
cargo run -p rq-desktop -- examples/jsonplaceholder
```

`rq gen` is deterministic for a given seed, so benchmarks are comparable across
machines. Line counts are approximate; the exact number is printed to stderr.

## Status

Working today: parser, projector (slices, collect, recursive walks), OpenAPI
provider (JSON + YAML), inference, validation with suggestions, starter-query
generation, Postman v2.1 import with secret splitting, and CLI request sending.
The desktop app adds draft editing and saving, scoped variables and auth,
pre-request and post-response scripts, tabbed requests, searchable/virtualized
JSON viewing, local response queries, cancellation, and persistent history.

The window is not a mockup: it reads the workspace from disk, sends through the
same HTTP core the CLI uses, and applies queries locally against a body
held in Rust — so reshaping a response makes no additional network calls.
Scripts can make additional calls explicitly through `pm.sendRequest`.

Measured on a generated 500,000-line / 10.8 MB response:

```
rq project (slice + alias + nested select)   42 ms wall, output 559 bytes
node JSON.parse + JSON.stringify             88 ms, 42 MB heap, no projection
```

The comparison understates the difference — `rq` never builds a tree, so its
cost is one linear scan, while the Node figure is paid again for every re-query.

## Toolchain

`rustup` + `rust-lld` link fine here without Visual Studio, and nothing in the
workspace needs a C compiler: TLS is SChannel through `native-tls`, not `ring`.
The window needs two Windows extras — the WebView2 runtime (already on Win10/11)
and `rc.exe` from the Windows SDK, which `tauri-build` uses to embed the icon and
manifest:

```powershell
$sdk = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Directory |
       Where-Object Name -like '10.*' | Sort-Object Name | Select-Object -Last 1
$env:Path = "$($sdk.FullName)\x64;$env:Path"
cargo run -p rq-desktop -- examples/jsonplaceholder
```

```powershell
cargo check --workspace --all-targets   # types, no linking
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

## Notes

- Specs may be **JSON or YAML** (JSON is tried first, then YAML). YAML goes
  through `serde_norway`, the maintained fork of the archived `serde_yaml`.
- Models that reference themselves (`Node.parent`, `Node.children`) are expanded
  one level past the cycle and then cut. Expanding in both directions eagerly is
  exponential — a two-field tree model hangs before that fix — but cutting to
  `Any` immediately would leave `children` untyped and lose completions where
  they matter most.
- A recursive walk re-scans each node's subtree once per level, so it costs
  `O(bytes × depth)` rather than `O(bytes)`. Real trees are shallow; the depth
  limit turns a pathological chain into an error instead of a truncation.
- `oneOf` / `anyOf` are treated as a **union of properties**. Two branches that
  declare the same key with different types degrade to a looser type rather
  than pretending to narrow. Branch discriminators are a later problem.
- `infer` reuses the same byte scanner as `project`, so it never builds a DOM.
  Empty arrays and all-null fields cannot be typed and become `Any`.
- The scanner enforces a depth limit rather than risking a stack overflow.
- The importer trusts Postman's `url.raw` when it is present. The object form
  also lists `query[]`, and honouring both would send every parameter twice.
  When only the object form exists, the URL is assembled and enabled parameters
  are appended — without percent-encoding, which Postman's `raw` already has.
  Either way the query is then normalised out of the URL into `params`, which is
  the form the window edits and the sender appends.
- The window sends the *draft's* auth resolved against
  `Collection::inherited_auth_at(path)`, not the auth the tree was loaded with.
  Changing a request's auth therefore takes effect on Send and on the cURL
  preview before it is saved; saving just makes it durable.
- `save_environment` goes through the same renderer, the same delta writer, and
  the same leak scan as a collection. Editing one variable from the window is not
  a second path to disk.
- The `{{variable}}` tint is a pointer-transparent layer painted above the input,
  not a `contenteditable`. The caret, selection, undo, and IME stay native; the
  token under the pointer is found from the layer's rectangles, so a click still
  places the caret in the input underneath.
- Disabled headers and variables survive the import as `"disabled": true` rather
  than being dropped: "off right now" is intent, not absence. Disabled
  *variables* are the exception, since they are not live config.
- Body modes other than `raw` (formdata, urlencoded, file) are recorded as a
  mode and reported, not silently converted. Guessing at a multipart body would
  be worse than saying so.
- A whole collection used to be one `collection.json`, which put every
  unrelated edit in the same file. It is now one YAML file per request, and
  `NN-` prefixes carry the order. The cost is that inserting at the top of a
  folder renames every file below it; git shows those as renames, and a
  conflict-free merge is worth more than a quiet diff.
- Collection and folder `auth` are written to `collection.yml` / `folder.yml`
  and inherited downwards; a request only carries `auth` when it overrides the
  chain, and `auth: none` opts out. Resolving inheritance at import time instead
  would put a copy of the token scheme in every request file, so a later change
  would be a diff in every file — the merge churn one-file-per-request exists to
  avoid.
- `rq import` refuses to write over an existing collection directory rather
  than merging into it, because stale request files from a previous import would
  load as extra requests.
- `send` keeps original response bytes in Rust and returns metadata. The frontend
  requests display text separately: a query projection, the complete pretty-printed
  JSON when the query is empty, or a capped non-JSON text view. The JSON viewer
  parses that display text in the webview; virtualization limits rendered rows,
  not the amount of JSON transferred or parsed.
- The last opened workspace is stored in the per-user config directory
  (`app_config_dir()`, which is `%APPDATA%` on Windows, `Application Support` on
  macOS, `$XDG_CONFIG_HOME` on Linux) rather than in webview storage: clearing the
  webview's data should not forget it. A remembered path that no longer contains
  `collections/` is ignored instead of failing at launch.
- The folder picker is native and invoked from Rust, so the frontend needs no npm
  packages and no bundler at all — `ui/` is plain HTML/CSS/JS, served from the
  binary.

## Secrets

`rq import` and the desktop's workspace saves enforce the split between committed
configuration and local secret values:

```
collections/acme-orders/collection.yml   committed: name and `secret: true`, no value
environments/dev.yml                     committed: same, per environment
.secrets/acme-orders.env                 gitignored: apiToken=tok_live_9f8e7d6c5b4a
.secrets/dev.env                         gitignored: adminKey=adm_dev_7788
secrets.example.env                      committed: names only, grouped by scope
.gitignore                               gets /.secrets/ if it is not already there
.gitattributes                           gets `* text=auto eol=lf`
```

`.gitattributes` is there because one file per request is many small files: with
CRLF checkout, every save is a whole-file diff for anyone on Windows.

A secret is a Postman variable with `"type": "secret"`, or any name passed as
`--secret`; there is deliberately no name-based guessing, because guessing wrong
in the other direction writes a token into the repository.

Two guards. The first is tested by asserting the generated file has no value in
it; the second by planting a value in a header and watching `write` refuse. The
ignore rule was checked by running `git init && git add -A` over an imported
workspace and confirming `.secrets/` never appears:

- `render` is the only function that serialises, and it blanks every secret value
  before writing workspace YAML. There is no second workspace serializer to audit.
- It then scans every committed file for every secret value and records a leak;
  `write` refuses to touch the disk if one is found. The scan is the safety net
  for a future importer, not a substitute for the first rule.

Every export path (cURL, copy-as-code, share) uses the shared renderer and masks
secret variables by default. The explicit **Copy with secrets…** action is the
opt-in exception: it uses the same renderer with resolved values and writes
directly to the native clipboard, never returning secret-bearing cURL text to
the webview or storing it in workspace files.

These guards protect workspace configuration, not captured response data. Desktop
history stores response bodies and resolved URLs locally outside the workspace;
use its storage limit and clear controls when working with sensitive APIs.

## Certificates

Verification is on by default. Internal endpoints are often signed by a CA the
public trust store does not know, so an environment can opt out:

```yaml
# environments/dev.yml
name: dev
insecure: true          # skip certificate verification
variables:
  - name: token
    secret: true
```

It is per-environment, not a global setting, so a flag set while pointed at the
dev cluster cannot follow you to prod. `rq send --insecure` overrides it for one
run, and both warn on stderr. The window shows `TLS not verified` beside the
status code whenever it is in effect.

This is `curl -k`: the connection stays encrypted, but nothing proves who is on
the other end — which is precisely the guarantee protecting the tokens in
`.secrets/` from a hostile network. That is why it is visible rather than silent,
and why it lives next to the target instead of in a global preference.

## Not built yet

- click-to-pick query construction from response fields (the viewer already
  supports navigation and search)
- declarative captures (scripts can already capture response values into variables)
- full Postman sandbox/test compatibility, event-script import, and CLI script execution
- creating, deleting, and drag-reordering requests and folders in the tree
