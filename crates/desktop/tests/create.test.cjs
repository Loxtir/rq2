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
  setAttribute() {}
  addEventListener() {}
  showModal() { this.open = true; }
  close() { this.open = false; }
  focus() {}
}

function harness(invoke) {
  const elements = new Map();
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const calls = [];
  const alerts = [];
  const context = vm.createContext({
    window: { confirm: () => true, alert: (text) => alerts.push(text), __TAURI__: { core: { invoke: async (command, args) => {
      calls.push({ command, args });
      return invoke(command, args);
    } } } },
    document: { getElementById: element, createElement: () => new Element(), querySelectorAll: () => [] },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    setTimeout: () => 1, clearTimeout() {},
  });
  const source = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  vm.runInContext(source.replace(/\nboot\(\);\s*$/, "\n"), context);
  const opened = [];
  context.renderWorkspace = () => {};
  context.openRequest = (node, collection, nodePath) => opened.push(["request", node.name, nodePath]);
  context.openFolder = (node, collection, nodePath) => opened.push(["folder", node.name, nodePath]);
  const state = vm.runInContext("state", context);
  const collection = { slug: "api", name: "API", items: [{ kind: "folder", name: "Users", items: [] }] };
  state.workspace = { path: "ws", collections: [collection], environments: [] };
  return { context, state, element, calls, alerts, opened, collection };
}

test("a request created from cURL is named after its URL, opened, and warnings are shown", async () => {
  const created = { kind: "request", name: "orders", method: "POST", url: "https://api.test/v1/orders" };
  const h = harness((command) => {
    assert.equal(command, "create_item");
    return {
      workspace: { path: "ws", environments: [], collections: [{ slug: "api", name: "API", items: [{ kind: "folder", name: "Users", items: [created] }] }] },
      path: [0, 0],
      warnings: ["`--insecure` is an environment setting here"],
    };
  });
  h.context.openCreateDialog({ kind: "item", collection: h.collection, parent: [0] });
  assert.equal(h.element("create-where").textContent, "In API / Users");
  assert.equal(h.element("create-curl-field").hidden, false);
  h.element("create-curl").value = "curl -k 'https://api.test/v1/orders?x=1' -d '{}'";
  await h.context.submitCreateDialog();

  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].args)), {
    collection: "api", parent: [0], name: "orders", folder: false,
    curl: "curl -k 'https://api.test/v1/orders?x=1' -d '{}'",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(h.opened)), [["request", "orders", [0, 0]]]);
  assert.equal(h.element("create-dialog").open, false);
  assert.match(h.alerts[0], /--insecure/);
});

test("folders and collections need a name and send no cURL", async () => {
  const h = harness((command, args) => {
    if (command === "create_collection") return { path: "ws", environments: [], collections: [] };
    return { workspace: { path: "ws", environments: [], collections: [{ slug: "api", name: "API", items: [{ kind: "folder", name: args.name, items: [] }] }] }, path: [0], warnings: [] };
  });

  h.context.openCreateDialog({ kind: "item", collection: h.collection, parent: [] });
  h.element("create-kind").value = "folder";
  h.context.syncCreateKind();
  assert.equal(h.element("create-curl-field").hidden, true);
  h.element("create-curl").value = "curl https://ignored.test/x";
  await h.context.submitCreateDialog();
  assert.equal(h.calls.length, 0, "a folder is not named from cURL");
  assert.match(h.element("create-note").textContent, /name is required/);

  h.element("create-name").value = "Admin";
  await h.context.submitCreateDialog();
  assert.equal(h.calls[0].args.folder, true);
  assert.equal(h.calls[0].args.curl, null);
  assert.deepEqual(JSON.parse(JSON.stringify(h.opened)), [["folder", "Admin", [0]]]);

  h.context.openCreateDialog({ kind: "collection" });
  assert.equal(h.element("create-kind-field").hidden, true);
  h.element("create-name").value = "Billing";
  await h.context.submitCreateDialog();
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[1])), { command: "create_collection", args: { name: "Billing" } });
});
