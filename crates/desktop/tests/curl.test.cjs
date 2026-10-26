const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");
const { syntaxParts } = require("../ui/syntax.js");

class Element {
  constructor() { this.children = []; this.disabled = true; this.hidden = false; this._text = ""; }
  set textContent(value) { this._text = String(value); this.children = []; }
  get textContent() { return this._text + this.children.map((child) => child.textContent).join(""); }
  set innerHTML(_) { throw new Error("cURL must never be rendered as HTML"); }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this._text = ""; this.children = children; }
}

function harness(options = {}) {
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
        return options.invoke?.(command, args);
      } } },
      confirm: (message) => { confirmations.push(message); return options.confirm ?? false; },
    },
    document: { getElementById: element, createElement: () => new Element() },
    localStorage: { getItem: () => null },
    setTimeout: () => 1, clearTimeout: () => {}, syntaxParts,
  });
  const source = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  vm.runInContext(source.replace(/\nboot\(\);\s*$/, "\n"), context);
  vm.runInContext("renderSummary = () => {};", context);
  const state = vm.runInContext("state", context);
  state.selection = { kind: "request", slug: "export", path: [0] };
  state.draft = { name: "Draft", method: "POST", url: "{{baseUrl}}/draft", headers: [], params: [] };
  state.env = "Dev";
  state.preview = "curl 'https://dev.test/draft' -H 'Authorization: Bearer ***'";
  context.renderCurlPreview(state.preview);
  context.setCurlCopyEnabled(true);
  return { context, state, element, calls, confirmations };
}

test("cURL highlighting retains exact copy text and treats HTML as text", () => {
  const h = harness();
  const text = `curl -X POST 'https://dev.test' --data-raw '<img src=x onerror=alert(1)>'`;
  h.context.renderCurlPreview(text);
  const preview = h.element("curl");
  assert.equal(preview.textContent, text);
  assert.ok(preview.children.some((part) => part.className === "syntax-function"));
  assert.ok(preview.children.some((part) => part.className === "syntax-keyword"));
  assert.ok(preview.children.some((part) => part.className === "json-string"));
});

test("preview refresh never asks for secrets and enables both copy options", async () => {
  const text = "curl 'https://dev.test' -H 'Authorization: Bearer ***'";
  const h = harness({ invoke: () => ({ text, masked: true, problem: null }) });
  await h.context.refreshPreview();
  assert.equal(h.calls[0].command, "curl_preview");
  assert.equal(h.calls[0].args.includeSecrets, undefined);
  assert.equal(h.state.preview, text);
  assert.equal(h.element("curl").textContent, text);
  assert.equal(h.element("preview-masked").hidden, false);
  assert.equal(h.element("copy-curl").disabled, false);
  assert.equal(h.element("copy-curl-secrets").disabled, false);
});

test("protected copy uses current draft and defaults to excluding secret values", async () => {
  const h = harness();
  h.state.draft.url = "{{baseUrl}}/edited";
  await h.context.copyCurl();
  assert.equal(h.calls[0].command, "copy_curl");
  assert.equal(h.calls[0].args.includeSecrets, false);
  assert.equal(h.calls[0].args.draft.url, "{{baseUrl}}/edited");
  assert.equal(h.calls[0].args.environment, "Dev");
  assert.equal(h.confirmations.length, 0);
  assert.equal(h.element("copy-note").textContent, "copied protected");
});

test("secret copy requires confirmation and never puts returned secrets in the preview", async () => {
  const h = harness({ confirm: true, invoke: () => "secret-value-not-for-the-webview" });
  const before = h.state.preview;
  await h.context.copyCurl(true);
  assert.equal(h.confirmations.length, 1);
  assert.match(h.confirmations[0], /clipboard history or sync/);
  assert.equal(h.calls[0].args.includeSecrets, true);
  assert.equal(h.state.preview, before);
  assert.equal(h.element("curl").textContent, before);
  assert.equal(h.element("copy-note").textContent, "copied with secrets");
});

test("cancelling secret copy makes no clipboard command", async () => {
  const h = harness({ confirm: false });
  await h.context.copyCurl(true);
  assert.equal(h.calls.length, 0);
  assert.equal(h.element("copy-note").textContent, "");
});

test("clipboard errors are shown without claiming success or replacing the preview", async () => {
  const h = harness({ invoke: () => { throw new Error("Clipboard unavailable"); } });
  const before = h.state.preview;
  await h.context.copyCurl(false);
  assert.match(h.element("copy-note").textContent, /Clipboard unavailable/);
  assert.equal(h.state.preview, before);
  assert.equal(h.element("copy-curl").disabled, false);
  assert.equal(h.element("copy-curl-secrets").disabled, false);
});

test("both buttons are disabled while copying and stay disabled if selection changes", async () => {
  let finish;
  const h = harness({ invoke: () => new Promise((resolve) => { finish = resolve; }) });
  const pending = h.context.copyCurl(false);
  assert.equal(h.element("copy-curl").disabled, true);
  assert.equal(h.element("copy-curl-secrets").disabled, true);
  h.state.selection = null;
  finish();
  await pending;
  assert.equal(h.element("copy-curl").disabled, true);
  assert.equal(h.element("copy-curl-secrets").disabled, true);
});

test("empty selections and failed previews disable both copy actions", async () => {
  const h = harness({ invoke: () => { throw new Error("Request missing"); } });
  await h.context.refreshPreview();
  assert.equal(h.element("copy-curl").disabled, true);
  assert.equal(h.element("copy-curl-secrets").disabled, true);
  assert.equal(h.state.preview, "");
  h.state.selection = null;
  await h.context.refreshPreview();
  await h.context.copyCurl(true);
  assert.equal(h.calls.length, 1);
  assert.equal(h.confirmations.length, 0);
});
