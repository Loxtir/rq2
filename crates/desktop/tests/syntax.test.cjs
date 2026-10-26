const assert = require("node:assert/strict");
const { test } = require("node:test");
const { syntaxParts } = require("../ui/syntax.js");

function has(parts, text, kind) {
  assert.ok(parts.some((part) => part.text === text && part.kind === kind), `${text}: ${kind}`);
}

test("JSON keys, strings, numbers, booleans, and null have distinct colors", () => {
  const source = '{"name": "Alice", "total": -1.25e+3, "active": true, "extra": null}';
  const parts = syntaxParts(source, "json");
  assert.equal(parts.map((part) => part.text).join(""), source);
  has(parts, '"name"', "json-key");
  has(parts, '"Alice"', "json-string");
  has(parts, "-1.25e+3", "json-number");
  has(parts, "true", "json-boolean");
  has(parts, "null", "json-null");
  has(parts, ":", "json-punctuation");
});

test("JSON escapes and variable placeholders remain intact", () => {
  const source = String.raw`{"say": "hello \"{{user}}\"", "count": {{count}}}`;
  const parts = syntaxParts(source, "json");
  assert.equal(parts.map((part) => part.text).join(""), source);
  has(parts, String.raw`"hello \"{{user}}\""`, "json-string");
  has(parts, "{{count}}", "");
});

test("JavaScript highlights scripts without interpreting their contents", () => {
  const source = 'const data = pm.response.json();\n// keep response\nif (data.ok === true) { pm.environment.set("id", data.id); }';
  const parts = syntaxParts(source, "javascript");
  assert.equal(parts.map((part) => part.text).join(""), source);
  has(parts, "const", "syntax-keyword");
  has(parts, "if", "syntax-keyword");
  has(parts, "json", "syntax-function");
  has(parts, "set", "syntax-function");
  has(parts, "// keep response", "syntax-comment");
  has(parts, '"id"', "json-string");
});

test("comments, strings, and template literals take precedence over keywords", () => {
  const source = "/* const true 123 */\nconst url = 'https://api.test';\nreturn `hello ${name}`;";
  const parts = syntaxParts(source, "javascript");
  has(parts, "/* const true 123 */", "syntax-comment");
  has(parts, "'https://api.test'", "json-string");
  has(parts, "`hello ${name}`", "json-string");
  assert.equal(parts.map((part) => part.text).join(""), source);
});

test("Unicode identifiers, tabs, numeric literals, and blank lines are preserved", () => {
  const source = "let café = 0xFF;\n\tconst price = 1_000n;\n\n";
  const parts = syntaxParts(source, "javascript");
  has(parts, "café", "");
  has(parts, "0xFF", "json-number");
  has(parts, "1_000n", "json-number");
  assert.equal(parts.map((part) => part.text).join(""), source);
});

test("incomplete code is highlighted without parsing errors", () => {
  for (const [language, source] of [
    ["json", '{"value": "typing'],
    ["javascript", 'const value = "typing'],
    ["javascript", "/* unfinished comment"],
    ["javascript", "`unfinished template"],
  ]) {
    const parts = syntaxParts(source, language);
    assert.equal(parts.map((part) => part.text).join(""), source);
    assert.ok(parts.some((part) => part.kind));
  }
});

test("HTML-like content remains literal source text", () => {
  for (const language of ["json", "javascript"]) {
    const source = '{"html": "<img src=x onerror=alert(1)>", "text": "&<>"}';
    assert.equal(syntaxParts(source, language).map((part) => part.text).join(""), source);
  }
});

test("plain text is not syntax highlighted", () => {
  const source = "<xml>const true 42</xml>";
  assert.deepEqual(syntaxParts(source, null), [{ text: source, kind: "" }]);
  assert.deepEqual(syntaxParts("", "json"), []);
});

test("cURL highlights commands, options, and quoted arguments without changing text", () => {
  const source = `curl -X POST 'https://api.test/items?x=1&y=2' -H 'Authorization: Bearer ***' --data-raw '{"name":"Alice"}'`;
  const parts = syntaxParts(source, "shell");
  has(parts, "curl", "syntax-function");
  has(parts, "-X", "syntax-keyword");
  has(parts, "-H", "syntax-keyword");
  has(parts, "--data-raw", "syntax-keyword");
  has(parts, "'Authorization: Bearer ***'", "json-string");
  assert.equal(parts.map((part) => part.text).join(""), source);
});

test("shell quotes, escapes, line continuations, and HTML-like arguments remain literal", () => {
  const source = "curl.exe \\\n  --data-raw 'it'\\''s <img src=x onerror=alert(1)>' # comment";
  const parts = syntaxParts(source, "shell");
  has(parts, "curl.exe", "syntax-function");
  has(parts, "\\\n", "json-punctuation");
  has(parts, "\\'", "json-punctuation");
  has(parts, "# comment", "syntax-comment");
  assert.equal(parts.map((part) => part.text).join(""), source);
});
