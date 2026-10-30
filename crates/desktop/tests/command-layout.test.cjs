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

async function checkCommandLayout() {
  const result = document.createElement("pre");
  result.id = "command-results";
  result.style.position = "fixed";
  document.body.append(result);
  const check = (condition, message) => { if (!condition) throw new Error(message); };
  const frame = () => new Promise((resolve) => setTimeout(resolve, 50));
  let scenarios = 0;
  try {
    const body = document.querySelector(".inspector-body");
    const curl = document.getElementById("curl");
    const details = document.getElementById("command-details");
    const summary = document.getElementById("summary");
    const problem = document.getElementById("preview-problem");
    check(details?.tagName === "DETAILS" && details.open, "Details must be collapsible and initially expanded");
    check(!body.textContent.includes("Runs in bash or zsh"), "command description was not removed");
    summary.innerHTML = "<dt>method</dt><dd>POST</dd><dt>params</dt><dd>0</dd><dt>headers</dt><dd>1</dd><dt>auth</dt><dd>none</dd><dt>body</dt><dd>347 B</dd>";
    problem.textContent = "Missing path parameter: orderId";
    for (const height of [600, 1000]) {
      for (const width of [240, 440]) {
        document.documentElement.style.height = `${height}px`;
        document.body.style.height = `${height}px`;
        document.documentElement.style.setProperty("--right-w", `${width}px`);
        for (const large of [false, true]) {
          curl.textContent = large ? "curl 'https://api.test'\n" + "  -H 'Content-Type: application/json'\n".repeat(200) : "curl 'https://api.test'";
          for (const warning of [false, true]) {
            problem.hidden = !warning;
            let expandedHeight;
            for (const open of [true, false, true]) {
              details.open = open;
              await frame();
              const label = `${height}px/${width}px large=${large} warning=${warning} details=${open}`;
              const bounds = body.getBoundingClientRect();
              const commandBounds = curl.getBoundingClientRect();
              const detailBounds = details.getBoundingClientRect();
              const actions = document.querySelector(".curl-actions").getBoundingClientRect();
              check(actions.bottom <= commandBounds.top + 1, `${label}: copy actions overlap cURL`);
              check(commandBounds.height > 0 && commandBounds.bottom <= detailBounds.top + 1, `${label}: cURL overlaps Details`);
              check(detailBounds.bottom <= bounds.bottom - 18 + 1, `${label}: Details leave the visible pane`);
              check(body.scrollHeight <= body.clientHeight + 1, `${label}: the whole panel scrolls`);
              if (warning) {
                const warningBounds = problem.getBoundingClientRect();
                check(commandBounds.bottom <= warningBounds.top + 1 && warningBounds.bottom <= detailBounds.top + 1, `${label}: warning overlaps content`);
              }
              if (large) {
                check(curl.scrollHeight > curl.clientHeight && getComputedStyle(curl).overflowY === "auto", `${label}: oversized cURL cannot scroll`);
                const last = document.getElementById("console-details").getBoundingClientRect();
                check(Math.abs(last.bottom - (bounds.bottom - 18)) <= 2, `${label}: cURL leaves unused space below Details and Console`);
                if (height === 1000) check(commandBounds.height > 500, `${label}: cURL still has a fixed height cap`);
                curl.scrollTop = 100;
                check(curl.scrollTop > 0 && body.scrollTop === 0, `${label}: cURL does not scroll independently`);
                curl.scrollTop = 0;
              } else {
                check(curl.scrollHeight <= curl.clientHeight + 1, `${label}: short cURL has an unnecessary scrollbar`);
                check(commandBounds.height < 100, `${label}: short cURL is stretched instead of fitting its content`);
              }
              if (open) expandedHeight = commandBounds.height;
              else {
                check(detailBounds.height <= details.querySelector("summary").getBoundingClientRect().height + 16, `${label}: collapsed Details still take full height`);
                if (large) check(commandBounds.height > expandedHeight + 50, `${label}: collapsing Details does not free command space`);
              }
              scenarios++;
            }
          }
        }
      }
    }
    result.textContent = JSON.stringify({ scenarios });
  } catch (error) {
    result.textContent = JSON.stringify({ scenarios, error: error.message });
  }
}

test("cURL grows with content and scrolls without hiding collapsible Details", { skip: !edge }, () => {
  const tempRoot = path.join(process.env.TEMP || os.tmpdir(), "opencode");
  fs.mkdirSync(tempRoot, { recursive: true });
  const temp = fs.mkdtempSync(path.join(tempRoot, "rq-command-"));
  try {
    const ui = path.resolve(__dirname, "../ui");
    const html = fs.readFileSync(path.join(ui, "index.html"), "utf8")
      .replace("<head>", `<head><base href="${pathToFileURL(ui + path.sep).href}">`)
      .replace(/<script src="[^"]+"><\/script>/g, "")
      .replace("</body>", `<script>(${checkCommandLayout.toString()})()</script></body>`);
    const fixture = path.join(temp, "command.html");
    fs.writeFileSync(fixture, html);
    const output = execFileSync(edge, [
      "--headless", "--disable-gpu", "--no-first-run", `--user-data-dir=${path.join(temp, "profile")}`,
      "--window-size=1280,1100", "--dump-dom", "--virtual-time-budget=6000", pathToFileURL(fixture).href,
    ], { encoding: "utf8", timeout: 60000, maxBuffer: 4 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    const match = output.match(/<pre id="command-results"[^>]*>([\s\S]*?)<\/pre>/);
    assert.ok(match, "browser did not report command layout results");
    const results = JSON.parse(match[1]);
    assert.equal(results.error, undefined, results.error);
    assert.equal(results.scenarios, 48);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
