const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

class Element {
  constructor() {
    this.children = [];
    this.value = "";
    this.textContent = "";
    this.hidden = false;
    this.disabled = false;
    this.classList = { add() {}, remove() {}, toggle() {} };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
}

function harness(invoke) {
  const elements = new Map();
  const storage = new Map();
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const calls = [];
  const context = vm.createContext({
    window: { confirm: () => true, __TAURI__: { core: { invoke: async (command, args) => {
      calls.push({ command, args });
      if (invoke) return invoke(command, args);
      return command === "refresh_workspace" ? state.workspace : { text: "curl https://target.test", masked: false };
    } } } },
    document: { getElementById: element, createElement: () => new Element(), querySelectorAll: () => [] },
    localStorage: { getItem: (key) => storage.get(key) || null, setItem: (key, value) => storage.set(key, value), removeItem: (key) => storage.delete(key) },
    setTimeout: () => 1, clearTimeout() {},
  });
  const source = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  vm.runInContext(source.replace(/\nboot\(\);\s*$/, "\n"), context);
  vm.runInContext(`
    for (const name of ["hideVariable", "repaintVariables", "renderCurlPreview", "renderSummary",
      "resetResponse", "showResponseMetadata", "applyQuery", "renderWorkspace"]) globalThis[name] = () => {};
  `, context);
  const state = vm.runInContext("state", context);
  const placements = [
    { tenant: "acme", platform: "stage-a", slot: "stable" },
    { tenant: "acme", platform: "stage-a", slot: "candidate" },
    { tenant: "globex", platform: "stage-b", slot: "stable" },
    { tenant: "acme", platform: "prod-a", slot: "stable" },
  ];
  const collection = { slug: "navigation", name: "Navigation", service_name: "navsvc", deployments: placements, items: [] };
  const ordinary = { slug: "ordinary", name: "Ordinary", items: [] };
  state.workspace = { path: "test", collections: [collection, ordinary], environments: [
    { name: "stage", platforms: [
      { name: "stage-a", url_template: "https://{{serviceName}}-{{slot}}.stage-a.test/{{tenant}}" },
      { name: "stage-b", url_template: "https://{{tenant}}.stage-b.test/{{serviceName}}/{{slot}}" },
    ] },
    { name: "production", platforms: [{ name: "prod-a", url_template: "https://{{serviceName}}.prod.test/{{tenant}}/{{slot}}" }] },
  ] };
  state.selection = { kind: "request", slug: "navigation", collection, path: [0] };
  state.activeRequestTab = "navigation:0";
  state.draft = { name: "Menu", method: "GET", url: "{{baseUrl}}/menu", params: [], path_params: {}, headers: [], auth: "inherit" };
  const tenant = (name) => { element("deployment-tenant").value = name; context.changeDeployment(true); };
  const target = (index) => { element("deployment-target").value = JSON.stringify(placements[index]); context.changeDeployment(); };
  return { context, state, element, calls, storage, placements, collection, tenant, target };
}

test("deployment choices are filtered by environment and tenant, with no automatic fallback", async () => {
  const h = harness();
  h.context.renderDeployments();
  h.context.updateSendButton();
  assert.equal(h.element("deployment-bar").hidden, false);
  assert.equal(h.element("send").disabled, true);
  assert.equal(h.element("deployment-tenant").disabled, true);
  h.state.env = "stage";
  h.context.renderDeployments();
  assert.deepEqual(h.element("deployment-tenant").children.map((option) => option.value), ["", "globex", "acme"]);
  h.tenant("acme");
  assert.deepEqual(h.element("deployment-target").children.map((option) => option.textContent), ["Select deployment…", "stage-a / stable", "stage-a / candidate"]);
  assert.equal(h.element("send").disabled, true);
  h.target(1);
  assert.equal(h.element("send").disabled, false);
  assert.equal(h.element("deployment-destination").textContent, "https://navsvc-candidate.stage-a.test/acme");
  h.tenant("globex");
  assert.equal(h.context.currentDeployment(), null);
  assert.equal(h.element("send").disabled, true);
  assert.deepEqual(h.element("deployment-target").children.map((option) => option.textContent), ["Select deployment…", "stage-b / stable"]);
  h.target(2);
  h.state.response = { id: 8, environment: "stage", deployment: { ...h.placements[2], service_name: "navsvc" } };
  h.element("environment").value = "production";
  h.context.changeEnvironment();
  assert.equal(h.context.currentDeployment(), null);
  assert.equal(h.element("send").disabled, true);
  assert.equal(h.state.response.id, 8);
  assert.match(h.element("response-environment").textContent, /globex.*stage-b/);
  assert.equal(h.element("response-environment").className, "tag tag-warn");
});

test("target preferences are isolated per collection and restored from local storage", () => {
  const h = harness();
  h.state.env = "stage";
  h.tenant("acme");
  h.target(1);
  h.state.selection.slug = "ordinary";
  h.context.renderDeployments();
  h.context.updateSendButton();
  assert.equal(h.element("deployment-bar").hidden, true);
  assert.equal(h.element("send").disabled, false);
  h.state.selection.slug = "navigation";
  h.state.deploymentSelections.clear();
  h.context.renderDeployments();
  assert.equal(h.context.currentDeployment().slot, "candidate");
});

test("generated URL and template inputs are read-only rather than editable variable scopes", () => {
  const h = harness();
  h.state.env = "stage";
  h.tenant("acme");
  h.target(0);
  for (const name of ["baseUrl", "serviceName", "tenant", "platform", "slot"]) {
    const info = h.context.variableInfo(name);
    assert.equal(info.readOnly, true);
    assert.equal(info.kind, "deployment");
    assert.equal(info.defined, true);
  }
  assert.equal(h.context.variableInfo("baseUrl").value, "https://navsvc-stable.stage-a.test/acme");
});

test("preview, protected copy, unprotected copy and Send pass the same deployment", async () => {
  const h = harness();
  h.state.env = "stage";
  h.tenant("acme");
  h.target(1);
  h.calls.length = 0;
  await h.context.refreshPreview();
  await h.context.copyCurl(false);
  await h.context.copyCurl(true);
  await h.context.send();
  for (const command of ["curl_preview", "copy_curl", "send"]) {
    const calls = h.calls.filter((call) => call.command === command);
    assert.ok(calls.length, command);
    for (const call of calls) {
      assert.equal(call.args.environment, "stage");
      assert.deepEqual(JSON.parse(JSON.stringify(call.args.deployment)), h.placements[1]);
    }
  }
});

test("Send without a selected deployment makes no IPC call and retains the response", async () => {
  const h = harness();
  h.state.env = "stage";
  h.state.response = { id: 9 };
  await h.context.send();
  assert.equal(h.calls.length, 0);
  assert.equal(h.state.response.id, 9);
});

test("a slow preview for the previous deployment cannot replace the current command", async () => {
  const pending = [];
  const h = harness((command) => {
    assert.equal(command, "curl_preview");
    return new Promise((resolve) => pending.push(resolve));
  });
  h.state.env = "stage";
  h.tenant("acme");
  h.target(0);
  h.target(1);
  pending[2]({ text: "candidate command", masked: false });
  await Promise.resolve();
  await Promise.resolve();
  pending[1]({ text: "old stable command", masked: false });
  pending[0]({ text: "old incomplete target", masked: false });
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(h.state.preview, "candidate command");
});

test("an edited or removed target invalidates the remembered selection", () => {
  const h = harness();
  h.state.env = "stage";
  h.tenant("acme");
  h.target(1);
  h.collection.deployments.splice(1, 1);
  h.context.renderDeployments();
  h.context.updateSendButton();
  assert.equal(h.context.currentDeployment(), null);
  assert.equal(h.element("send").disabled, true);
});

test("response provenance distinguishes targets and changed URL templates without discarding the body", () => {
  const h = harness();
  h.state.env = "stage";
  h.tenant("acme");
  h.target(0);
  h.state.response = { id: 8, environment: "stage", deployment: {
    ...h.placements[0], service_name: "navsvc", environment: "stage",
    base_url: h.context.deploymentInputs().baseUrl,
  } };
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").className, "tag");
  h.target(1);
  assert.equal(h.element("response-environment").className, "tag tag-warn");
  h.target(0);
  h.state.workspace.environments[0].platforms[0].url_template = "https://new.test/{{serviceName}}/{{tenant}}/{{slot}}";
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").className, "tag tag-warn");
  assert.equal(h.state.response.id, 8);
});

test("older responses without deployment provenance are labeled unknown", () => {
  const h = harness();
  h.state.env = "stage";
  h.state.response = { id: 9, environment: "stage" };
  h.context.renderResponseEnvironment();
  assert.match(h.element("response-environment").textContent, /deployment unknown/);
  assert.equal(h.state.response.id, 9);
});

test("conditional base paths use Rust's preview metadata without evaluating expressions in JavaScript", async () => {
  const h = harness((command, args) => {
    if (command === "curl_preview") {
      const selected = args.deployment;
      if (!selected) return { text: "no target", masked: false };
      return { text: "resolved command", masked: false, deployment: {
        ...selected, service_name: "navsvc", environment: "stage",
        base_path: `/navsvc-${selected.slot}`,
        base_url: `https://navsvc.stage-a.test/navsvc-${selected.slot}`,
      } };
    }
    if (command === "refresh_workspace") return h.state.workspace;
    return { id: 8, environment: "stage" };
  });
  h.state.env = "stage";
  h.collection.base_path = "/{{tenant == 'acme' ? serviceName : 'shared'}}-{{slot}}";
  h.state.workspace.environments[0].platforms[0].url_template = "https://{{serviceName}}.stage-a.test{{basePath}}";
  h.tenant("acme");
  h.target(0);
  assert.equal(h.element("send").disabled, true, "wait for Rust resolution");
  assert.equal(h.element("deployment-destination").textContent, "Resolving base URL…");
  await h.context.refreshPreview();
  assert.equal(h.element("send").disabled, false);
  assert.equal(h.element("deployment-destination").textContent, "https://navsvc.stage-a.test/navsvc-stable");
  const info = h.context.variableInfo("basePath");
  assert.equal(info.value, "/navsvc-stable");
  assert.equal(info.readOnly, true);
  assert.equal(info.defined, true);
  await h.context.send();
  assert.equal(h.calls.filter((call) => call.command === "send").length, 1);
});

test("base path resolution errors disable sending and copying without discarding a response", async () => {
  const h = harness(() => { throw new Error("platform must include {{basePath}}"); });
  h.state.env = "stage";
  h.collection.base_path = "/catalog";
  h.state.response = { id: 8 };
  h.tenant("acme");
  h.target(0);
  await h.context.refreshPreview();
  assert.equal(h.element("send").disabled, true);
  assert.equal(h.element("copy-curl").disabled, true);
  assert.match(h.element("deployment-destination").textContent, /must include/);
  const before = h.calls.length;
  await h.context.send();
  assert.equal(h.calls.length, before);
  assert.equal(h.state.response.id, 8);
});

test("old resolution metadata cannot overwrite a newer slot or edited base path", async () => {
  const pending = [];
  const h = harness(() => new Promise((resolve) => pending.push(resolve)));
  h.state.env = "stage";
  h.collection.base_path = "/{{serviceName}}-{{slot}}";
  h.state.workspace.environments[0].platforms[0].url_template = "https://{{serviceName}}.stage-a.test{{basePath}}";
  h.tenant("acme");
  h.target(0);
  h.target(1);
  const preview = (slot) => ({ text: `${slot} command`, masked: false, deployment: {
    ...h.placements[slot === "stable" ? 0 : 1], service_name: "navsvc", environment: "stage",
    base_path: `/navsvc-${slot}`, base_url: `https://navsvc.stage-a.test/navsvc-${slot}`,
  } });
  pending[2](preview("candidate"));
  await Promise.resolve();
  await Promise.resolve();
  pending[1](preview("stable"));
  pending[0](preview("stable"));
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(h.context.deploymentInputs().basePath, "/navsvc-candidate");
  assert.equal(h.state.preview, "candidate command");
  h.collection.base_path = "/changed";
  h.context.renderDeployments();
  h.context.updateSendButton();
  assert.equal(h.context.deploymentInputs(), null);
  assert.equal(h.element("send").disabled, true);
});
