# Deployment selector demo

Open this directory as a workspace in rq, then select **Get menu** under
**Navigation Demo**.

1. Select the **stage** environment in the sidebar.
2. Select tenant **acme** in the sidebar, under the environment.
3. Choose **stage-a / stable** or **stage-a / candidate**.
4. Inspect the resolved destination and cURL preview. Hover `{{baseUrl}}` or
   `{{tenant}}` to see the read-only generated value.
5. Switch to **globex**: only **stage-b / stable** is offered.
6. Switch the environment to **production**: Acme uses **prod-a**, while Globex
   uses **prod-b**. Select a deployment again; rq never chooses a fallback.

These are fictional deployments. All hosts use the reserved `.example.test`
domain and are not intended to resolve. **Send will fail**, but the selectors,
generated URLs and cURL previews work without a server or credentials.

For example, stage / acme / stage-a / candidate resolves the request URL to:

```text
https://navigation-demo-candidate.stage-a.example.test/acme/navigation/menu
```

The environment files own platform URL templates, `collection.yml` identifies
the service, and `deployments.yml` contains only tenant/platform/slot placement.

## Main slots without a URL suffix

To designate a named slot such as `default` as the primary slot for every service,
set `main_slot: default` in each relevant environment YAML. Then use
`{{slotSuffix}}` in platform URL templates (or collection base paths) in place of
`-{{slot}}`. For example:

```yaml
name: stage
main_slot: default
platforms:
  - name: stage-a
    url_template: 'https://{{serviceName}}{{slotSuffix}}.stage-a.example.test/{{tenant}}{{slotSuffix}}'
```

A `default` deployment has no suffix at either position; a `worker` deployment
gets `-worker` at both positions. The deployment still has `slot: default` or
`slot: worker`; only URL formatting changes. This is environment-wide, not a
per-service convention. The demo's existing `stable`, `candidate`, and `stage`
placements remain unchanged.

## Conditional base path

Open **Catalog Demo → List categories**, select **stage**, and choose **north** or
**south**, then **stage-c / stage**. The hostname always contains `shopsvc`; only
the base path changes:

```text
north → https://shopsvc.north.stage-c.example.test/shopsvc-stage/catalogs/demo-catalog/categories
south → https://shopsvc.south.stage-c.example.test/legacysvc-stage/catalogs/demo-catalog/categories
```

The collection has a stable `service_name: shopsvc` and:

```yaml
base_path: '/{{tenant == "north" ? serviceName : "legacysvc"}}-{{slot}}'
```

Its platform template inserts the result with `{{basePath}}`. Expressions are
evaluated only in the collection's `base_path`, never in the request endpoint.
This example has no production deployments; switching to production disables
sending until a valid target is selected again.

From the repository root, you can also open it with:

```powershell
.\target\release\rq-desktop.exe examples/deployments
```
