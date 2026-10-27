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

async function checkQueryControls() {
  const result = document.createElement("pre");
  result.id = "query-results";
  document.body.append(result);
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  try {
    const query = "{\n  # keep the comment on its own line\n  id\n  title\n}";
    const node = { kind: "request", name: "First", method: "GET", url: "https://api.test", queries: { Brief: query } };
    const collection = { slug: "queries", name: "Queries", items: [node] };
    state.workspace = { path: "test", collections: [collection], environments: [
      { name: "Dev", variables: [] }, { name: "Stage", variables: [] },
    ], globals: [] };
    openRequest(node, collection, [0]);
    check($("saved-query").disabled, "selector must wait for a response");
    const themed = getComputedStyle(document.querySelector(".sidebar-tools .select"));
    const scopeStyle = getComputedStyle($("var-create"));
    for (const property of ["backgroundColor", "color", "fontFamily", "fontSize", "borderRadius", "borderColor", "padding"]) {
      check(scopeStyle[property] === themed[property], `variable scope dropdown must use the app's ${property}`);
    }
    check(scopeStyle.colorScheme === "dark", "variable scope dropdown must keep native options dark");
    for (const id of ["saved-query", "query-name"]) {
      const style = getComputedStyle($(id));
      check(style.backgroundColor === themed.backgroundColor, `${id} must use the app's dark control background`);
      check(style.fontFamily === themed.fontFamily, `${id} must use the app's control font`);
      check(style.borderRadius === themed.borderRadius, `${id} must use the app's rounded control border`);
      check(style.colorScheme === "dark", `${id} must keep native control popups dark`);
    }
    check(getComputedStyle($("query")).resize === "none", "query editor must not show a native resize grip");
    let singleLineHeight;
    for (const value of ["", "{ id title }"]) {
      $("query").value = value;
      queryEdited();
      await new Promise((resolve) => setTimeout(resolve, 50));
      const editor = $("query").getBoundingClientRect();
      const style = getComputedStyle($("query"));
      singleLineHeight = parseFloat(style.lineHeight) + parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
      check(Math.abs(editor.height - singleLineHeight) <= 1, "empty and single-line queries must occupy only one line");
      for (const control of [document.querySelector(".query-bar .prompt"), $("apply-query"), $("suggest")]) {
        const bounds = control.getBoundingClientRect();
        check(Math.abs(editor.top + editor.height / 2 - bounds.top - bounds.height / 2) <= 1, "query text must be vertically centered with QUERY and its buttons");
      }
    }
    state.env = "Dev";
    state.response = { id: 7, status: 200, bytes: 20, elapsed_ms: 1, url: node.url, environment: "Dev" };
    showResponseMetadata(state.response);
    check(!$("saved-query").disabled, "selector must enable with a response");
    $("saved-query").value = "Brief";
    selectSavedQuery();
    check($("query").value === query, "multiline query or comment was changed by the editor");
    await new Promise((resolve) => setTimeout(resolve, 50));
    check($("query").getBoundingClientRect().height > singleLineHeight * 3, "multiline queries must expand the editor");
    check(window.calls.some((call) => call.command === "project_body" && call.args.query === query), "selection must apply the exact multiline query");
    $("query-name").value = "Tree";
    $("query").value = "{ children** { id } }";
    storeQuery();
    check(state.draft.queries.Tree === $("query").value, "Keep query did not update the draft");
    check(!$("save").disabled, "request Save must enable for query edits");
    const storedBody = $("response-body").textContent;
    $("environment").replaceChildren(new Option("Dev", "Dev"), new Option("Stage", "Stage"));
    $("environment").value = "Stage";
    changeEnvironment();
    check(state.response?.id === 7 && $("response-body").textContent === storedBody, "changing environments must not discard or block the response");
    check($("response-environment").textContent === "Dev" && $("response-environment").classList.contains("tag-warn"), "response badge must show the original environment in warning colors");
    check(!$("saved-query").disabled, "stored response queries must remain available after changing environments");
    for (const width of [340, 650]) {
      document.documentElement.style.setProperty("--left-w", "0px");
      document.documentElement.style.setProperty("--right-w", "0px");
      const main = document.querySelector(".saved-query-bar");
      main.style.width = `${width}px`;
      await new Promise((resolve) => setTimeout(resolve, 50));
      check(main.scrollWidth <= main.clientWidth + 1, `saved query controls overflow at ${width}px`);
      for (const control of main.children) check(control.getBoundingClientRect().height > 0, "query control is not visible");
      for (const control of main.children) check(Math.abs(control.getBoundingClientRect().height - 28) <= 1, "query controls must have matching compact heights");
    }
    result.textContent = JSON.stringify({ ok: true });
  } catch (error) {
    result.textContent = JSON.stringify({ error: error.message });
  }
}

test("saved-query controls preserve multiline queries and fit narrow panes in Edge", { skip: !edge }, () => {
  const tempRoot = path.join(process.env.TEMP || os.tmpdir(), "opencode");
  fs.mkdirSync(tempRoot, { recursive: true });
  const temp = fs.mkdtempSync(path.join(tempRoot, "rq-query-controls-"));
  try {
    const ui = path.resolve(__dirname, "../ui");
    const app = fs.readFileSync(path.join(ui, "app.js"), "utf8").replace(/\nboot\(\);\s*$/, "\n");
    const setup = `window.calls = []; window.__TAURI__ = { core: { invoke: async (command, args) => {
      window.calls.push({command, args});
      return command === "curl_preview" ? { text: "curl https://api.test", masked: false } : { text: '{"id":1}', bytes: 8, elapsed_ms: 0 };
    } } };`;
    const html = fs.readFileSync(path.join(ui, "index.html"), "utf8")
      .replace("<head>", `<head><base href="${pathToFileURL(ui + path.sep).href}">`)
      .replace(/<script src="[^"]+"><\/script>/g, "")
      .replace("</body>", `<script>${setup}</script><script>${fs.readFileSync(path.join(ui, "syntax.js"), "utf8")}</script><script>${app}</script><script>(${checkQueryControls.toString()})()</script></body>`);
    const fixture = path.join(temp, "queries.html");
    fs.writeFileSync(fixture, html);
    const output = execFileSync(edge, [
      "--headless", "--disable-gpu", "--no-first-run", `--user-data-dir=${path.join(temp, "profile")}`,
      "--window-size=1280,1000", "--dump-dom", "--virtual-time-budget=3000", pathToFileURL(fixture).href,
    ], { encoding: "utf8", timeout: 60000, maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    const match = output.match(/<pre id="query-results"[^>]*>([\s\S]*?)<\/pre>/);
    assert.ok(match, "browser did not report query control results");
    const results = JSON.parse(match[1]);
    assert.equal(results.error, undefined, results.error);
    assert.equal(results.ok, true);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
