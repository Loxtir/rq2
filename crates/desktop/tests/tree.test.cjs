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
    this.dataset = {};
    this.style = {};
    this.classList = { add() {}, remove() {}, toggle() {} };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute() {}
  addEventListener() {}
  querySelector() { return null; }
  querySelectorAll() { return []; }
  getBoundingClientRect() { return { left: 0, top: 0, width: 100, height: 20 }; }
  focus() {}
}

function harness(invoke) {
  const elements = new Map();
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const calls = [];
  const context = vm.createContext({
    window: { confirm: () => true, alert: (text) => { throw new Error(text); }, innerWidth: 1000, innerHeight: 800,
      __TAURI__: { core: { invoke: async (command, args) => { calls.push({ command, args }); return invoke(command, args); } } } },
    document: { getElementById: element, createElement: () => new Element(), createDocumentFragment: () => new Element(), querySelectorAll: () => [] },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
    setTimeout: () => 1, clearTimeout() {},
  });
  const source = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  vm.runInContext(source.replace(/\nboot\(\);\s*$/, "\n"), context);
  const shown = [];
  context.renderWorkspace = () => {};
  context.openRequest = (node, collection, nodePath, keep, key) => {
    context.state.activeTab = key;
    shown.push(["request", node.name, nodePath]);
  };
  context.openFolder = (node, collection, nodePath) => shown.push(["folder", node.name, nodePath]);
  context.state = vm.runInContext("state", context);
  return { context, state: context.state, calls, shown, drafts: vm.runInContext("drafts", context) };
}

const plain = (value) => JSON.parse(JSON.stringify(value));

test("paths shift the way the tree does when items are removed, inserted or moved", () => {
  const { context } = harness(() => null);
  const remove = context.removalMap([1]);
  assert.deepEqual(plain([[0], [1], [1, 0], [2], [2, 3]].map(remove)), [[0], null, null, [1], [1, 3]]);
  const insert = context.insertionMap([2, 0]);
  assert.deepEqual(plain([[2], [2, 0], [2, 1], [3, 0]].map(insert)), [[2], [2, 1], [2, 2], [3, 0]]);
  const move = context.moveMap([0], [1, 0]);
  assert.deepEqual(plain([[0], [1], [2], [2, 0]].map(move)), [[1, 0], [0], [1], [1, 1]]);
});

test("a drop is turned into a destination in the tree without the dragged item", () => {
  const { context } = harness(() => null);
  // Items a, b, c at the root. Dragging a below c: c is at 1 once a is out.
  assert.deepEqual(plain(context.moveTarget([0], [2], "after", 0)), { parent: [], index: 2 });
  assert.equal(context.moveTarget([1], [0], "after", 0), null, "b dropped just below a stays put");
  assert.equal(context.moveTarget([1], [1], "before", 0), null);
  // Into folder [3] holding 2 items; the folder is at 2 once a is out.
  assert.deepEqual(plain(context.moveTarget([0], [3], "into", 2)), { parent: [2], index: 2 });
  assert.equal(context.moveTarget([3], [3, 1], "before", 0), null, "not into itself");
});

test("deleting an item closes its tab and keeps the others on their own drafts", async () => {
  const items = [
    { kind: "request", name: "b", method: "GET", url: "", items: [] },
    { kind: "folder", name: "F", items: [{ kind: "request", name: "c", method: "GET", url: "", items: [] }] },
  ];
  const h = harness((command, args) => {
    assert.deepEqual(plain(args), { collection: "api", edit: { action: "delete", path: [0] } });
    return { workspace: { path: "ws", environments: [], collections: [{ slug: "api", name: "API", items }] }, collection: "api", path: null };
  });
  const tab = (key, tabPath) => ({ key, kind: "request", slug: "api", path: tabPath, name: key, draftKey: `request:api:${tabPath.join(".")}|${key}` });
  const tabs = [tab("A", [0]), tab("B", [1]), tab("C", [2, 0])];
  h.state.requestTabs = tabs;
  h.state.activeTab = "C";
  for (const item of tabs) h.drafts.set(item.draftKey, { draft: { name: item.key }, saved: "" });
  h.drafts.set("request:api:9", { draft: {}, saved: "" });

  await h.context.deleteNode({ slug: "api" }, [0], { kind: "request", name: "a" });

  assert.deepEqual(plain(h.state.requestTabs.map((item) => [item.key, item.path])), [["B", [0]], ["C", [1, 0]]]);
  assert.equal(h.drafts.get(h.state.requestTabs[1].draftKey).draft.name, "C");
  assert.equal(h.drafts.size, 2, "the closed tab's draft and an orphan draft are dropped");
  assert.deepEqual(plain(h.shown), [["request", "c", [1, 0]]], "the active tab follows its request");
});


test("ticking shared scripts keeps run order and unticking all leaves the draft clean", () => {
  const h = harness(() => null);
  h.state.workspace = { scripts: [{ name: "milestones", used_by: [] }, { name: "auth", used_by: [] }] };
  h.state.draft = { scripts: { pre_request: "", post_response: "" } };
  const before = JSON.stringify(h.state.draft);
  h.context.toggleInclude("auth", true);
  h.context.toggleInclude("milestones", true);
  assert.deepEqual(plain(h.state.draft.scripts.include), ["auth", "milestones"]);
  h.context.toggleInclude("auth", false);
  h.context.toggleInclude("milestones", false);
  assert.equal(JSON.stringify(h.state.draft), before);
});

test("renaming a shared script renames it in open drafts without making them dirty", () => {
  const h = harness(() => null);
  const draft = { scripts: { include: ["old", "other"] } };
  h.drafts.set("collection:api:", { draft, saved: JSON.stringify(draft) });
  h.state.draft = draft;
  h.context.renameIncludes("old", "new");
  const entry = h.drafts.get("collection:api:");
  assert.deepEqual(plain(entry.draft.scripts.include), ["new", "other"]);
  assert.equal(entry.saved, JSON.stringify(entry.draft));
  assert.equal(h.state.saved, entry.saved);
});