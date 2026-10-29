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

test("JSON line comments are highlighted without treating URLs or escaped quotes as comments", () => {
  const source = String.raw`{
  // disabled: {{unused}} "not a string"
  "url": "https://api.test/path//segment",
  "text": "escaped \" // still inside", // inline comment
  "value": {{value}}
} // final comment`;
  const parts = syntaxParts(source, "json");
  has(parts, '// disabled: {{unused}} "not a string"', "syntax-comment");
  has(parts, '"https://api.test/path//segment"', "json-string");
  has(parts, String.raw`"escaped \" // still inside"`, "json-string");
  has(parts, "// inline comment", "syntax-comment");
  has(parts, "// final comment", "syntax-comment");
  has(parts, "{{value}}", "");
  assert.equal(parts.map((part) => part.text).join(""), source);
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

test("queries highlight identifiers, comparisons, membership, scalars and comments", () => {
  const source = '{ payload { items[? item_id { value in ["{{myVar}}", 42, true, null] }] { attributes !secret... children** { id } } } } # note';
  const parts = syntaxParts(source, "query");
  assert.equal(parts.map((part) => part.text).join(""), source);
  for (const field of ["payload", "items", "item_id", "value", "attributes"]) has(parts, field, "json-key");
  for (const operator of ["in", "?", "!", "...", "**"]) has(parts, operator, "syntax-keyword");
  has(parts, '"{{myVar}}"', "json-string");
  has(parts, "42", "json-number");
  has(parts, "true", "json-boolean");
  has(parts, "null", "json-null");
  has(parts, "# note", "syntax-comment");
});

test("query braces and brackets share colors with their matching partner", () => {
  const parts = syntaxParts("{ items[? profile { name == \"Mike\" }] { id } }", "query");
  const stack = [];
  const colors = new Set();
  for (const part of parts.filter((part) => part.kind.startsWith("query-bracket"))) {
    if ("{[".includes(part.text)) { stack.push(part.kind); colors.add(part.kind); }
    else assert.equal(part.kind, stack.pop());
  }
  assert.equal(stack.length, 0);
  assert.ok(colors.size >= 3);
});

test("query matching underlines only the caret's pair and ignores quoted or commented braces", () => {
  const source = '{ name == "{} {{myVar}}" # } ]\n profile { id } }';
  const parts = syntaxParts(source, "query", source.indexOf("{ id"));
  const matched = parts.filter((part) => part.kind.includes("query-match"));
  assert.deepEqual(matched.map((part) => part.text), ["{", "}"]);
  assert.ok(matched.every((part) => part.kind.startsWith("query-bracket-1")));
  assert.equal(parts.map((part) => part.text).join(""), source);
  assert.ok(!syntaxParts(source, "query", source.indexOf("{}") + 1).some((part) => part.kind.includes("query-match")));
});

test("query highlighting tolerates incomplete input, unmatched delimiters and HTML-like strings", () => {
  for (const source of ['{ items[? name == "typing', '{ items ]', '{ html == "<img src=x onerror=alert(1)>" }', "{ a.b näme @id $value }"]) {
    const parts = syntaxParts(source, "query");
    assert.equal(parts.map((part) => part.text).join(""), source);
  }
  assert.ok(syntaxParts("{ items ]", "query").some((part) => part.kind.includes("query-unmatched")));
});

test("recursive query edges and variable predicates retain highlighting and brace matching", () => {
  for (const mode of ["...", "**"]) {
    const source = `{ items${mode}children[? definition { item_id { value == "{{myVar}}" } }] { definition { item_id attributes } } }`;
    const parts = syntaxParts(source, "query", 0);
    assert.equal(parts.map((part) => part.text).join(""), source);
    has(parts, mode, "syntax-keyword");
    has(parts, "children", "json-key");
    has(parts, '"{{myVar}}"', "json-string");
    assert.equal(parts.filter((part) => part.kind.includes("query-match")).length, 2);
    assert.ok(!parts.some((part) => part.kind.includes("query-unmatched")));
  }
});
