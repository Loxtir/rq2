# rq — response queries

A projection language for JSON API responses, plus the schema plumbing that
makes it safe to write.

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

| Syntax | Meaning |
| --- | --- |
| `id` | leaf, copied verbatim |
| `profile { a b }` | projected object |
| `orders { id }` | array, each element projected |
| `orders[:10] { id }` | slice; negative indices count from the end |
| `metadata` (no braces) | whole subtree, verbatim |
| `metadata { * }` | one level of keys, values verbatim |
| `owner: author { login }` | alias |
| `id!` | fail loudly if the response omits it |
| `items[id]` | one value per element, flattened into a list |
| `items[0:5][id]` | slice first, then collect |
| `children...{ id }` | walk a repeated key to any depth, keeping structure |
| `children...[id]` | walk it, then flatten to bare values |
| `# comment` | ignored |

Field names may contain `-`, `.`, `@`, `$` and unicode, so `content-type` and
`x-amz-date` work without quoting.

### Trees

A category tree is the case a fixed-depth projection gets wrong: you have to
guess the depth, and a deeper response silently truncates.

```
{ ids: children...[id] }
```

```json
{"ids": [2, 4, 7, 5, 3]}
```

Depth-first, document order. Leaves without the key are skipped, never errors,
and there is no depth limit in the query — so nothing is silently dropped.

The walked edge is consumed, never emitted. That is why `children...{ * }` is
safe: it gives you every node flattened, minus the nested tree, instead of
re-emitting each subtree at every level.

`children...[0:3][id]` bounds the *output* without bounding the traversal.

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
  src/main.rs            commands; response bodies never leave this process
  ui/                    the front end
crates/rq-cli/           thin driver binary
queries/                 saved queries, committed
examples/jsonplaceholder/  a workspace to open in the app
```

An imported workspace is one **YAML file per request**, because a git-synced
collection has to survive review and merge:

```
collections/acme-orders/collection.yml               name, description, auth, variables
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

The schema is an **internal abstraction** with providers, which is what makes
spec-less APIs first-class:

| Provider | Source | Works for |
| --- | --- | --- |
| OpenAPI | `--spec` | documented operations |
| Inferred | captured responses | everything else, after one call |
| `Any` | neither | accepts anything |

## The window

`cargo run -p rq-desktop -- examples/jsonplaceholder` opens a workspace from disk;
the last one is remembered. Three panes: the tree, the request, the command.

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
  The card shows the winning definition and wider definitions it shadows. Save
  edits the narrowest existing definition; for a new name, choose whether to
  create it in the collection, selected environment, or globals. Folder and
  request scopes are edited through the same card. A secret's value never
  crosses into the window, so its box starts empty and empty means "keep the
  stored value".
- The environment controls add, edit, rename, and delete environments, including
  their TLS verification setting and secret variables. **Globals** opens the
  workspace-wide fallback variable editor.
- Edits are drafts, marked with a dot in the tree, and they survive switching
  requests. **Save** is explicit: it re-renders in Rust and writes only the
  difference, so a secret file is never rewritten and a file the tool did not
  write is never deleted. The command pane shows the same request as a runnable
  cURL, with secrets masked to `***` and a warning when a `:path` value is
  missing.
- Collection, folder, and request scripts can run before a request or after its
  response. See the [scripts and response queries tutorial](docs/scripts-and-queries.md)
  for `pm.sendRequest`, variable scopes, and query examples.

## Commands

```powershell
cargo test

# project a response
cargo run -p rq-cli -- gen --lines 5000 > big.json
cargo run -p rq-cli -- project -q queries/orders/brief.query big.json --pretty

# check a query, with schema validation if you have a spec
cargo run -p rq-cli -- check -q queries/orders/brief.query
cargo run -p rq-cli -- check -q queries/orders/brief.query --spec openapi/service.json --operation getOrders

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

# the app: opens a workspace, sends, and projects locally per keystroke
cargo run -p rq-desktop -- examples/jsonplaceholder
```

`rq gen` is deterministic for a given seed, so benchmarks are comparable across
machines. Line counts are approximate; the exact number is printed to stderr.

## Status

Working today: parser, projector (slices, collect, recursive walks), OpenAPI
provider (JSON + YAML), inference, validation with suggestions, starter-query
generation, Postman v2.1 import with secret splitting, sending a stored request,
and the desktop window. 132 tests, `clippy -D warnings` clean.

The window is not a mockup: it reads the workspace from disk, sends through the
same core the CLI uses, and re-projects on every keystroke against a body held in
Rust — so reshaping a response costs nothing and touches the network once.

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
$sdk = (Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Directory |
        Sort-Object Name | Select-Object -Last 1).FullName
$env:Path = "$sdk\x64;$env:Path"
cargo run -p rq-desktop -- examples/jsonplaceholder
```

```powershell
cargo check --workspace --all-targets   # types, no linking
cargo test --workspace                  # 132 tests
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
- The response body never crosses into the webview. `send` keeps the bytes in the
  Rust process and returns metadata; the frontend asks for a projection and gets
  the projected text back. Holding to that now costs nothing and is the same
  boundary a 500k-line body needs later.
- The last opened workspace is stored in the per-user config directory
  (`app_config_dir()`, which is `%APPDATA%` on Windows, `Application Support` on
  macOS, `$XDG_CONFIG_HOME` on Linux) rather than in webview storage: clearing the
  webview's data should not forget it. A remembered path that no longer contains
  `collections/` is ignored instead of failing at launch.
- The folder picker is native and invoked from Rust, so the frontend needs no npm
  packages and no bundler at all — `ui/` is plain HTML/CSS/JS, served from the
  binary.

## Secrets

`rq import` already writes the layout, so the rule is enforced before there is
an app to forget it:

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
  before writing JSON. There is no second code path to audit.
- It then scans every committed file for every secret value and records a leak;
  `write` refuses to touch the disk if one is found. The scan is the safety net
  for a future importer, not a substitute for the first rule.

Every export path (cURL, copy-as-code, share) must resolve through one place that
renders `***`, because with a plain file that is the only guard.

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

- the response viewer (virtualized raw view) and click-to-pick; both have the RPC
  they need
- pre-request scripts (the sandbox), declarative captures
- creating, deleting, and drag-reordering requests and folders in the tree

