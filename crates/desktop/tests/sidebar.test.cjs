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

async function checkSidebar() {
  const result = document.createElement("pre");
  result.id = "sidebar-results";
  result.style.position = "fixed";
  document.body.append(result);
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const frame = () => new Promise((resolve) => setTimeout(resolve, 50));
  const collections = document.getElementById("collections-section");
  const history = document.getElementById("history-section");
  const tree = document.getElementById("tree");
  const historyList = document.getElementById("history-list");
  tree.innerHTML = Array.from({ length: 80 }, (_, i) => `<button class="request-row"><span class="method method-get">GET</span><span class="name">Request ${i}</span></button>`).join("");
  historyList.innerHTML = Array.from({ length: 80 }, (_, i) => `<button class="history-entry"><span class="method method-get">GET</span><span class="history-entry-name">Request ${i}</span><span class="history-entry-meta">200 · 12:30 · 22 MB</span></button>`).join("");
  let scenarios = 0;
  try {
    for (const height of [600, 900]) {
      for (const width of [240, 360]) {
        document.documentElement.style.height = `${height}px`;
        document.body.style.height = `${height}px`;
        document.documentElement.style.setProperty("--left-w", `${width}px`);
        for (const [collectionsOpen, historyOpen] of [[true, true], [true, false], [false, true], [false, false], [true, true]]) {
          collections.open = collectionsOpen;
          history.open = historyOpen;
          await frame();
          const label = `${height}px/${width}px open=${collectionsOpen}/${historyOpen}`;
          const a = collections.getBoundingClientRect();
          const b = history.getBoundingClientRect();
          const footer = document.querySelector(".sidebar-foot").getBoundingClientRect();
          check(a.bottom <= b.top + 1, `${label}: sections overlap`);
          check(b.bottom <= footer.top + 1 && footer.bottom <= height + 1, `${label}: footer overlaps`);
          for (const [section, list, open, controls] of [
            [collections, tree, collectionsOpen, document.querySelector(".tree-search")],
            [history, historyList, historyOpen, document.querySelector(".history-controls")],
          ]) {
            if (!open) {
              check(list.getClientRects().length === 0, `${label}: collapsed list remains visible`);
              continue;
            }
            const bounds = list.getBoundingClientRect();
            const sectionBounds = section.getBoundingClientRect();
            check(bounds.height > 0 && bounds.bottom <= sectionBounds.bottom + 1, `${label}: list overflows section (section=${sectionBounds.height}, list=${bounds.height}, bottom=${bounds.bottom}/${sectionBounds.bottom}, wrapper=${getComputedStyle(section, "::details-content").height})`);
            check(controls.getBoundingClientRect().bottom <= bounds.top + 1, `${label}: controls overlap list`);
            check(getComputedStyle(list).overflowY === "auto" && list.scrollHeight > list.clientHeight, `${label}: list cannot scroll`);
            list.scrollTop = 100;
            check(list.scrollTop > 0, `${label}: vertical scrolling failed`);
            list.scrollTop = 0;
          }
          if (collectionsOpen && historyOpen) {
            check(Math.abs(a.height - b.height) <= 12, `${label}: expanded sections do not share height`);
            tree.scrollTop = 100;
            check(historyList.scrollTop === 0, `${label}: list scrolling is not independent`);
          } else if (collectionsOpen || historyOpen) {
            const expanded = collectionsOpen ? a : b;
            check(expanded.height > height / 2, `${label}: expanded section does not fill available height`);
          }
          scenarios++;
        }
      }
    }
    result.textContent = JSON.stringify({ scenarios });
  } catch (error) {
    result.textContent = JSON.stringify({ scenarios, error: error.message });
  }
}

test("sidebar sections share height and scroll independently in Edge", { skip: !edge }, () => {
  const tempRoot = path.join(process.env.TEMP || os.tmpdir(), "opencode");
  fs.mkdirSync(tempRoot, { recursive: true });
  const temp = fs.mkdtempSync(path.join(tempRoot, "rq-sidebar-"));
  try {
    const ui = path.resolve(__dirname, "../ui");
    const html = fs.readFileSync(path.join(ui, "index.html"), "utf8")
      .replace("<head>", `<head><base href="${pathToFileURL(ui + path.sep).href}">`)
      .replace(/<script src="[^"]+"><\/script>/g, "")
      .replace("</body>", `<script>(${checkSidebar.toString()})()</script></body>`);
    const fixture = path.join(temp, "sidebar.html");
    fs.writeFileSync(fixture, html);
    const output = execFileSync(edge, [
      "--headless", "--disable-gpu", "--no-first-run", `--user-data-dir=${path.join(temp, "profile")}`,
      "--window-size=1280,1000", "--dump-dom", "--virtual-time-budget=4000", pathToFileURL(fixture).href,
    ], { encoding: "utf8", timeout: 60000, maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    const match = output.match(/<pre id="sidebar-results"[^>]*>([\s\S]*?)<\/pre>/);
    assert.ok(match, "browser did not report layout results");
    const results = JSON.parse(match[1]);
    assert.equal(results.error, undefined, results.error);
    assert.equal(results.scenarios, 20);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
