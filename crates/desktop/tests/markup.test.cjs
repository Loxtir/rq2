const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

test("every element app.js looks up by id exists before the scripts run", () => {
  const html = fs.readFileSync(path.join(__dirname, "../ui/index.html"), "utf8");
  const app = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  const beforeScripts = html.slice(0, html.indexOf("<script"));
  const defined = new Set([...beforeScripts.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]));
  const used = new Set([...app.matchAll(/\$\("([^"]+)"\)/g)].map((match) => match[1]));
  const missing = [...used].filter((id) => !defined.has(id));
  assert.deepEqual(missing, []);
});

test("app.js declares each top-level function once, since a later one silently wins", () => {
  const app = fs.readFileSync(path.join(__dirname, "../ui/app.js"), "utf8");
  const names = [...app.matchAll(/^(?:async )?function (\w+)/gm)].map((match) => match[1]);
  const duplicates = names.filter((name, index) => names.indexOf(name) !== index);
  assert.deepEqual(duplicates, []);
});
