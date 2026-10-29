// Small, error-tolerant lexers: highlighting must work while code is incomplete.
// Parts retain the exact source text; the editor renders them with text nodes.
function syntaxParts(text, language, caret = null) {
  if (language === "query") return querySyntaxParts(text, caret);
  if (!language) return [{ text, kind: "" }];
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

if (typeof module !== "undefined") module.exports = { syntaxParts };
