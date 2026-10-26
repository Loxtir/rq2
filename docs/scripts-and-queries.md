# Pre-request scripts and response queries

This guide shows how to update a variable before a request is sent, then shape
the response locally with rq's query language. Scripts prepare requests;
queries project responses. They run at different points in the request flow.

## 1. Set up the milestone getter

In the selected environment, add a variable such as:

| Name | Value |
| --- | --- |
| `milestoneGetterUrl` | `https://api.example.test/current-milestone` |
| `baseUrl` | `https://api.example.test` |

The getter should return JSON similar to:

```json
{"milestone":"2026.10"}
```

## 2. Add a pre-request script

Open the collection, folder, or request and enter this in **Scripts →
Pre-request**:

```javascript
pm.sendRequest(pm.environment.get("milestoneGetterUrl"), (err, response) => {
  if (err) throw new Error(`Milestone lookup failed: ${err}`);

  const data = response.json();
  pm.globals.set("milestoneNOW", data.milestone);
});
```

The script waits for `pm.sendRequest` and its callback before sending the main
request. The response supports `json()`, `text()`, `code`, `status`,
`responseTime`, and `responseSize`. A request object is also accepted:

```javascript
const response = await pm.sendRequest({
  url: pm.environment.get("milestoneGetterUrl"),
  method: "GET",
});

pm.globals.set("milestoneNOW", response.json().milestone);
```

`pm.sendRequest` accepts a URL string or an object with `url` (including
`url.raw`), `method`, `header`/`headers`, and a raw body. It uses the selected
environment's TLS verification setting and variable substitution.

This is a Postman-style subset, not the complete Postman sandbox. Broader
Postman SDK objects and the full test/assertion APIs are not available.

## 3. Use the value in the main request

For example, set the request URL to:

```text
{{baseUrl}}/milestones/{{milestoneNOW}}/tasks
```

Script-set variables are applied before rq substitutes `{{variables}}` in the
URL, parameters, headers, authentication, and body. So the main request uses
the milestone returned by the getter on that send.

### Variable scopes

Scripts run from widest to narrowest scope: collection, enclosing folders from
outermost to innermost, then request. Pre-request scripts run before the main
request; post-response scripts run after it. If scripts set the same name,
later, narrower scopes win.

```javascript
pm.globals.set("milestoneNOW", value);
pm.environment.set("milestoneNOW", value);
pm.collectionVariables.set("milestoneNOW", value);
pm.variables.set("milestoneNOW", value); // current script's scope
```

To choose a scope explicitly from any script level:

```javascript
pm.variables.set("milestoneNOW", value, "global");
pm.variables.set("milestoneNOW", value, "environment");
pm.variables.set("milestoneNOW", value, "collection");
pm.variables.set("milestoneNOW", value, "folder");
pm.variables.set("milestoneNOW", value, "request");
```

Script writes update the actual variable at that scope and are saved as the
script runs. Ordinary values go to their environment, collection, folder, or
request YAML file; variables marked secret continue to use the ignored
`.secrets/` files. The editor refreshes after a request so it shows the saved
value. Getters read the currently effective value using normal scope
precedence.

## 4. Project the main response with a query

After sending the request, use the **Query** field above the response. For a
response such as:

```json
{
  "milestone": "2026.10",
  "data": {
    "total": 18,
    "items": [
      {"id": 1, "title": "First task", "internal": true},
      {"id": 2, "title": "Second task", "internal": false}
    ]
  },
  "debug": {"trace": "abc"}
}
```

Enter:

```text
{
  milestone
  data {
    total
    items[:5] { id title }
  }
}
```

The response view shows only the selected fields and the first five items. The
raw response remains available, and changing the query only changes the local
view; it does not send another request.

## Query language reference

Every query is one selection set starting with `{`.

| Query | Effect |
| --- | --- |
| `{ id title }` | Keep those fields; drop other object fields. |
| `{ user { id name } }` | Select nested object fields. |
| `{ items { id } }` | Apply the selection to each array item. |
| `{ itemIds: items[id] }` | Collect each item's `id` into a flat array; a missing `id` becomes `null`. |
| `{ items[:5] { id } }` | Keep the first five array entries, then project them. Negative bounds count from the end. |
| `{ * }` | Keep all keys at this object level and their values verbatim. |
| `{ id! }` | Require `id`; show an error if it is missing. |
| `{ owner: author { login } }` | Rename `author` to `owner` in the result. |
| `{ nodes: children...{ id name } }` | Walk recursively and flatten projected nodes into one list. |
| `{ ids: children...[id] }` | Walk recursively and collect each `id` into a flat list. |
| `{ children** { id name } }` | Keep the nested children tree, selecting `id` and `name` at every depth. |
| `{ * !debug }` | Keep everything except `debug` at this level. |
| `{ * !debug... }` | Keep everything except `debug` at any depth, including inside copied subtrees. |
| `{ * data { * !debug... } }` | Remove `debug` recursively only within `data`; keep sibling subtrees unchanged. |

A field without a nested selection keeps its whole value. For example, `{ data }`
copies all of `data`; `{ data { id } }` narrows it. The wildcard `*` fills in
unlisted keys at that object level. Required fields use `!`. Field names can
include hyphens, dots, `@`, `$`, and Unicode without quoting.

Slices come before collection brackets. For example,
`{ ids: items[:10][id] }` slices the first ten items, then collects their IDs.
Collection brackets select fields, so do not add a nested `{...}` to that same
field. Flattening recursive fields must specify either a nested selection or a
collection, such as `children...{ id }` or `children...[id]`. Tree-preserving
`children** { id }` automatically repeats the `children` edge, preserving missing,
empty, and null branches; it requires a selection and does not accept brackets.

Prefix `!field` excludes; suffix `field!` requires. Exclusions use original names,
before aliases, and override inclusions. Use `*` to keep other fields; exclusions
alone do not imply it. Dots are literal, so use nested selections to exclude a
particular path. Exclusions change only the view, not the stored response/history.

## Putting it together

1. Select the environment that defines `milestoneGetterUrl` and `baseUrl`.
2. Put the milestone lookup in a collection, folder, or request pre-request
   script.
3. Reference `{{milestoneNOW}}` in the main request URL or other request fields.
4. Send the request. The script refreshes the milestone first.
5. Enter a response query to keep the fields you want to inspect.

For more query examples, see the [query language overview in the README](../README.md).
