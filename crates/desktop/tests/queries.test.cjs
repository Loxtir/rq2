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
    for (const name of ["hideVariable", "paintVariables", "renderParams", "renderHeaders",
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

test("Save to file sends only the response ID and current query to Rust", async () => {
  const h = harness(true, () => true);
  h.state.response = { id: 7, environment: "Dev" };
  h.element("query").value = "{ id title }";
  await h.context.saveResponse();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].command, "save_response");
  assert.deepEqual(Object.keys(h.calls[0].args).sort(), ["id", "query"]);
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
