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
    await boot();
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
    collection.variables = [{ name: "myVar", value: "2762467" }];
    const colored = '{ payload {\n items[? definition { item_id { value == "{{myVar}}" } }] { definition { item_id attributes } }\n} }';
    $("query").value = colored;
    queryEdited();
    const layer = $("query").parentElement.querySelector(".varlayer");
    check(layer && layer.textContent === colored, "query highlighting must preserve every source character");
    check($("query").parentElement.classList.contains("syntax-editor"), "query must use the native-textarea syntax overlay");
    check(layer.querySelector('[data-name="myVar"]')?.textContent === "{{myVar}}", "query variables must use the shared hover tokens");
    $("query").focus();
    $("query").setSelectionRange(0, 0);
    paintVariables($("query"));
    const pair = layer.querySelectorAll(".query-match");
    check(pair.length === 2, "the caret's matching braces must be underlined");
    check(getComputedStyle(pair[0]).color === getComputedStyle(pair[1]).color, "matching braces must have the same depth color");
    check(getComputedStyle(pair[0]).textDecorationLine === "underline", "matching braces need a visible underline");
    const editorStyle = getComputedStyle($("query"));
    const layerStyle = getComputedStyle(layer);
    for (const property of ["fontFamily", "fontSize", "lineHeight", "paddingTop", "paddingLeft", "tabSize"]) {
      check(editorStyle[property] === layerStyle[property], `query overlay must align ${property}`);
    }
    showVariable("myVar", $("query").getBoundingClientRect());
    check($("var-value").value === "2762467", "query variables must share the existing variable card");
    check(!$("var-save"), "automatic variable editing must not require an Apply button");
    check($("var-cancel").textContent === "Close", "closing the variable editor must not imply cancelling applied edits");
    const originalSaveVariable = saveVariable;
    let automaticEdits = 0;
    saveVariable = async () => { automaticEdits++; };
    $("var-value").value = "2762468";
    $("var-value").dispatchEvent(new Event("input", { bubbles: true }));
    saveVariable = originalSaveVariable;
    check(automaticEdits === 1, "typing a variable value must apply it automatically without a click or blur");
    check(!$("var-popover").hidden, "typing must leave the variable editor open");
    hideVariable();
    $("query").value = '{\n' + '  id\n'.repeat(30) + '}';
    queryEdited();
    await new Promise((resolve) => setTimeout(resolve, 50));
    $("query").scrollTop = 90;
    $("query").dispatchEvent(new Event("scroll"));
    check(layer.scrollTop === $("query").scrollTop && layer.scrollTop > 0, "query highlighting must follow textarea scrolling");
    $("query").value = '{ html == "<img src=x onerror=alert(1)>" }';
    queryEdited();
    check(!layer.querySelector("img"), "query highlighting must never interpret HTML");
    $("query-name").value = "Tree";
    $("query").value = "{ children** { id } }";
    await storeQuery();
    check(state.selection.node.queries.Tree === $("query").value, "Keep query did not store the query");
    check($("save").disabled, "keeping a query must not dirty the request");
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
    collection.service_name = "navsvc";
    collection.deployments = [
      { tenant: "acme", platform: "stage-a", slot: "candidate" },
      { tenant: "globex", platform: "stage-b", slot: "stable" },
    ];
    state.workspace.environments[1].platforms = [
      { name: "stage-a", url_template: "https://{{serviceName}}-{{slot}}.stage-a.test/{{tenant}}" },
      { name: "stage-b", url_template: "https://{{tenant}}.stage-b.test/{{serviceName}}" },
    ];
    openRequest(node, collection, [0], true);
    check(!$("deployment-bar").hidden, "deployment controls must show for a service collection");
    check($("send").disabled, "sending must wait for an explicit deployment selection");
    $("deployment-tenant").value = "acme";
    changeDeployment(true);
    $("deployment-target").value = JSON.stringify(collection.deployments[0]);
    changeDeployment();
    check(!$("send").disabled, "a configured deployment must enable sending");
    check($("deployment-destination").textContent === "https://navsvc-candidate.stage-a.test/acme", "resolved deployment destination must be visible");
    for (const id of ["deployment-tenant", "deployment-target"]) {
      const style = getComputedStyle($(id));
      check(style.backgroundColor === themed.backgroundColor && style.colorScheme === "dark", `${id} must use dark controls and native popups`);
    }
    const deploymentBar = $("deployment-bar");
    deploymentBar.style.width = "340px";
    await new Promise((resolve) => setTimeout(resolve, 50));
    check(deploymentBar.scrollWidth <= deploymentBar.clientWidth + 1, "deployment controls must wrap without overflow");
    collection.base_path = "/{{tenant == 'acme' ? serviceName : 'shared'}}-{{slot}}";
    state.workspace.environments[1].platforms[0].url_template = "https://{{serviceName}}.stage-a.test{{basePath}}";
    renderDeployments();
    updateSendButton();
    check($("send").disabled, "conditional base paths must wait for backend resolution");
    await refreshPreview();
    check(!$("send").disabled, "a backend-resolved base path must enable sending");
    check($("deployment-destination").textContent === "https://navsvc.stage-a.test/navsvc-candidate", "the destination must use the backend-resolved base path");
    showVariable("basePath", $("url").getBoundingClientRect());
    check($("var-value").value === "/navsvc-candidate" && $("var-value").readOnly, "generated basePath must be visible and read-only");
    check($("var-value").readOnly, "generated basePath must not allow automatic variable edits");
    hideVariable();
    const original = state.requestTabs.find((tab) => tab.key === state.activeRequestTab);
    const originalResponse = state.response;
    const evaluations = window.calls.filter((call) => call.command === "project_body").length;
    const activeTab = [...document.querySelectorAll(".request-tab")].find((tab) => tab.getAttribute("aria-selected") === "true");
    activeTab.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 40 }));
    check(!$("context-menu").hidden, "right-clicking a request tab must open its menu");
    [...$("context-menu").querySelectorAll("button")].find((item) => item.textContent === "Duplicate tab").click();
    check($("context-menu").hidden, "choosing an item must close the menu");
    const duplicate = state.requestTabs.find((tab) => tab.key === state.activeRequestTab);
    check(duplicate.key !== original.key && duplicate.slug === original.slug, "Duplicate tab must open the same saved request with an independent identity");
    check(state.response === null && duplicate.view === null, "a duplicated draft must not pretend to have sent another response");
    state.draft.url = "https://different-store.test";
    activateRequestTab(original.key);
    check(state.response === originalResponse, "returning to the original must restore its own response");
    check(state.draft.url !== "https://different-store.test", "duplicate drafts must not share mutable request fields");
    check(window.calls.filter((call) => call.command === "project_body").length === evaluations, "switching back must not reevaluate the response query");
    check(document.querySelectorAll(".request-tab").length >= 2, "both copies must be visible in the tab strip");
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
      if (command === "initial_workspace") return null;
      if (command === "curl_preview" && args.deployment) {
        const collection = state.workspace.collections.find((item) => item.slug === args.collection);
        if (collection.base_path != null) {
          const basePath = "/navsvc-" + args.deployment.slot;
          return { text: "curl https://navsvc.stage-a.test" + basePath, masked: false, deployment: {
            ...args.deployment, service_name: collection.service_name, environment: args.environment,
            base_path: basePath, base_url: "https://navsvc.stage-a.test" + basePath,
          } };
        }
      }
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
