# Deployment selector demo

Open this directory as a workspace in rq, then select **Get menu** under
**Navigation Demo**.

1. Select the **stage** environment in the sidebar.
2. Select tenant **acme** above the request URL.
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
