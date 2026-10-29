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
  close() {}
}

function harness(confirm = true, invoke) {
  const elements = new Map();
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const calls = [];
  const confirmations = [];
  const context = vm.createContext({
    window: {
      __TAURI__: { core: { invoke: async (command, args) => {
        calls.push({ command, args });
        if (invoke) return invoke(command, args);
        return { text: '{"id":1}', bytes: 8, elapsed_ms: 0 };
      } } },
      confirm: (message) => { confirmations.push(message); return confirm; },
    },
    document: { getElementById: element, createElement: () => new Element(), querySelectorAll: () => [] },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    setTimeout: () => 1, clearTimeout() {},
  });
  const source = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  vm.runInContext(source.replace(/\nboot\(\);\s*$/, "\n"), context);
  vm.runInContext(`
    for (const name of ["hideVariable", "withVariables", "paintVariables", "renderParams", "renderHeaders",
      "renderBody", "renderRequestAuth", "updateAuthTag", "markSelected", "renderRequestTabs",
      "updateSendButton", "refreshPreview", "updateMethodSelectStyle", "renderResponseText", "renderWorkspace", "repaintVariables"]) {
      globalThis[name] = () => {};
    }
    updateDirty = () => {};
  `, context);
  const state = vm.runInContext("state", context);
  const nodes = [
    { kind: "request", name: "First", method: "GET", url: "https://api.test/first", queries: { Brief: "{ id }" } },
    { kind: "request", name: "Second", method: "GET", url: "https://api.test/second", queries: { Titles: "{ title }" } },
  ];
  const collection = { slug: "queries", name: "Queries", items: nodes };
  state.workspace = { path: "test", collections: [collection] };
  const open = (index) => context.openRequest(nodes[index], collection, [index]);
  open(0);
  return { context, state, element, calls, confirmations, nodes, open };
}

test("saved queries load per request; selector waits for a response", () => {
  const h = harness();
  assert.deepEqual(h.element("saved-query").children.map((option) => option.textContent), ["Custom query", "Brief"]);
  assert.equal(h.element("saved-query").disabled, true);
  h.state.response = { id: 1, bytes: 20 };
  h.context.showResponseMetadata(h.state.response);
  assert.equal(h.element("saved-query").disabled, false);
  h.context.resetResponse("No response");
  assert.equal(h.element("saved-query").disabled, true);
});

test("selecting a query projects the current response without sending HTTP", async () => {
  const h = harness();
  h.state.response = { id: 7, bytes: 20 };
  h.element("saved-query").value = "Brief";
  h.context.selectSavedQuery();
  await Promise.resolve();
  assert.equal(h.element("query").value, "{ id }");
  assert.equal(h.element("query-name").value, "Brief");
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, "project_body");
  assert.equal(h.calls[0].args.id, 7);
  assert.equal(h.calls[0].args.query, "{ id }");
  assert.equal(h.state.requestTabs[0].query, "{ id }");
});

test("query variables stay as placeholders in saves and both projection IPC paths", async () => {
  const h = harness();
  h.state.env = "Stage";
  h.state.response = { id: 7, bytes: 20 };
  const query = '{ items[? definition { item_id { value == "{{myVar}}" } }] { definition { item_id attributes } } }';
  h.element("query").value = query;
  h.element("query-name").value = "By ID";
  h.context.storeQuery();
  assert.equal(h.context.draftForIpc().queries["By ID"], query);
  await h.context.applyQuery();
  await h.context.saveResponse();
  for (const command of ["project_body", "save_response"]) {
    const call = h.calls.find((call) => call.command === command);
    assert.equal(call.args.query, query);
    assert.deepEqual(JSON.parse(JSON.stringify(call.args.context)), {
      collection: "queries", path: [0], environment: "Stage", deployment: null,
    });
    assert.equal(Object.hasOwn(call.args, "variables"), false);
  }
});

test("live values are scoped overrides, not a highest-priority layer or saved defaults", () => {
  const h = harness();
  const collection = h.state.workspace.collections[0];
  collection.variables = [{ name: "id", value: "saved", has_value: true }];
  h.nodes[0].variables = [{ name: "id", value: "request", has_value: true }];
  h.state.workspace.live_variables = [{ scope: { kind: "collection", collection: "queries" },
    variable: { name: "id", value: "local", has_value: true } }];
  assert.equal(h.context.variableInfo("id").value, "request");
  h.open(1);
  assert.equal(h.context.variableInfo("id").value, "local");
  assert.equal(h.context.variableInfo("id").live, true);
  assert.equal(collection.variables[0].value, "saved");
  h.state.workspace.live_variables.push({ scope: { kind: "environment", environment: "Stage" },
    variable: { name: "envOnly", value: "stage-local", has_value: true } });
  h.state.workspace.environments = [{ name: "Stage", variables: [] }, { name: "Dev", variables: [] }];
  h.state.env = "Stage";
  assert.equal(h.context.variableInfo("envOnly").value, "stage-local");
  h.state.env = "Dev";
  assert.equal(h.context.variableInfo("envOnly").defined, false);
});

test("applying a live value leaves collection drafts and Save payloads unchanged and reapplies the query", async () => {
  let h;
  h = harness(true, (command, args) => {
    assert.equal(command, "set_variable");
    const workspace = JSON.parse(JSON.stringify(h.state.workspace));
    workspace.live_variables = [{ scope: { kind: "collection", collection: "queries" },
      variable: { name: args.edit.name, value: args.edit.value, has_value: true } }];
    return workspace;
  });
  h.state.workspace.collections[0].variables = [{ name: "id", value: "saved", has_value: true }];
  vm.runInContext(`varHoverName = "id";
    drafts.set("collection:queries:", { draft: { variables: [{ name: "id", value: "saved" }] }, saved: "unchanged" });
    applyQuery = () => { globalThis.queryApplied = true; };`, h.context);
  const before = vm.runInContext('JSON.stringify(drafts.get("collection:queries:"))', h.context);
  const request = JSON.stringify(h.context.draftForIpc());
  h.state.response = { id: 7 };
  h.element("var-value").value = "current";
  await h.context.saveVariable();
  assert.equal(h.context.variableInfo("id").value, "current");
  assert.equal(h.context.queryApplied, true);
  assert.equal(h.state.workspace.collections[0].variables[0].value, "saved");
  assert.equal(vm.runInContext('JSON.stringify(drafts.get("collection:queries:"))', h.context), before);
  assert.equal(JSON.stringify(h.context.draftForIpc()), request);
  assert.equal(h.calls[0].args.edit.reset, false);
});

test("reset clears a local-only value without writing a shared default", async () => {
  let h;
  h = harness(true, (_command, args) => {
    assert.equal(args.edit.reset, true);
    return { ...h.state.workspace, live_variables: [] };
  });
  h.state.workspace.live_variables = [{ scope: { kind: "collection", collection: "queries" },
    variable: { name: "new", value: "local", has_value: true } }];
  vm.runInContext('varHoverName = "new";', h.context);
  await h.context.saveVariable(true);
  assert.equal(h.context.variableInfo("new").defined, false);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, "set_variable");
});

test("secret live edits omit unchanged values and generated deployment inputs cannot be edited", async () => {
  let h;
  h = harness(true, (_command, args) => {
    assert.equal(args.edit.value, null);
    assert.equal(args.edit.secret, true);
    return h.state.workspace;
  });
  h.state.workspace.collections[0].variables = [{ name: "token", value: null, has_value: true, secret: true }];
  vm.runInContext('varHoverName = "token";', h.context);
  await h.context.saveVariable();
  assert.equal(h.calls.length, 1);
  h.state.workspace.collections[0].service_name = "demo";
  vm.runInContext('varHoverName = "tenant";', h.context);
  await h.context.saveVariable();
  assert.equal(h.calls.length, 1);
});

test("rapid automatic edits are serialized and keep the latest typed value without closing the editor", async () => {
  const pending = [];
  let h;
  h = harness(true, (command, args) => {
    assert.equal(command, "set_variable");
    return new Promise((resolve) => pending.push(() => resolve({ ...h.state.workspace,
      live_variables: [{ scope: { kind: "collection", collection: "queries" },
        variable: { name: "id", value: args.edit.value, has_value: true } }] })));
  });
  h.state.workspace.collections[0].variables = [{ name: "id", value: "saved", has_value: true }];
  vm.runInContext('varHoverName = "id"; hideVariable = () => { globalThis.editorClosed = true; };', h.context);
  h.element("var-value").value = "a";
  const first = h.context.saveVariable();
  h.element("var-value").value = "ab";
  const second = h.context.saveVariable();
  await new Promise(setImmediate);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].args.edit.value, "a");
  pending.shift()();
  await first;
  await new Promise(setImmediate);
  assert.equal(h.calls.length, 2);
  assert.equal(h.calls[1].args.edit.value, "ab");
  pending.shift()();
  await second;
  assert.equal(h.context.variableInfo("id").value, "ab");
  assert.equal(h.element("var-value").value, "ab");
  assert.equal(h.context.editorClosed, undefined);
});

test("query and response export wait for automatic variable writes, including newer typing", async () => {
  const pending = [];
  let h;
  h = harness(true, (command, args) => {
    if (command === "set_variable") return new Promise((resolve) => pending.push(() => resolve({ ...h.state.workspace,
      live_variables: [{ scope: { kind: "collection", collection: "queries" },
        variable: { name: "id", value: args.edit.value, has_value: true } }] })));
    if (command === "save_response") return true;
    return { text: '{"id":"ab"}', bytes: 11, elapsed_ms: 0 };
  });
  h.state.workspace.collections[0].variables = [{ name: "id", value: "saved", has_value: true }];
  vm.runInContext('varHoverName = "id";', h.context);
  h.element("var-value").value = "a";
  const first = h.context.saveVariable();
  h.state.response = { id: 7, bytes: 20 };
  h.element("query").value = '{ id == "{{id}}" }';
  const projected = h.context.applyQuery();
  const exported = h.context.saveResponse();
  h.element("var-value").value = "ab";
  const second = h.context.saveVariable();
  await new Promise(setImmediate);
  assert.deepEqual(h.calls.map((call) => call.command), ["set_variable"]);
  pending.shift()();
  await first;
  await new Promise(setImmediate);
  assert.deepEqual(h.calls.map((call) => call.command), ["set_variable", "set_variable"]);
  pending.shift()();
  await Promise.all([second, projected, exported]);
  assert.ok(h.calls.some((call) => call.command === "project_body"));
  assert.ok(h.calls.some((call) => call.command === "save_response"));
  assert.equal(h.context.variableInfo("id").value, "ab");
});

test("queued edits keep their original scope and cannot replace a different workspace", async () => {
  let finish;
  const h = harness(true, () => new Promise((resolve) => { finish = resolve; }));
  h.state.workspace.collections[0].variables = [{ name: "id", value: "saved", has_value: true }];
  vm.runInContext('varHoverName = "id";', h.context);
  h.element("var-value").value = "new";
  h.state.env = "Stage";
  const edit = h.context.saveVariable();
  h.state.selection.path = [1];
  h.state.env = "Dev";
  const workspace = { path: "other-workspace", collections: [], environments: [] };
  h.state.workspace = workspace;
  await new Promise(setImmediate);
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].args.path)), [0]);
  assert.equal(h.calls[0].args.environment, "Stage");
  finish({ path: "test", collections: [] });
  await edit;
  assert.equal(h.state.workspace, workspace);
});

test("Send and cURL copy wait for automatic edits before using the live value", async () => {
  let finish;
  let h;
  h = harness(true, (command) => {
    if (command === "set_variable") return new Promise((resolve) => { finish = () => resolve(h.state.workspace); });
    if (command === "send") throw new Error("stop after checking send order");
    return true;
  });
  h.state.workspace.collections[0].variables = [{ name: "id", value: "saved", has_value: true }];
  vm.runInContext('varHoverName = "id"; resetResponse = () => {}; showRequestError = () => {};', h.context);
  h.element("var-value").value = "current";
  const edit = h.context.saveVariable();
  h.state.preview = "curl https://api.test";
  const copied = h.context.copyCurl();
  const sent = h.context.send();
  await new Promise(setImmediate);
  assert.deepEqual(h.calls.map((call) => call.command), ["set_variable"]);
  finish();
  await Promise.all([edit, copied, sent]);
  assert.ok(h.calls.some((call) => call.command === "copy_curl"));
  assert.ok(h.calls.some((call) => call.command === "send"));
});

test("automatic typing can clear a value even before a new local definition finishes saving", async () => {
  const values = [];
  let h;
  h = harness(true, (_command, args) => {
    values.push(args.edit.value);
    return { ...h.state.workspace, live_variables: [{ scope: { kind: "collection", collection: "queries" },
      variable: { name: "new", value: args.edit.value, has_value: true } }] };
  });
  vm.runInContext('varHoverName = "new";', h.context);
  h.element("var-create").value = "collection";
  h.element("var-value").value = "a";
  const first = h.context.saveVariable();
  h.element("var-value").value = "";
  const second = h.context.saveVariable();
  await Promise.all([first, second]);
  assert.deepEqual(values, ["a", ""]);
  assert.equal(h.context.variableInfo("new").value, "");
  assert.equal(h.context.variableInfo("new").defined, true);
});

test("rapid query selection ignores stale errors from the previous projection", async () => {
  let rejectFirst;
  const h = harness(true, (_command, args) => {
    if (args.query === "{ id }") return new Promise((_resolve, reject) => { rejectFirst = reject; });
    return { text: '{"title":"Test"}', bytes: 16, elapsed_ms: 0 };
  });
  h.state.response = { id: 7, bytes: 20 };
  h.state.draft.queries.Titles = "{ title }";
  h.element("saved-query").value = "Brief";
  h.context.selectSavedQuery();
  h.element("saved-query").value = "Titles";
  h.context.selectSavedQuery();
  await Promise.resolve();
  rejectFirst(new Error("Old query failed"));
  await Promise.resolve();
  assert.equal(h.element("query-error").hidden, true);
  assert.equal(h.element("query").value, "{ title }");
});

test("named queries are staged in the request draft and included in Save IPC", () => {
  const h = harness();
  h.element("query").value = "{ title }";
  h.element("query-name").value = "  Titles  ";
  h.context.storeQuery();
  assert.equal(h.state.draft.queries.Titles, "{ title }");
  assert.equal(h.nodes[0].queries.Titles, undefined);
  assert.equal(h.context.isDirty(), true);
  assert.equal(h.context.draftForIpc().queries.Titles, "{ title }");
  assert.match(h.element("query-note").textContent, /Save the request/);
});

test("request Save reloads named queries from the returned workspace and preserves the response", async () => {
  const h = harness(true, (command, args) => {
    assert.equal(command, "save");
    const workspace = JSON.parse(JSON.stringify(h.state.workspace));
    workspace.collections[0].items[0].queries = args.edit.draft.queries;
    return { workspace, collection: "queries", written: 1, removed: 0 };
  });
  h.state.response = { id: 7, bytes: 20 };
  h.element("query").value = "{ title }";
  h.element("query-name").value = "Titles";
  h.context.storeQuery();
  await h.context.save();
  assert.equal(h.context.isDirty(), false);
  assert.equal(h.state.draft.queries.Titles, "{ title }");
  assert.equal(h.element("query").value, "{ title }");
  assert.equal(h.state.response.id, 7);
  assert.equal(h.element("saved-query").disabled, false);
  assert.match(h.element("save-note").textContent, /saved/);
});

test("failed saves leave query changes in the draft and report the error", async () => {
  const h = harness(true, () => { throw new Error("refusing to save: secret value found"); });
  h.element("query").value = "{ title }";
  h.element("query-name").value = "Titles";
  h.context.storeQuery();
  await h.context.save();
  assert.equal(h.context.isDirty(), true);
  assert.equal(h.state.draft.queries.Titles, "{ title }");
  assert.match(h.element("save-note").textContent, /refusing to save/);
});

test("switching requests preserves unsaved named queries and freeform query text", () => {
  const h = harness();
  h.element("query").value = "{ children** { id } }";
  h.element("query-name").value = "Tree";
  h.context.storeQuery();
  h.open(1);
  assert.equal(h.state.draft.queries.Tree, undefined);
  assert.equal(h.element("query").value, "");
  assert.equal(h.element("query-name").value, "");
  assert.deepEqual(h.element("saved-query").children.map((option) => option.textContent), ["Custom query", "Titles"]);
  h.open(0);
  assert.equal(h.state.draft.queries.Tree, "{ children** { id } }");
  assert.equal(h.element("query").value, "{ children** { id } }");
});

test("editing a selected query remains freeform until explicitly kept", () => {
  const h = harness();
  h.element("query").value = "{ title }";
  h.context.queryEdited();
  assert.equal(h.element("saved-query").value, "");
  assert.equal(h.element("remove-query").disabled, true);
  assert.equal(h.state.draft.queries.Brief, "{ id }");
  assert.equal(h.context.isDirty(), false);
});

test("overwriting a named query requires confirmation; cancelling keeps the original", () => {
  for (const confirmed of [false, true]) {
    const h = harness(confirmed);
    h.element("query").value = "{ title }";
    h.element("query-name").value = "Brief";
    h.context.storeQuery();
    assert.equal(h.confirmations.length, 1);
    assert.equal(h.state.draft.queries.Brief, confirmed ? "{ title }" : "{ id }");
  }
});

test("removing a query stages an empty map without discarding the displayed view", () => {
  const h = harness();
  h.element("query").value = "{ id }";
  h.element("saved-query").value = "Brief";
  h.context.removeSavedQuery();
  assert.equal(Object.keys(h.context.draftForIpc().queries).length, 0);
  assert.equal(h.element("query").value, "{ id }");
  assert.equal(h.context.isDirty(), true);
});

test("different names with identical query text keep the selected identity", () => {
  const h = harness();
  h.state.response = { id: 1, bytes: 20 };
  h.element("query").value = "{ id }";
  h.element("query-name").value = "Another brief";
  h.context.storeQuery();
  assert.equal(h.element("saved-query").value, "Another brief");
  h.context.removeSavedQuery();
  assert.equal(h.state.draft.queries["Another brief"], undefined);
  assert.equal(h.state.draft.queries.Brief, "{ id }");
});

test("blank names and blank queries are not stored; unusual names remain literal", () => {
  const h = harness();
  for (const [name, query] of [[" ", "{ id }"], ["Blank", " "]]) {
    h.element("query-name").value = name;
    h.element("query").value = query;
    h.context.storeQuery();
    assert.equal(Object.keys(h.state.draft.queries).length, 1);
  }
  for (const name of ["__proto__", "<img src=x>", "résumé"]) {
    h.element("query-name").value = name;
    h.element("query").value = "{ title }";
    h.context.storeQuery();
    assert.ok(Object.hasOwn(h.state.draft.queries, name));
    assert.ok(h.element("saved-query").children.some((option) => option.textContent === name));
  }
});

test("history tabs retain freeform projection but cannot change request queries", () => {
  const h = harness();
  h.state.requestTabs.push({ key: "history", kind: "history", response: { id: 3, bytes: 20 }, query: "" });
  h.context.activateRequestTab("history");
  assert.equal(h.element("saved-query-bar").hidden, true);
  h.element("query").value = "{ title }";
  h.element("query-name").value = "Nope";
  h.context.storeQuery();
  h.context.queryEdited();
  assert.equal(h.state.requestTabs.at(-1).query, "{ title }");
  assert.equal(h.nodes[0].queries.Nope, undefined);
});

test("changing environments preserves responses in every tab and keeps queries usable", async () => {
  const h = harness();
  const response = { id: 7, bytes: 20, environment: "Dev" };
  const otherResponse = { id: 8, bytes: 30, environment: "Stage" };
  h.state.env = "Dev";
  h.state.response = response;
  h.state.requestTabs[0].response = response;
  h.state.requestTabs.push({ key: "other", kind: "request", response: otherResponse });
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").textContent, "Dev");
  assert.equal(h.element("response-environment").className, "tag");
  h.element("response-body").textContent = "Stored response";
  h.element("environment").value = "Stage";
  h.context.changeEnvironment();
  assert.equal(h.state.response, response);
  assert.equal(h.state.requestTabs[0].response, response);
  assert.equal(h.state.requestTabs[1].response, otherResponse);
  assert.equal(h.element("response-body").textContent, "Stored response");
  assert.equal(h.element("response-environment").className, "tag tag-warn");
  assert.equal(h.element("response-environment").textContent, "Dev");
  assert.match(h.element("response-environment").title, /currently selected: Stage/);
  h.element("saved-query").value = "Brief";
  h.context.selectSavedQuery();
  await Promise.resolve();
  assert.equal(h.calls.at(-1).command, "project_body");
  assert.equal(h.calls.at(-1).args.id, 7);
  h.element("environment").value = "Dev";
  h.context.changeEnvironment();
  assert.equal(h.element("response-environment").className, "tag");
});

test("environment badge handles no environment, older history, and absent responses", () => {
  const h = harness();
  h.state.response = { id: 1, environment: "" };
  h.state.env = "Dev";
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").textContent, "no environment");
  assert.equal(h.element("response-environment").className, "tag tag-warn");
  h.state.env = "";
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").className, "tag");
  h.state.response = { id: 1 };
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").textContent, "environment unknown");
  assert.equal(h.element("response-environment").className, "tag");
  h.context.resetResponse("No response");
  assert.equal(h.element("response-environment").hidden, true);
});

test("editing or deleting an environment leaves the stored response intact", async () => {
  const h = harness(true, () => ({ path: "test", collections: [], environments: [] }));
  const response = { id: 7, environment: "Dev", bytes: 20 };
  h.state.response = response;
  h.state.env = "Dev";
  h.state.requestTabs[0].response = response;
  h.state.environmentEditor = { kind: "environment", oldName: "Dev", variables: [] };
  h.element("environment-name").value = "Renamed";
  await h.context.saveEnvironmentEditor();
  assert.equal(h.state.response, response);
  assert.equal(h.state.requestTabs[0].response, response);
  assert.equal(h.state.env, "Renamed");
  h.context.renderResponseEnvironment();
  assert.equal(h.element("response-environment").textContent, "Dev");
  assert.equal(h.element("response-environment").className, "tag tag-warn");
  h.state.environmentEditor = { kind: "environment", oldName: "Renamed" };
  await h.context.deleteSelectedEnvironment();
  assert.equal(h.state.response, response);
  assert.equal(h.state.requestTabs[0].response, response);
  assert.equal(h.state.env, "");
});

test("Save to file sends the response ID, query and scope identifiers, never response bytes or variable values", async () => {
  const h = harness(true, () => true);
  h.state.response = { id: 7, environment: "Dev" };
  h.element("query").value = "{ id title }";
  await h.context.saveResponse();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, "save_response");
  assert.deepEqual(Object.keys(h.calls[0].args).sort(), ["context", "id", "query"]);
  assert.equal(h.calls[0].args.id, 7);
  assert.equal(h.calls[0].args.query, "{ id title }");
  assert.equal(h.element("response-save-note").textContent, "Response saved.");
  assert.equal(h.element("response-save-note").hidden, false);
  assert.equal(h.element("save-response").disabled, false);
});

test("Save to file works with historical responses and an empty query", async () => {
  const h = harness(true, () => true);
  h.state.selection = null;
  h.state.response = { id: 8 };
  h.element("query").value = "";
  await h.context.saveResponse();
  assert.equal(h.calls[0].args.id, 8);
  assert.equal(h.calls[0].args.query, "");
});

test("cancelling Save to file is silent and errors are reported without changing the response", async () => {
  for (const fail of [false, true]) {
    const h = harness(true, () => {
      if (fail) throw new Error("Could not save response: access denied");
      return false;
    });
    const response = { id: 7 };
    h.state.response = response;
    await h.context.saveResponse();
    assert.equal(h.state.response, response);
    assert.equal(h.element("response-save-note").hidden, !fail);
    if (fail) assert.match(h.element("response-save-note").textContent, /access denied/);
    assert.equal(h.state.savingResponse, false);
  }
});

test("saving prevents duplicate dialogs and does not show a result on a different response", async () => {
  let finish;
  const h = harness(true, () => new Promise((resolve) => { finish = resolve; }));
  await h.context.saveResponse();
  assert.equal(h.calls.length, 0);
  h.state.response = { id: 7 };
  const pending = h.context.saveResponse();
  assert.equal(h.element("save-response").disabled, true);
  await h.context.saveResponse();
  assert.equal(h.calls.length, 1);
  h.state.response = { id: 8 };
  finish(true);
  await pending;
  assert.equal(h.element("response-save-note").hidden, true);
  assert.equal(h.element("save-response").disabled, false);
  h.context.resetResponse("No response");
  assert.equal(h.element("save-response").disabled, true);
});
