// Small, error-tolerant lexers: highlighting must work while code is incomplete.
// Parts retain the exact source text; the editor renders them with text nodes.
function syntaxParts(text, language, caret = null) {
  if (language === "query") return querySyntaxParts(text, caret);
  if (!language) return [{ text, kind: "" }];
  if (language === "graphql") return graphqlSyntaxParts(text);
  const pattern = language === "json"
    ? /(?<comment>\/\/[^\r\n]*)|(?<variable>\{\{[^{}]+\}\})|(?<string>"(?:\\[\s\S]|[^"\\])*"?)|(?<number>-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)|(?<literal>\b(?:true|false|null)\b)|(?<punctuation>[{}\[\],:])/g
    : language === "shell"
    ? /(?<string>'[^']*'?|"(?:\\[\s\S]|[^"\\])*"?)|(?<escape>\\[\s\S])|(?<comment>#[^\n]*)|(?<option>--?[A-Za-z][\w-]*)|(?<command>\bcurl(?:\.exe)?\b)|(?<punctuation>[|&;<>])/g
    : /(?<comment>\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$))|(?<string>"(?:\\[\s\S]|[^"\\\n])*"?|'(?:\\[\s\S]|[^'\\\n])*'?|`(?:\\[\s\S]|[^`\\])*`?)|(?<number>\b(?:0[xX][\da-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?[\d_]+)?n?)\b)|(?<literal>\b(?:true|false|null|undefined|NaN|Infinity)\b)|(?<keyword>\b(?:async|await|break|case|catch|class|const|continue|debugger|default|delete|do|else|export|extends|finally|for|from|function|if|import|in|instanceof|let|new|of|return|static|super|switch|this|throw|try|typeof|var|void|while|with|yield)\b)|(?<identifier>[$\p{ID_Start}][$\p{ID_Continue}\u200c\u200d]*)|(?<punctuation>[{}\[\]().,;:?=+*/%<>!&|~^-])/gu;
  const parts = [];
  let at = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > at) parts.push({ text: text.slice(at, match.index), kind: "" });
    const type = Object.keys(match.groups).find((key) => match.groups[key] !== undefined);
    const rest = text.slice(match.index + match[0].length);
    let kind = "";
    if (type === "string") kind = language === "json" && /^\s*:/.test(rest) ? "json-key" : "json-string";
    else if (type === "number") kind = "json-number";
    else if (type === "literal") kind = match[0] === "null" ? "json-null" : "json-boolean";
    else if (type === "keyword" || type === "comment") kind = `syntax-${type}`;
    else if (type === "option") kind = "syntax-keyword";
    else if (type === "command") kind = "syntax-function";
    else if (type === "escape") kind = "json-punctuation";
    else if (type === "punctuation") kind = "json-punctuation";
    else if (type === "identifier" && /^\s*\(/.test(rest)) kind = "syntax-function";
    parts.push({ text: match[0], kind });
    at = match.index + match[0].length;
  }
  if (at < text.length) parts.push({ text: text.slice(at), kind: "" });
  return parts;
}

function querySyntaxParts(text, caret) {
  const pattern = /(?<comment>#[^\r\n]*)|(?<string>"(?:\\[\s\S]|[^"\\])*"?)|(?<variable>\{\{[^{}]+\}\})|(?<operator>==|!=|\.\.\.|\*\*|[!?*:])|(?<bracket>[{}\[\]])|(?<number>-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)|(?<name>[^\s{}\[\]:!*#=,?".]+(?:\.(?!\.\.)[^\s{}\[\]:!*#=,?".]*)*)/g;
  const parts = [];
  const stack = [];
  const brackets = [];
  let at = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > at) parts.push({ text: text.slice(at, match.index), kind: "" });
    const type = Object.keys(match.groups).find((key) => match.groups[key] !== undefined);
    let kind = type === "string" ? "json-string"
      : type === "comment" ? "syntax-comment"
      : type === "number" ? "json-number"
      : type === "operator" ? "syntax-keyword" : "";
    if (type === "name") {
      kind = ["in", "not"].includes(match[0]) ? "syntax-keyword"
        : match[0] === "null" ? "json-null"
        : ["true", "false"].includes(match[0]) ? "json-boolean" : "json-key";
    }
    const part = { text: match[0], kind };
    if (type === "bracket") {
      const bracket = { part, offset: match.index, partner: null };
      const opening = "{[".includes(match[0]);
      const top = stack[stack.length - 1];
      if (opening) {
        part.kind = `query-bracket-${stack.length % 6}`;
        stack.push(bracket);
      } else if (top && "{[".indexOf(top.part.text) === "}]".indexOf(match[0])) {
        stack.pop();
        part.kind = top.part.kind;
        bracket.partner = top;
        top.partner = bracket;
      } else part.kind = "query-unmatched";
      brackets.push(bracket);
    }
    parts.push(part);
    at = match.index + match[0].length;
  }
  if (at < text.length) parts.push({ text: text.slice(at), kind: "" });
  for (const bracket of stack) bracket.part.kind += " query-unmatched";
  const active = brackets.find((bracket) => bracket.offset === caret)
    || brackets.find((bracket) => bracket.offset === caret - 1 && caret !== null);
  if (active?.partner) {
    active.part.kind += " query-match";
    active.partner.part.kind += " query-match";
  }
  return parts;
}

function graphqlSyntaxParts(text) {
  const pattern = /(?<comment>#[^\r\n]*)|(?<variable>\{\{[^{}]+\}\})|(?<string>"""[\s\S]*?(?:"""|$)|"(?:\\[\s\S]|[^"\\\n])*"?)|(?<argument>\$[_A-Za-z]\w*)|(?<directive>@[_A-Za-z]\w*)|(?<number>-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)|(?<name>[_A-Za-z]\w*)|(?<punctuation>\.\.\.|[{}()\[\]:=!|&,])/g;
  const keywords = new Set(["query", "mutation", "subscription", "fragment", "on"]);
  const parts = [];
  let at = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > at) parts.push({ text: text.slice(at, match.index), kind: "" });
    const type = Object.keys(match.groups).find((key) => match.groups[key] !== undefined);
    const word = match[0];
    const kind = type === "comment" ? "syntax-comment"
      : type === "string" ? "json-string"
      : type === "number" ? "json-number"
      : type === "argument" ? "json-boolean"
      : type === "directive" ? "syntax-keyword"
      : type === "punctuation" ? "json-punctuation"
      : type !== "name" ? ""
      : keywords.has(word) ? "syntax-keyword"
      : word === "null" ? "json-null"
      : word === "true" || word === "false" ? "json-boolean"
      : /^[A-Z]/.test(word) ? "syntax-function"
      : "json-key";
    parts.push({ text: word, kind });
    at = match.index + word.length;
  }
  if (at < text.length) parts.push({ text: text.slice(at), kind: "" });
  return parts;
}

// Re-indents JSON by its tokens rather than by parsing, so text that is not
// strict JSON yet (`//` comments, unquoted {{placeholders}}) still formats.
function formatJson(text) {
  const pattern = /(?<comment>\/\/[^\r\n]*)|(?<variable>\{\{[^{}]+\}\})|(?<string>"(?:\\[\s\S]|[^"\\])*"?)|(?<punctuation>[{}\[\],:])|(?<atom>[^\s{}\[\],:"\/]+|\/)/g;
  const tokens = [...text.matchAll(pattern)].map((match) => ({
    text: match[0],
    type: Object.keys(match.groups).find((key) => match.groups[key] !== undefined),
    lineBefore: /\n/.test(text.slice(0, match.index).match(/\s*$/)[0]),
  }));
  let out = "";
  let depth = 0;
  let newline = false;
  const pad = () => `\n${"  ".repeat(depth)}`;
  tokens.forEach((token, index) => {
    const value = token.text;
    const close = value === "}" || value === "]";
    if (token.type === "comment") {
      if (!token.lineBefore && out) out = `${out.trimEnd()} ${value}`;
      else out += (out && !/\n *$/.test(out) ? pad() : "") + value;
      newline = true;
      return;
    }
    if (token.type === "punctuation" && close) {
      const empty = /[{[]$/.test(out);
      if (!empty) depth = Math.max(0, depth - 1);
      out = empty ? out + value : out.replace(/\n *$/, "") + pad() + value;
    } else if (value === ",") {
      out += "," + pad();
    } else if (value === ":") {
      out += ": ";
    } else {
      if (newline) out = out.replace(/\n *$/, "") + pad();
      out += value;
      if (value === "{" || value === "[") {
        const next = tokens[index + 1]?.text;
        if (next !== "}" && next !== "]") {
          depth += 1;
          out += pad();
        }
      }
    }
    newline = false;
  });
  return out.replace(/[ \t]+\n/g, "\n").trimEnd();
}

if (typeof module !== "undefined") module.exports = { syntaxParts, formatJson };
