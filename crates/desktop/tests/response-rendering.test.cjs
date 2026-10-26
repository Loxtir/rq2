const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { pathToFileURL } = require("node:url");

const edge = [process.env["ProgramFiles(x86)"], process.env.ProgramFiles]
  .filter(Boolean)
  .map((root) => path.join(root, "Microsoft/Edge/Application/msedge.exe"))
  .find((binary) => fs.existsSync(binary));

async function checkResponseRendering() {
  const result = document.createElement("pre");
  result.id = "rendering-results";
  document.body.append(result);
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const frame = () => new Promise((resolve) => setTimeout(resolve, 50));
  const selectText = (node) => {
    const range = document.createRange();
    range.selectNodeContents(node);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    const text = selection.toString();
    selection.removeAllRanges();
    return text;
  };
  try {
    $("empty-state").hidden = true;
    $("request-view").hidden = false;
    const host = $("response-body");
    for (const selector of [".tab-body", "#response-body"]) {
      const corner = getComputedStyle(document.querySelector(selector), "::-webkit-scrollbar-corner");
      check(corner.backgroundColor === "rgba(0, 0, 0, 0)", `${selector}: scrollbar corner must blend with its pane rather than paint a black square`);
    }

    renderResponseText('{"items":[{"id":42,"title":"Example"}],"ok":true}');
    await frame();
    let tree = host.querySelector(".json-tree");
    check(tree, "small JSON must retain its collapsible tree");
    for (const line of tree.querySelectorAll(".json-summary, .json-row, .json-close")) {
      const gutter = getComputedStyle(line, "::before");
      check(gutter.content.includes("counter(response-line)"), "each JSON display line must have a line-number counter");
      check(gutter.pointerEvents === "none" && gutter.userSelect === "none", "line numbers must not intercept clicks or selection");
    }
    const copied = selectText(tree).replace(/\s/g, "");
    check(copied === tree.innerText.replace(/\s/g, ""), "copying JSON must not include gutter numbers");
    const nested = tree.querySelector(".json-children .json-object");
    nested.open = false;
    await frame();
    check(!nested.open && host.querySelector(".json-tree"), "line numbers must preserve native folding");

    for (const text of ["alpha\nbeta\n", "{\n  broken: [\n", "<img src=x onerror=alert(1)>\nsecond line"]) {
      renderResponseText(text);
      await frame();
      const content = host.querySelector(".response-text");
      check(content.dataset.lineNumbers === text.split("\n").map((_, index) => index + 1).join("\n"), "plain and incomplete JSON responses need one number per line");
      const copiedText = selectText(content);
      // Chromium omits a final unpainted newline from native selections.
      check(content.textContent === text && copiedText.replace(/\n$/, "") === text.replace(/\n$/, ""), "text copying must preserve the response without gutter numbers");
      check(!content.querySelector("img"), "response text must never be interpreted as HTML");
    }

    renderResponseText(JSON.stringify(Array.from({ length: 6000 }, (_, index) => index)));
    await frame();
    const model = host._jsonModel;
    check(model?.isVirtual, "large JSON must remain virtualized");
    tree = host.querySelector(".json-virtual-tree");
    check(tree.children.length < 200, "line numbering must not materialize thousands of rows");
    check(tree.firstElementChild.dataset.lineNumber === "1", "virtual numbering must start at one");
    host.scrollTop = 10000;
    renderVirtualWindow(model);
    check(Array.from(tree.children).some((row) => Number(row.dataset.lineNumber) > 100), "virtual numbering must follow the scroll window, not restart at one");
    for (const row of tree.children) {
      check(Number(row.dataset.lineNumber) === Math.round(parseFloat(row.style.top) / model.lineHeight) + 1, "virtual line numbers must match displayed row positions");
    }
    toggleJsonNode(model, model.value);
    check(tree.children.length === 1 && tree.firstElementChild.dataset.lineNumber === "1", "collapsed virtual JSON must retain correct numbering");
    toggleJsonNode(model, model.value);
    searchJsonModel(model, "5999");
    check(model.matches.length > 0 && tree.querySelector(".json-search-current"), "search must still navigate virtual JSON with line numbers");
    result.textContent = JSON.stringify({ ok: true });
  } catch (error) {
    result.textContent = JSON.stringify({ error: error.message });
  }
}

test("response line numbers preserve copying, folding, search, and virtualization; scrollbar corners blend with panes", { skip: !edge }, () => {
  const tempRoot = path.join(process.env.TEMP || os.tmpdir(), "opencode");
  fs.mkdirSync(tempRoot, { recursive: true });
  const temp = fs.mkdtempSync(path.join(tempRoot, "rq-response-rendering-"));
  try {
    const ui = path.resolve(__dirname, "../ui");
    const app = fs.readFileSync(path.join(ui, "app.js"), "utf8").replace(/\nboot\(\);\s*$/, "\n");
    const html = fs.readFileSync(path.join(ui, "index.html"), "utf8")
      .replace("<head>", `<head><base href="${pathToFileURL(ui + path.sep).href}">`)
      .replace(/<script src="[^"]+"><\/script>/g, "")
      .replace("</body>", `<script>window.__TAURI__ = { core: { invoke() {} } };</script><script>${app}</script><script>(${checkResponseRendering.toString()})()</script></body>`);
    const fixture = path.join(temp, "rendering.html");
    fs.writeFileSync(fixture, html);
    const output = execFileSync(edge, [
      "--headless", "--disable-gpu", "--no-first-run", `--user-data-dir=${path.join(temp, "profile")}`,
      "--window-size=1280,1000", "--dump-dom", "--virtual-time-budget=4000", pathToFileURL(fixture).href,
    ], { encoding: "utf8", timeout: 60000, maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    const match = output.match(/<pre id="rendering-results"[^>]*>([\s\S]*?)<\/pre>/);
    assert.ok(match, "browser did not report response rendering results");
    const results = JSON.parse(match[1]);
    assert.equal(results.error, undefined, results.error);
    assert.equal(results.ok, true);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
