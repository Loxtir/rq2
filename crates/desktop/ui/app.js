// The frontend never sees a response body. It sees metadata, then asks Rust to
// project a stored body down to text. That is deliberate: the same boundary
// keeps a 500k-line payload out of the webview later.
//
// Everything editable is a draft held here and written through one command,
// `save`, which re-renders the collection in Rust. The window never writes a
// YAML file itself, so secrets keep a single exit.

const { invoke } = window.__TAURI__.core;

const $ = (id) => document.getElementById(id);
const environmentStorageKey = (path) => `rq.environment:${path}`;

const state = {
  workspace: null,
  selection: null, // { kind, slug, path, node, collection }
  draft: null, // the editable copy of the selection
  saved: "", // JSON of the draft as loaded, for the dirty check
  response: null,
  savingResponse: false,
  env: "",
  preview: "",
  previewTimer: null,
  environmentEditor: null,
  requestTabs: [],
  activeTab: null,
  activeRequestTab: null,
  pendingRequests: new Set(),
  cancelRequests: new Map(),
  projectionVersion: 0,
  treeSearch: "",
};

// Unsaved edits, per node. Switching requests does not throw work away: the
// draft stays here and comes back when you return. The tree marks it.
const drafts = new Map();

const nodeKey = (slug, path) => `${slug}:${path.join(".")}`;
const draftKey = (kind, slug, path) => `${kind}:${nodeKey(slug, path)}`;
const requestTabKey = (slug, path) => `${state.workspace?.path || ""}|${nodeKey(slug, path)}`;

function draftFor(kind, slug, path, build) {
  const key = draftKey(kind, slug, path);
  const existing = drafts.get(key);
  if (existing) return existing;

  const entry = { draft: build(), saved: "" };
  entry.saved = JSON.stringify(entry.draft);
  drafts.set(key, entry);
  return entry;
}

/* ---------- formatting ---------- */

function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

function elapsed(ms) {
  return ms < 1 ? "<1 ms" : `${Math.round(ms)} ms`;
}

function statusClass(code) {
  if (code >= 200 && code < 300) return "status-ok";
  if (code >= 300 && code < 400) return "status-warn";
  return "status-error";
}

const clone = (value) => (value == null ? value : JSON.parse(JSON.stringify(value)));

/* ---------- layout: draggable, collapsible panes ---------- */

const LAYOUT_KEY = "rq.layout";
const DEFAULTS = { left: 288, right: 380, leftOpen: true, rightOpen: true };

const layout = (() => {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(LAYOUT_KEY) || "{}") };
  } catch {
    return { ...DEFAULTS };
  }
})();

function applyLayout() {
  const root = document.documentElement.style;
  root.setProperty("--left-w", layout.leftOpen ? `${layout.left}px` : "0px");
  root.setProperty("--right-w", layout.rightOpen ? `${layout.right}px` : "0px");
  root.setProperty("--split-l", layout.leftOpen ? "4px" : "0px");
  root.setProperty("--split-r", layout.rightOpen ? "4px" : "0px");

  $("reveal-left").hidden = layout.leftOpen;
  $("reveal-right").hidden = layout.rightOpen;
  const collapsedBar = $("collapsed-bar");
  collapsedBar.hidden = layout.leftOpen && layout.rightOpen;
  collapsedBar.classList.toggle("reveal-right-only", layout.leftOpen && !layout.rightOpen);

  document.body.classList.toggle("left-closed", !layout.leftOpen);
  document.body.classList.toggle("right-closed", !layout.rightOpen);
}

function saveLayout() {
  localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
}

function toggleLeft(open) {
  layout.leftOpen = open ?? !layout.leftOpen;
  applyLayout();
  saveLayout();
}

function toggleRight(open) {
  layout.rightOpen = open ?? !layout.rightOpen;
  applyLayout();
  saveLayout();
}

function setupSplitter(element, side) {
  const min = side === "left" ? 190 : 260;
  const max = side === "left" ? 560 : 760;
  let startX = 0;
  let startWidth = 0;
  let dragging = false;

  element.addEventListener("pointerdown", (event) => {
    if (side === "left" ? !layout.leftOpen : !layout.rightOpen) return;
    dragging = true;
    startX = event.clientX;
    startWidth = layout[side];
    element.setPointerCapture(event.pointerId);
    document.body.classList.add("dragging");
    event.preventDefault();
  });

  element.addEventListener("pointermove", (event) => {
    if (!dragging) return;
    const delta = event.clientX - startX;
    const next = side === "left" ? startWidth + delta : startWidth - delta;
    layout[side] = Math.round(Math.max(min, Math.min(max, next)));
    applyLayout();
  });

  const end = (event) => {
    if (!dragging) return;
    dragging = false;
    try {
      element.releasePointerCapture(event.pointerId);
    } catch {
      /* the pointer can already be gone */
    }
    document.body.classList.remove("dragging");
    saveLayout();
  };

  element.addEventListener("pointerup", end);
  element.addEventListener("pointercancel", end);
  element.addEventListener("dblclick", () => {
    layout[side] = DEFAULTS[side];
    applyLayout();
    saveLayout();
  });
}

/* ---------- URL and parameter helpers ---------- */

const splitUrl = (url) => {
  const at = url.indexOf("?");
  if (at === -1) return { base: url, pairs: [] };
  const query = url.slice(at + 1);
  const pairs =
    query === ""
      ? []
      : query.split("&").map((part) => {
          const eq = part.indexOf("=");
          return eq === -1
            ? { name: part, value: "" }
            : { name: part.slice(0, eq), value: part.slice(eq + 1) };
        });
  return { base: url.slice(0, at), pairs };
};

/// Whole `:name` segments in the path, in order. Only a segment that *starts*
/// with `:` counts, so `https:` and gRPC's `v1:batchGet` are left alone.
function pathTokens(url) {
  const names = url
    .split("/")
    .filter((segment) => segment.startsWith(":") && segment.length > 1)
    .map((segment) => segment.slice(1).split("?")[0])
    .filter(Boolean);
  // `:id/:id` is one variable with one value, not two rows.
  return [...new Set(names)];
}

function displayedUrl(draft) {
  const query = (draft.params || [])
    .filter((param) => !param.disabled && (param.name || param.value))
    .map((param) => param.value ? `${param.name}=${param.value}` : param.name)
    .join("&");
  return query ? `${draft.url}?${query}` : draft.url;
}

/// Keep `draft.url` canonical while the URL field displays the enabled params.
/// Disabled rows retain their values and positions when the inline query edits.
function absorbUrlInput() {
  const parsed = splitUrl($("url").value);
  const previous = state.draft.params || [];
  const next = [];
  let pairIndex = 0;

  for (const param of previous) {
    if (param.disabled) {
      next.push(param);
    } else if (pairIndex < parsed.pairs.length) {
      next.push({ ...parsed.pairs[pairIndex++], disabled: false });
    }
  }
  while (pairIndex < parsed.pairs.length) {
    next.push({ ...parsed.pairs[pairIndex++], disabled: false });
  }

  state.draft.url = parsed.base;
  state.draft.params = next;
}

function updateUrlDisplay() {
  if (!state.draft || state.selection?.kind !== "request") return;
  $("url").value = displayedUrl(state.draft);
  paintVariables($("url"));
}

/// Canonicalize the edited field on blur, then redraw it from the saved rows.
function absorbQuery() {
  if (!state.draft || state.selection?.kind !== "request") return;
  absorbUrlInput();
  updateUrlDisplay();
  renderParams();
  updateDirty();
  schedulePreview();
}

/* ---------- variables ---------- */

const variablePattern = /\{\{([^{}]+)\}\}/g;

/// Every definition of `name` the selection can see, widest first.
///
/// Send order is globals, environment, collection, folders, request: a later
/// layer overrides an earlier one, so the last entry here is the winner. Folders
/// come from the tree path of whatever is selected, outermost first.
function variableScopes(name) {
  const workspace = state.workspace;
  const scopes = [];
  if (!workspace) return scopes;

  const push = (variables, label, kind) => {
    const variable = (variables || []).find((entry) => entry.name === name);
    if (variable) scopes.push({ label, kind, variable });
  };

  push(workspace.globals, "globals", "globals");

  const environment = (workspace.environments || []).find(
    (entry) => entry.name === state.env
  );
  if (environment) {
    push(
      environment.variables,
      `environment ${environment.name}`,
      "environment"
    );
  }

  const collection = (workspace.collections || []).find(
    (entry) => entry.slug === state.selection?.slug
  );
  if (!collection) return scopes;
  push(collection.variables, "collection", "collection");

  let items = collection.items;
  for (const index of state.selection?.path || []) {
    const node = items?.[index];
    if (!node) break;
    const label = node.kind === "request" ? "request" : `folder ${node.name}`;
    push(node.variables, label, node.kind);
    items = node.items;
  }

  return scopes;
}

/// The definition the selected request would actually use, plus every wider one
/// it shadows. Nested `{{...}}` inside a value is expanded by Rust at send time,
/// so the raw value is what the editor shows.
function variableInfo(name) {
  const scopes = variableScopes(name);
  const winner = scopes[scopes.length - 1];

  if (!winner) {
    return {
      defined: false,
      scope: "undefined",
      kind: null,
      value: "",
      secret: false,
      hasValue: false,
      scopes,
    };
  }

  return {
    defined: true,
    scope: winner.label,
    kind: winner.kind,
    value: winner.variable.value,
    secret: Boolean(winner.variable.secret),
    hasValue: Boolean(winner.variable.has_value),
    scopes,
  };
}

/* ---------- {{variable}} tinting ----------
   An input cannot colour part of its own text. A layer above it paints a tint
   behind every token instead: the text stays the real input text (crisp, with a
   caret), and the layer is the only thing that knows where a token is. The
   layer takes no pointer events; hover is found from the token rectangles.
   Code editors also paint syntax here and hide the textarea's text, but keep
   its native caret, selection, undo, and input behavior. */

function withVariables(input) {
  if (input.dataset.layered) return input.parentElement;

  const wrap = document.createElement("div");
  wrap.className = "varfield";
  if (input.classList.contains("body-text")) wrap.classList.add("varfield-body");
  if (input.classList.contains("url-input")) wrap.classList.add("varfield-url");
  const layer = document.createElement("div");
  layer.className = "varlayer";
  layer.setAttribute("aria-hidden", "true");

  input.parentNode.insertBefore(wrap, input);
  wrap.append(input, layer);
  input.dataset.layered = "1";

  input.addEventListener("input", () => paintVariables(input));
  input.addEventListener("scroll", () => syncVariableLayer(input, layer));
  if (input.tagName === "TEXTAREA") {
    new ResizeObserver(() => syncVariableLayer(input, layer)).observe(input);
  }

  wireVariableHover(wrap);
  paintVariables(input);
  return wrap;
}

function paintVariables(input) {
  const layer = input.parentElement?.querySelector?.(".varlayer");
  if (!layer) return;

  layer.replaceChildren();
  const script = input.classList.contains("script-editor");
  const language = script ? "javascript"
    : input.id === "body-text" && $("body-language").value === "json" ? "json" : null;
  input.parentElement.classList.toggle("syntax-editor", Boolean(language));
  for (const part of syntaxParts(input.value, language)) {
    const target = part.kind ? document.createElement("span") : layer;
    if (part.kind) {
      target.className = part.kind;
      layer.append(target);
    }
    if (script) target.append(document.createTextNode(part.text));
    else appendVariableText(target, part.text, input.id === "url");
  }
  // Textareas retain a final empty line; the overlay must have one too.
  if (input.value.endsWith("\n")) layer.append(document.createTextNode("\u200b"));
  syncVariableLayer(input, layer);
}

function syncVariableLayer(input, layer) {
  if (input.tagName === "TEXTAREA") {
    // Exclude the textarea's scrollbars so both layers wrap at the same column.
    layer.style.width = `${input.clientWidth + 2}px`;
    layer.style.height = `${input.clientHeight + 2}px`;
  }
  layer.scrollTop = input.scrollTop;
  layer.scrollLeft = input.scrollLeft;
}

function appendVariableText(layer, text, paths) {
  const tokens = [];
  for (const match of text.matchAll(variablePattern)) {
    tokens.push({ start: match.index, end: match.index + match[0].length, kind: "variable", text: match[0], name: match[1].trim() });
  }
  if (paths) {
    const queryAt = text.indexOf("?");
    const path = text.slice(0, queryAt < 0 ? text.length : queryAt);
    for (const match of path.matchAll(/:[^/?#]+/g)) {
      if (match.index !== 0 && path[match.index - 1] !== "/") continue;
      tokens.push({ start: match.index, end: match.index + match[0].length, kind: "path", text: match[0] });
    }
  }
  tokens.sort((a, b) => a.start - b.start || a.end - b.end);
  let at = 0;

  for (const match of tokens) {
    if (match.start < at) continue;
    if (match.start > at) {
      layer.append(document.createTextNode(text.slice(at, match.start)));
    }
    const token = document.createElement("span");
    token.className = match.kind === "path" ? "path-token-url" : "var-token";
    if (match.kind === "variable") {
      token.dataset.name = match.name;
      if (!variableInfo(match.name).defined) token.classList.add("var-undefined");
    }
    token.textContent = match.text;
    layer.append(token);
    at = match.end;
  }

  if (at < text.length) layer.append(document.createTextNode(text.slice(at)));
}

function repaintVariables() {
  for (const input of document.querySelectorAll("[data-layered]")) {
    paintVariables(input);
  }
}

let varHoverTimer = null;
let varHideTimer = null;
let varHoverName = null;

function tokenAt(wrap, x, y) {
  for (const token of wrap.querySelectorAll(".var-token")) {
    const box = token.getBoundingClientRect();
    if (x >= box.left && x <= box.right && y >= box.top && y <= box.bottom) {
      return token;
    }
  }
  return null;
}

function wireVariableHover(wrap) {
  if (wrap.dataset.hoverWired) return;
  wrap.dataset.hoverWired = "1";

  wrap.addEventListener("mousemove", (event) => {
    const token = tokenAt(wrap, event.clientX, event.clientY);
    for (const other of wrap.querySelectorAll(".var-token.hovered")) {
      other.classList.remove("hovered");
    }
    if (!token) {
      scheduleHideVariable();
      return;
    }
    token.classList.add("hovered");
    clearTimeout(varHoverTimer);
    clearTimeout(varHideTimer);
    varHoverTimer = setTimeout(() => {
      const box = token.getBoundingClientRect();
      showVariable(token.dataset.name, box);
    }, 140);
  });

  wrap.addEventListener("mouseleave", scheduleHideVariable);
}

function scheduleHideVariable() {
  clearTimeout(varHoverTimer);
  clearTimeout(varHideTimer);
  varHideTimer = setTimeout(hideVariable, 240);
}

function showVariable(name, box) {
  const popover = $("var-popover");
  clearTimeout(varHideTimer);
  varHoverName = name;

  const info = variableInfo(name);
  $("var-name").textContent = `{{${name}}}`;
  $("var-scope").textContent = info.scope;
  $("var-scope").classList.toggle("tag-warn", !info.defined);

  const value = $("var-value");
  value.value = info.secret ? "" : info.value ?? "";
  value.placeholder = info.secret
    ? info.hasValue
      ? "•••••• stored — type to replace"
      : "not set"
    : "value";

  // A name nothing defines has to be created somewhere; a name that exists is
  // edited where it already lives, so no scope picker is needed.
  const createRow = $("var-create-row");
  createRow.hidden = info.defined;
  if (!info.defined) {
    const environmentOption = $("var-create").querySelector(
      'option[value="environment"]'
    );
    environmentOption.disabled = !state.env;
    if (!state.env && $("var-create").value === "environment") {
      $("var-create").value = "collection";
    }
  }

  const shadowed = info.scopes.slice(0, -1).map((scope) => scope.label);
  $("var-note").classList.remove("error-text");
  let note;
  if (info.secret) {
    note = `Secret stored in .secrets/ for ${info.scope}, never shown here. Leave empty to keep it.`;
  } else if (info.defined) {
    note = `Saving writes this to ${info.scope}.`;
  } else {
    note = "Not defined yet — saving creates it in the scope above.";
  }
  if (shadowed.length) {
    note += ` Also set in ${shadowed.join(", ")}, overridden here.`;
  }
  $("var-note").textContent = note;

  popover.hidden = false;

  // Under the token, flipped above or nudged left rather than leaving the window.
  const width = popover.offsetWidth;
  const height = popover.offsetHeight;
  let left = box.left;
  let top = box.bottom + 6;
  if (left + width > window.innerWidth - 8) left = window.innerWidth - width - 8;
  if (top + height > window.innerHeight - 8) top = box.top - height - 6;
  popover.style.left = `${Math.max(8, left)}px`;
  popover.style.top = `${Math.max(8, top)}px`;
}

function hideVariable() {
  clearTimeout(varHoverTimer);
  clearTimeout(varHideTimer);
  varHoverName = null;
  $("var-popover").hidden = true;
}

async function saveVariable() {
  const name = varHoverName;
  if (!name) return;

  const info = variableInfo(name);
  const typed = $("var-value").value;
  // An empty box on a secret means "leave the stored value alone".
  const value = info.secret && typed === "" ? null : typed;
  const create = info.defined ? null : $("var-create").value;

  if (!info.defined && typed === "") {
    $("var-note").classList.add("error-text");
    $("var-note").textContent = "Type a value first — an empty one would define nothing.";
    return;
  }

  $("var-note").classList.remove("error-text");
  try {
    state.workspace = await invoke("set_variable", {
      collection: state.selection?.slug ?? "",
      path: state.selection?.path ?? [],
      environment: state.env || null,
      edit: { name, value, secret: info.secret, create },
    });

    patchVariableDrafts(name, value, info.defined ? info.kind : create);
    renderWorkspace();
    repaintVariables();
    hideVariable();
    refreshPreview();
  } catch (error) {
    $("var-note").textContent = String(error);
    $("var-note").classList.add("error-text");
  }
}

/// A stashed collection draft still holds the old value. Without this, saving
/// that editor later would quietly undo the edit just made here.
///
/// Only the collection editor keeps variables in a draft; a folder's and a
/// request's are read straight from the tree, which the save reloaded.
function patchVariableDrafts(name, value, target) {
  if (target !== "collection" || !state.selection?.slug) return;
  const entry = drafts.get(`collection:${state.selection.slug}:`);
  if (!entry) return;

  const wasDirty = JSON.stringify(entry.draft) !== entry.saved;
  const variable = entry.draft.variables.find((item) => item.name === name);
  if (variable) {
    if (value !== null) variable.value = value;
    if (value !== null) variable.stored = true;
  } else {
    entry.draft.variables.push({
      name,
      value: value ?? "",
      secret: false,
      stored: true,
    });
  }
  if (!wasDirty) entry.saved = JSON.stringify(entry.draft);
  if (state.draft === entry.draft) updateDirty();
}

/* ---------- auth ---------- */

const AUTH_KINDS = [
  ["inherit", "inherit"],
  ["none", "no auth"],
  ["bearer", "bearer token"],
  ["basic", "basic"],
  ["apikey", "API key"],
];

const AUTH_FIELDS = {
  bearer: [{ key: "token", label: "Token", placeholder: "{{token}}" }],
  basic: [
    { key: "username", label: "Username" },
    { key: "password", label: "Password", placeholder: "{{password}}" },
  ],
  apikey: [
    { key: "key", label: "Name", placeholder: "X-Api-Key" },
    { key: "value", label: "Value", placeholder: "{{apiKey}}" },
  ],
};

const authMode = (auth) =>
  typeof auth === "string" ? auth : auth?.kind || "inherit";
const authFields = (auth) =>
  auth && typeof auth === "object" ? auth : {};

/// What a request actually sends: its own method, or the one it inherits.
function effectiveAuth(node) {
  const own = authMode(node.auth);
  if (own === "none") return null;
  if (own !== "inherit") return own;
  return node.inherited || null;
}

function renderAuth(host, auth, inherited, onChange) {
  host.replaceChildren();
  const mode = authMode(auth);
  const fields = authFields(auth);

  const line = document.createElement("div");
  line.className = "auth-line";
  const label = document.createElement("span");
  label.className = "field-label";
  label.textContent = "Mode";
  const select = document.createElement("select");
  select.className = "select select-auth";
  for (const [value, text] of AUTH_KINDS) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = text;
    select.append(option);
  }
  select.value = mode;
  line.append(label, select);
  host.append(line);

  if (mode === "inherit" || mode === "none") {
    const note = document.createElement("p");
    note.className = "hint auth-note";
    note.textContent =
      mode === "inherit"
        ? inherited
          ? `Inherits ${inherited} auth from above.`
          : "Nothing above sets auth, so none is sent."
        : "Sends no auth, and stops the chain: nothing below inherits either.";
    host.append(note);
  }

  for (const field of AUTH_FIELDS[mode] || []) {
    const row = document.createElement("div");
    row.className = "auth-field";
    const name = document.createElement("span");
    name.className = "field-label";
    name.textContent = field.label;

    const input = document.createElement("input");
    input.className = "cell";
    input.spellcheck = false;
    input.autocomplete = "off";
    input.placeholder = field.placeholder || "";
    input.value = fields[field.key] || "";
    // Mutate the draft's object rather than replacing it, so the next keystroke
    // still sees the previous one.
    input.addEventListener("input", () => {
      if (auth && typeof auth === "object") {
        auth[field.key] = input.value;
        auth.kind = mode;
      }
      onChange(auth, false);
    });

    row.append(name, input);
    withVariables(input);
    host.append(row);
  }

  if (mode === "apikey") {
    const row = document.createElement("div");
    row.className = "auth-field";
    const name = document.createElement("span");
    name.className = "field-label";
    name.textContent = "Send as";

    const where = document.createElement("select");
    where.className = "select select-narrow";
    for (const value of ["header", "query"]) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      where.append(option);
    }
    where.value = fields.in || "header";
    where.addEventListener("change", () => {
      if (auth && typeof auth === "object") auth.in = where.value;
      onChange(auth, false);
    });

    row.append(name, where);
    host.append(row);
  }

  select.addEventListener("change", () => {
    const next = select.value;
    if (next === "inherit" || next === "none") {
      onChange(next, true);
      return;
    }
    const value = { kind: next };
    for (const field of AUTH_FIELDS[next]) {
      value[field.key] = fields[field.key] || "";
    }
    if (next === "apikey") value.in = fields.in || "header";
    onChange(value, true);
  });
}


/* ---------- tree ---------- */

const keyFor = (slug, path) => `${slug}:${path.join(".")}`;

function requestMatchesSearch(node, query) {
  const params = (node.params || []).map((param) => `${param.name}=${param.value}`);
  const pathParams = Object.entries(node.path_params || {}).flatMap(([name, value]) => [
    `:${name}`,
    String(value),
  ]);
  const haystack = [node.name, node.url, ...params, ...pathParams]
    .join(" ")
    .toLocaleLowerCase();
  return haystack.includes(query);
}

function buildNodes(items, collection, parent, query) {
  const fragment = document.createDocumentFragment();
  let count = 0;

  items.forEach((node, index) => {
    const path = [...parent, index];

    if (node.kind === "folder") {
      const children = buildNodes(node.items, collection, path, query);
      if (query && children.count === 0) return;
      const details = document.createElement("details");
      details.open = true;
      details.dataset.key = keyFor(collection.slug, path);

      const summary = document.createElement("summary");
      const label = document.createElement("span");
      label.className = "folder-name";
      label.textContent = node.name;
      const folderAuth = authMode(node.auth);
      if (folderAuth !== "inherit") {
        label.title =
          folderAuth === "none"
            ? "Sends no auth; requests below inherit nothing"
            : `${folderAuth} auth set on this folder`;
      }
      summary.append(label);
      // Clicking the name opens the folder for editing; the caret still toggles.
      summary.addEventListener("click", (event) => {
        if (!event.target.closest(".folder-name")) return;
        event.preventDefault();
        openFolder(node, collection, path);
      });

      details.append(summary, children.fragment);
      fragment.append(details);
      count += 1;
    } else {
      if (query && !requestMatchesSearch(node, query)) return;
      const row = document.createElement("button");
      row.type = "button";
      row.className = "request-row";
      row.dataset.key = keyFor(collection.slug, path);

      const method = document.createElement("span");
      method.className = `method ${node.method.toLowerCase()}`;
      method.textContent = node.method;

      const name = document.createElement("span");
      name.className = "name";
      name.textContent = node.name;

      row.append(method, name);
      row.addEventListener("click", () => openRequest(node, collection, path));
      fragment.append(row);
      count += 1;
    }
  });

  return { fragment, count };
}

function renderTree() {
  const tree = $("tree");
  tree.replaceChildren();

  const workspace = state.workspace;
  if (!workspace) return;
  const query = state.treeSearch.trim().toLocaleLowerCase();

  if (!workspace.collections.length) {
    tree.innerHTML = '<p class="hint">No collections in this workspace.</p>';
    return;
  }

  for (const collection of workspace.collections) {
    const group = document.createElement("div");
    group.className = "collection-group";

    const name = document.createElement("button");
    name.type = "button";
    name.className = "collection-name";
    name.dataset.key = nodeKey(collection.slug, []);
    name.textContent = collection.name;
    name.title = collection.description || "Edit this collection";
    name.addEventListener("click", () => openCollection(collection));

    const children = buildNodes(collection.items, collection, [], query);
    if (query && children.count === 0) continue;
    group.append(name, children.fragment);
    tree.append(group);
  }

  if (query && !tree.childElementCount) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = "No requests match that name or URL.";
    tree.append(empty);
  }

  markSelected();
}

function markSelected() {
  const selection = state.selection;
  const key = selection ? keyFor(selection.slug, selection.path) : null;
  for (const element of $("tree").querySelectorAll("[data-key]")) {
    element.classList.toggle("selected", element.dataset.key === key);
  }
  markUnsaved();
}

function cacheActiveRequestTab() {
  const tab = state.requestTabs.find((item) => item.key === (state.activeRequestTab || state.activeTab));
  if (!tab) return;
  if (tab.kind !== "history" || state.response) tab.response = state.response;
  tab.query = $("query").value;
}

function renderRequestTabs() {
  const host = $("request-tabs");
  host.replaceChildren();
  for (const tab of state.requestTabs) {
    const entry = tab.kind === "history" ? null : drafts.get(draftKey(tab.kind, tab.slug, tab.path));
    const dirty = entry && JSON.stringify(entry.draft) !== entry.saved;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "request-tab";
    button.classList.toggle("active", tab.key === state.activeTab);
    button.setAttribute("role", "tab");
    button.setAttribute("aria-selected", String(tab.key === state.activeTab));
    button.title = tab.kind === "request" ? tab.url || tab.name : tab.kind;

    const method = document.createElement("span");
    if (tab.kind === "request") {
      method.className = `method ${String(tab.method || "GET").toLowerCase()}`;
      method.textContent = tab.method || "GET";
    } else {
      method.className = "tab-kind";
      method.textContent = tab.kind;
    }
    const name = document.createElement("span");
    name.className = "tab-name";
    name.textContent = tab.name;
    button.append(method, name);
    if (dirty) {
      const marker = document.createElement("span");
      marker.className = "tab-dirty";
      marker.textContent = "●";
      marker.title = "Unsaved changes";
      button.append(marker);
    }

    const close = document.createElement("span");
    close.className = "request-tab-close";
    close.setAttribute("role", "button");
    close.setAttribute("aria-label", `Close ${tab.name}`);
    close.textContent = "×";
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      closeRequestTab(tab.key);
    });
    button.append(close);
    button.addEventListener("click", () => activateRequestTab(tab.key));
    host.append(button);
  }
  host.hidden = state.requestTabs.length === 0;
}

function setHistoryViewer(active) {
  if (active) {
    $("saved-query-bar").hidden = true;
    $("query-note").hidden = true;
  }
  for (const element of document.querySelectorAll("#request-view > .url-bar, #request-view > .meta-bar, #request-view > .tabs, #request-view > .tab-body")) {
    element.hidden = active;
  }
  $("send").hidden = active;
}

async function refreshHistory() {
  const data = await invoke("list_history", { collection: null, path: null });
  $("history-limit").value = (data.limit / (1024 * 1024)).toString();
  $("history-count").textContent = data.entries.length ? String(data.entries.length) : "";
  const host = $("history-list");
  host.replaceChildren();
  const summary = document.createElement("div");
  summary.className = "history-summary";
  summary.textContent = `${bytes(data.bytes)} / ${data.limit === 0 ? "off" : bytes(data.limit)}`;
  host.append(summary);
  renderHistoryEntries(host, data.entries, "No request history yet.");
}

function renderHistoryEntries(host, entries, emptyMessage) {
  if (!entries.length) {
    const empty = document.createElement("p");
    empty.className = "hint";
    empty.textContent = emptyMessage;
    host.append(empty);
    return;
  }
  for (const item of entries) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "history-entry";
    button.title = `${item.method} ${item.url}`;
    const method = document.createElement("span");
    method.className = `method ${String(item.method).toLowerCase()}`;
    method.textContent = item.method;
    const name = document.createElement("span");
    name.className = "history-entry-name";
    name.textContent = item.name;
    const meta = document.createElement("span");
    meta.className = "history-entry-meta";
    const responseSize = item.error || item.response_available ? bytes(item.bytes) : "response removed";
    meta.textContent = `${item.error ? "error" : item.status} · ${new Date(item.timestamp * 1000).toLocaleString()} · ${responseSize}`;
    if (item.error) button.title += `\n${item.error}`;
    button.append(method, name, meta);
    button.addEventListener("click", () => openHistoryEntry(item));
    host.append(button);
  }
}

async function showRequestHistory() {
  const selection = state.selection;
  if (selection?.kind !== "request") return;
  const data = await invoke("list_history", {
    collection: selection.slug,
    path: selection.path,
  });
  $("request-history-title").textContent = `${state.draft?.name || selection.node.name} · History`;
  const host = $("request-history-list");
  host.replaceChildren();
  renderHistoryEntries(host, data.entries, "No history for this request yet.");
  $("request-history-dialog").showModal();
}

async function openHistoryEntry(item) {
  try {
    const response = await invoke("open_history", { id: item.id });
    const key = `history:${item.id}`;
    let tab = state.requestTabs.find((candidate) => candidate.key === key);
    if (!tab) {
      tab = { key, kind: "history", name: item.name, method: item.method, response, query: "" };
      state.requestTabs.push(tab);
    }
    tab.response = response;
    $("request-history-dialog").close();
    activateRequestTab(key);
  } catch (error) {
    window.alert(String(error));
    await refreshHistory();
  }
}

function updateSendButton() {
  const pending = state.pendingRequests.has(state.activeRequestTab);
  $("send").disabled = false;
  $("send").textContent = pending ? "Cancel" : "Send";
  $("send").classList.toggle("btn-cancel", pending);
}

function updateMethodSelectStyle() {
  const select = $("method");
  const method = select.value.toLowerCase();
  const trigger = $("method-trigger");
  trigger.classList.remove("get", "post", "put", "patch", "delete");
  if (["get", "post", "put", "patch", "delete"].includes(method)) trigger.classList.add(method);
  $("method-label").textContent = select.value;
  renderMethodMenu();
}

function renderMethodMenu() {
  const menu = $("method-menu");
  if (!menu) return;
  menu.replaceChildren();
  for (const option of $("method").options) {
    const value = option.value;
    const item = document.createElement("button");
    item.type = "button";
    item.className = "method-option";
    const colorClass = value.toLowerCase();
    if (["get", "post", "put", "patch", "delete"].includes(colorClass)) item.classList.add(colorClass);
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(value === $("method").value));
    item.textContent = value;
    item.addEventListener("click", () => {
      $("method").value = value;
      $("method").dispatchEvent(new Event("change", { bubbles: true }));
      closeMethodMenu();
    });
    menu.append(item);
  }
}

function closeMethodMenu() {
  $("method-menu").hidden = true;
  $("method-trigger").setAttribute("aria-expanded", "false");
}

function activateRequestTab(key) {
  cacheActiveRequestTab();
  const tab = state.requestTabs.find((item) => item.key === key);
  if (!tab) return;
  if (tab.kind === "history") {
    state.activeTab = key;
    state.activeRequestTab = null;
    state.selection = null;
    state.draft = null;
    state.response = tab.response;
    state.query = tab.query || "";
    $("empty-state").hidden = true;
    $("node-view").hidden = true;
    $("request-view").hidden = false;
    setHistoryViewer(true);
    $("query").value = tab.query || "";
    renderRequestTabs();
    if (tab.response.error) showRequestError(tab.response.error);
    else {
      showResponseMetadata(tab.response);
      void applyQuery();
    }
    return;
  }
  setHistoryViewer(false);
  const restored = findNode(tab.slug, tab.path);
  if (!restored || restored.kind !== tab.kind) {
    state.requestTabs = state.requestTabs.filter((item) => item.key !== key);
    renderRequestTabs();
    return;
  }

  state.activeTab = key;
  if (tab.kind === "request") {
    state.activeRequestTab = null;
    state.response = tab.response || null;
    $("query").value = tab.query || "";
    openRequest(restored.node, restored.collection, restored.path, true);
  } else if (tab.kind === "folder") {
    state.activeRequestTab = null;
    openFolder(restored.node, restored.collection, restored.path, true);
  } else {
    state.activeRequestTab = null;
    openCollection(restored.collection, true);
  }
  renderRequestTabs();
  if (tab.kind === "request" && tab.response) {
    showResponseMetadata(tab.response);
    applyQuery();
  } else if (tab.kind === "request" && tab.error) {
    if (tab.cancelled) showRequestCancelled();
    else showRequestError(tab.error);
  } else {
    resetResponse("Send the request to see a response.");
  }
}

function closeRequestTab(key) {
  const tab = state.requestTabs.find((item) => item.key === key);
  if (!tab) return;
  const entry = tab.kind === "history" ? null : drafts.get(draftKey(tab.kind, tab.slug, tab.path));
  if (entry && JSON.stringify(entry.draft) !== entry.saved &&
      !window.confirm(`Discard unsaved changes and close “${tab.name}”?`)) {
    return;
  }

  if (state.pendingRequests.has(key)) void cancelCurrentRequest(key);
  if (state.activeRequestTab === key) cacheActiveRequestTab();
  if (tab.kind !== "history") drafts.delete(draftKey(tab.kind, tab.slug, tab.path));
  const index = state.requestTabs.indexOf(tab);
  state.requestTabs.splice(index, 1);
  if (state.activeTab !== key) {
    renderRequestTabs();
    return;
  }
  state.activeRequestTab = null;
  state.activeTab = null;
  state.selection = null;
  state.draft = null;
  state.saved = "";
  state.response = null;
  renderRequestTabs();
  const next = state.requestTabs[Math.min(index, state.requestTabs.length - 1)];
  if (next) activateRequestTab(next.key);
  else {
    showEmpty();
    resetResponse("Send the request to see a response.");
  }
}

/* ---------- selecting ---------- */

function isDirty() {
  return Boolean(state.draft) && JSON.stringify(state.draft) !== state.saved;
}

function dropDraft(kind, slug, path) {
  drafts.delete(draftKey(kind, slug, path));
}

function markUnsaved() {
  for (const element of $("tree").querySelectorAll("[data-key]")) {
    const key = element.dataset.key;
    const dirty = ["request", "folder", "collection"].some((kind) => {
      const entry = drafts.get(`${kind}:${key}`);
      return entry && JSON.stringify(entry.draft) !== entry.saved;
    });
    element.classList.toggle("unsaved", Boolean(dirty));
  }
}

function openRequest(node, collection, path, keepResponse = false) {
  setHistoryViewer(false);
  hideVariable();
  cacheActiveRequestTab();
  const tabKey = requestTabKey(collection.slug, path);
  let tab = state.requestTabs.find((item) => item.key === tabKey);
  if (!tab) {
    tab = {
      key: tabKey,
      kind: "request",
      slug: collection.slug,
      path: [...path],
      name: node.name,
      method: node.method || "GET",
      url: node.url || "",
      response: null,
      query: "",
    };
    state.requestTabs.push(tab);
  }
  tab.name = node.name;
  tab.method = node.method || "GET";
  tab.url = node.url || "";
  state.activeRequestTab = tabKey;
  state.activeTab = tabKey;
  if (!keepResponse) {
    state.response = tab.response || null;
    $("query").value = tab.query || "";
  }
  state.selection = { kind: "request", slug: collection.slug, path, node, collection };
  $("request-history").hidden = false;
  const entry = draftFor("request", collection.slug, path, () => ({
    name: node.name,
    method: node.method || "GET",
    url: node.url || "",
    params: clone(node.params || []),
    path_params: clone(node.path_params || {}),
    headers: clone(node.headers || []),
    body: clone(node.body || null),
    auth: clone(node.auth ?? "inherit"),
    scripts: clone(node.scripts || { pre_request: "", post_response: "" }),
    queries: clone(node.queries || {}),
  }));
  state.draft = entry.draft;
  state.saved = entry.saved;
  tab.name = state.draft.name;
  tab.method = state.draft.method;
  tab.url = state.draft.url;
  $("query-name").value = "";
  $("query-note").hidden = true;
  renderSavedQueries();

  if (!keepResponse && tab.response) {
    showResponseMetadata(tab.response);
    applyQuery();
  } else if (!keepResponse && tab.error) {
    if (tab.cancelled) showRequestCancelled();
    else showRequestError(tab.error);
  } else if (!keepResponse) {
    resetResponse("Send the request to see a response.");
  }

  $("empty-state").hidden = true;
  $("node-view").hidden = true;
  $("request-view").hidden = false;

  $("method").value = state.draft.method;
  if ($("method").value !== state.draft.method) {
    const option = document.createElement("option");
    option.textContent = state.draft.method;
    $("method").append(option);
    $("method").value = state.draft.method;
  }
  updateMethodSelectStyle();
  $("url").value = displayedUrl(state.draft);
  paintVariables($("url"));
  $("request-name").value = state.draft.name;
  $("request-pre-script").value = state.draft.scripts?.pre_request || "";
  $("request-post-script").value = state.draft.scripts?.post_response || "";
  paintVariables($("request-pre-script"));
  paintVariables($("request-post-script"));

  renderParams();
  renderHeaders();
  renderBody();
  renderRequestAuth();
  updateAuthTag();
  updateDirty();
  markSelected();
  renderRequestTabs();
  updateSendButton();
  if (!keepResponse && tab.response) {
    showResponseMetadata(tab.response);
    applyQuery();
  }
  refreshPreview();
}

function editorTabKey(kind, slug, path) {
  return `${requestTabKey(slug, path)}|${kind}`;
}

function ensureEditorTab(kind, collection, path, key = editorTabKey(kind, collection.slug, path)) {
  let tab = state.requestTabs.find((item) => item.key === key);
  if (!tab) {
    tab = { key, kind, slug: collection.slug, path: [...path], name: kind === "collection" ? collection.name : collection.items?.[path[path.length - 1]]?.name };
    state.requestTabs.push(tab);
  }
  tab.name = kind === "collection" ? collection.name : findNode(collection.slug, path)?.node?.name || tab.name;
  return tab;
}

function draftEffectiveAuth() {
  const own = authMode(state.draft?.auth ?? state.selection?.node?.auth);
  if (own === "none") return null;
  if (own !== "inherit") return own;
  return state.selection?.node?.inherited || null;
}

function updateAuthTag() {
  const node = state.selection?.node;
  if (!node) return;

  const own = authMode(state.draft?.auth ?? node.auth);
  const effective = draftEffectiveAuth();
  const tag = $("auth");
  tag.textContent = effective ? `${effective} auth` : "no auth";
  tag.title =
    own === "inherit"
      ? effective
        ? `${node.inherited} auth, inherited from the collection or a folder`
        : "Nothing above this request sets auth"
      : "Set on this request";
}

function renderRequestAuth() {
  const node = state.selection?.node;
  if (!node) return;

  renderAuth(
    $("request-auth"),
    state.draft.auth,
    node.inherited ?? null,
    (next, structural) => {
      state.draft.auth = next;
      updateDirty();
      schedulePreview();
      updateAuthTag();
      if (structural) renderRequestAuth();
    }
  );
}

function openFolder(node, collection, path, keepResponse = false) {
  setHistoryViewer(false);
  hideVariable();
  cacheActiveRequestTab();
  state.activeRequestTab = null;
  const tabKey = editorTabKey("folder", collection.slug, path);
  ensureEditorTab("folder", collection, path, tabKey);
  state.activeTab = tabKey;
  renderRequestTabs();
  state.selection = { kind: "folder", slug: collection.slug, path, node, collection };
  const entry = draftFor("folder", collection.slug, path, () => ({
    name: node.name,
    auth: clone(node.auth ?? "inherit"),
    scripts: clone(node.scripts || { pre_request: "", post_response: "" }),
  }));
  state.draft = entry.draft;
  state.saved = entry.saved;
  const tab = state.requestTabs.find((item) => item.key === tabKey);
  if (tab) tab.name = state.draft.name;

  showNode("folder");
  $("node-name").value = state.draft.name;
  $("collection-fields").hidden = true;
  $("node-count").textContent = `${countRequests(node.items)} requests`;
  paintNodeAuth(node.inherited ?? null);
  renderNodeScripts();
  markSelected();
  updateDirty();
  refreshPreview();
}

function openCollection(collection, keepResponse = false) {
  setHistoryViewer(false);
  hideVariable();
  cacheActiveRequestTab();
  state.activeRequestTab = null;
  const tabKey = editorTabKey("collection", collection.slug, []);
  ensureEditorTab("collection", collection, [], tabKey);
  state.activeTab = tabKey;
  renderRequestTabs();
  state.selection = {
    kind: "collection",
    slug: collection.slug,
    path: [],
    node: collection,
    collection,
  };

  const entry = draftFor("collection", collection.slug, [], () => ({
    name: collection.name,
    description: collection.description || "",
    auth: clone(collection.auth ?? "inherit"),
    scripts: clone(collection.scripts || { pre_request: "", post_response: "" }),
    variables: (collection.variables || []).map((variable) => ({
      name: variable.name,
      // A secret's value never arrives; null means "keep what is stored".
      value: variable.secret ? null : variable.value ?? "",
      secret: Boolean(variable.secret),
      stored: Boolean(variable.has_value),
    })),
  }));
  state.draft = entry.draft;
  state.saved = entry.saved;

  showNode("collection");
  $("node-name").value = state.draft.name;
  $("node-description").value = state.draft.description || "";
  $("collection-fields").hidden = false;
  $("node-count").textContent = `${countRequests(collection.items)} requests`;
  renderVariables();
  paintNodeAuth(null);
  renderNodeScripts();
  markSelected();
  updateDirty();
  refreshPreview();
}

function paintNodeAuth(inherited) {
  renderAuth($("node-auth"), state.draft.auth, inherited, (next, structural) => {
    state.draft.auth = next;
    updateDirty();
    if (structural) paintNodeAuth(inherited);
  });
}

function renderNodeScripts() {
  $("node-pre-script").value = state.draft?.scripts?.pre_request || "";
  $("node-post-script").value = state.draft?.scripts?.post_response || "";
  paintVariables($("node-pre-script"));
  paintVariables($("node-post-script"));
}
function showNode(kind) {
  $("empty-state").hidden = true;
  $("request-view").hidden = true;
  $("node-view").hidden = false;
  $("node-kind").textContent = kind;
  $("node-dirty").hidden = true;
  $("node-note").textContent = "";
  $("node-save").disabled = true;
}

function countRequests(items) {
  return items.reduce(
    (total, item) =>
      total + (item.kind === "folder" ? countRequests(item.items) : 1),
    0
  );
}

/* ---------- request editor ---------- */

function renderParams() {
  renderPathParams();

  const rows = $("params");
  rows.replaceChildren();
  (state.draft.params || []).forEach((param, index) => {
    addParamRow(rows, param, index);
  });
  updateParamsCount();
}

/// Path values are derived from the URL, never stored as rows: the `:name`
/// segment *is* the row, and its value lives in `path_params`.
function renderPathParams() {
  const rows = $("path-params");
  rows.replaceChildren();
  const tokens = pathTokens(state.draft.url);

  for (const name of tokens) {
    const row = document.createElement("div");
    row.className = "row row-path";

    const token = document.createElement("span");
    token.className = "cell cell-name path-token";
    token.textContent = `:${name}`;
    token.title = "A whole path segment in the URL";

    const value = document.createElement("input");
    value.className = "cell cell-value";
    value.placeholder = "value";
    value.spellcheck = false;
    value.value = state.draft.path_params?.[name] ?? "";
    value.addEventListener("input", () => {
      state.draft.path_params = state.draft.path_params || {};
      state.draft.path_params[name] = value.value;
      updateDirty();
      schedulePreview();
    });

    const remove = iconButton("×", "Remove this segment from the URL");
    remove.addEventListener("click", () => removePathToken(name));

    row.append(token, value, remove);
    withVariables(value);
    rows.append(row);
  }
}

function removePathToken(name) {
  state.draft.url = state.draft.url
    .split("/")
    .filter((segment) => segment !== `:${name}`)
    .join("/");
  delete state.draft.path_params?.[name];
  updateUrlDisplay();
  renderParams();
  updateDirty();
  schedulePreview();
}

function addParamRow(rows, param, index) {
  const row = document.createElement("div");
  row.className = "row";
  row.classList.toggle("row-off", Boolean(param.disabled));

  const enabled = document.createElement("input");
  enabled.type = "checkbox";
  enabled.className = "cell-check";
  enabled.title = "Send this parameter";
  enabled.checked = !param.disabled;
  enabled.addEventListener("change", () => {
    param.disabled = !enabled.checked;
    row.classList.toggle("row-off", param.disabled);
    updateParamsCount();
    updateDirty();
    updateUrlDisplay();
    schedulePreview();
  });

  const name = document.createElement("input");
  name.className = "cell cell-name";
  name.placeholder = "name";
  name.spellcheck = false;
  name.value = param.name;

  const value = document.createElement("input");
  value.className = "cell cell-value";
  value.placeholder = "value";
  value.spellcheck = false;
  value.value = param.value;

  const remove = iconButton("×", "Remove this parameter");
  remove.addEventListener("click", () => {
    state.draft.params.splice(index, 1);
    renderParams();
    updateDirty();
    updateUrlDisplay();
    schedulePreview();
  });

  name.addEventListener("input", () => {
    param.name = name.value;
    updateParamsCount();
    updateDirty();
    updateUrlDisplay();
    schedulePreview();
  });
  value.addEventListener("input", () => {
    param.value = value.value;
    updateParamsCount();
    updateDirty();
    updateUrlDisplay();
    schedulePreview();
  });

  row.append(enabled, name, value, remove);
  withVariables(name);
  withVariables(value);
  rows.append(row);
}

function updateParamsCount() {
  const params = state.draft.params || [];
  const on = params.filter(
    (param) => !param.disabled && (param.name || param.value)
  ).length;
  $("param-count").textContent = on ? String(on) : "";
}

function renderHeaders() {
  const rows = $("headers");
  rows.replaceChildren();

  state.draft.headers.forEach((header, index) => {
    const row = document.createElement("div");
    row.className = "row";

    const enabled = document.createElement("input");
    enabled.type = "checkbox";
    enabled.className = "cell-check";
    enabled.title = "Send this header";
    enabled.checked = !header.disabled;
    enabled.addEventListener("change", () => {
      header.disabled = !enabled.checked;
      updateHeadersCount();
      updateDirty();
      schedulePreview();
    });

    const name = document.createElement("input");
    name.className = "cell cell-name";
    name.placeholder = "name";
    name.spellcheck = false;
    name.value = header.name;

    const value = document.createElement("input");
    value.className = "cell cell-value";
    value.placeholder = "value";
    value.spellcheck = false;
    value.value = header.value;

    const remove = iconButton("×", "Remove this header");
    remove.addEventListener("click", () => {
      state.draft.headers.splice(index, 1);
      renderHeaders();
      updateDirty();
      schedulePreview();
    });

    const sync = () => {
      header.name = name.value;
      header.value = value.value;
      updateDirty();
      schedulePreview();
    };
    name.addEventListener("input", sync);
    value.addEventListener("input", sync);

    row.append(enabled, name, value, remove);
    withVariables(name);
    withVariables(value);
    rows.append(row);
  });

  updateHeadersCount();
}

function updateHeadersCount() {
  const count = state.draft.headers.filter((header) => !header.disabled).length;
  $("header-count").textContent = count ? String(count) : "";
}

function renderBody() {
  const body = state.draft.body;
  $("body-text").value = body?.text || "";
  $("body-language").value = body?.language || "json";
  if (body?.language && $("body-language").value !== body.language) {
    const option = document.createElement("option");
    option.textContent = body.language;
    $("body-language").append(option);
    $("body-language").value = body.language;
  }
  paintVariables($("body-text"));
}

function readBody() {
  const text = $("body-text").value;
  const language = $("body-language").value || "json";
  state.draft.body = text
    ? { mode: state.draft.body?.mode || "raw", language, text }
    : null;
  updateDirty();
  schedulePreview();
}

function renderVariables() {
  const rows = $("variables");
  rows.replaceChildren();

  state.draft.variables.forEach((variable, index) => {
    const row = document.createElement("div");
    row.className = "row row-variable";

    const name = document.createElement("input");
    name.className = "cell cell-name";
    name.placeholder = "name";
    name.spellcheck = false;
    name.value = variable.name;

    const value = document.createElement("input");
    value.className = "cell cell-value";
    value.spellcheck = false;
    value.value = variable.value ?? "";
    if (variable.secret) {
      value.placeholder = variable.stored
        ? "•••••• stored — type to replace"
        : "not set";
      value.title = "Secret values live in .secrets/ and are never shown here";
    } else {
      value.placeholder = "value";
    }

    const secret = document.createElement("input");
    secret.type = "checkbox";
    secret.className = "cell-check";
    secret.title = "Keep this value out of git";
    secret.checked = variable.secret;

    const remove = iconButton("×", "Remove this variable");
    remove.addEventListener("click", () => {
      state.draft.variables.splice(index, 1);
      renderVariables();
      updateDirty();
    });

    name.addEventListener("input", () => {
      variable.name = name.value;
      updateDirty();
    });
    value.addEventListener("input", () => {
      variable.value = value.value;
      updateDirty();
    });
    secret.addEventListener("change", () => {
      variable.secret = secret.checked;
      if (!secret.checked && variable.value === null) variable.value = "";
      updateDirty();
    });

    row.append(name, value, secret, remove);
    rows.append(row);
  });
}

function iconButton(text, title) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "icon-btn row-remove";
  button.textContent = text;
  button.title = title;
  return button;
}

/* ---------- dirty state and saving ---------- */

function updateDirty() {
  const dirty = isDirty();

  if (state.selection?.kind === "request") {
    $("dirty").hidden = !dirty;
    $("save").disabled = !dirty;
    if (!dirty) {
      $("save-note").textContent = "";
      $("save-note").classList.remove("error-text");
    }
  } else {
    $("node-dirty").hidden = !dirty;
    $("node-save").disabled = !dirty;
    if (!dirty) {
      $("node-note").textContent = "";
      $("node-note").classList.remove("error-text");
    }
  }

  markUnsaved();
  const tab = state.requestTabs.find((item) => item.key === state.activeTab);
  if (tab && state.draft?.name) tab.name = state.draft.name;
  renderRequestTabs();
}

function draftForIpc() {
  const draft = state.draft;
  return {
    name: draft.name,
    method: draft.method,
    url: draft.url,
    params: draft.params || [],
    path_params: draft.path_params || {},
    headers: draft.headers,
    body:
      draft.body && draft.body.text
        ? {
            mode: draft.body.mode || "raw",
            language: draft.body.language || "json",
            text: draft.body.text,
          }
        : null,
    auth: draft.auth ?? "inherit",
    scripts: draft.scripts || { pre_request: "", post_response: "" },
    queries: draft.queries || {},
  };
}

function editForIpc() {
  const selection = state.selection;

  if (selection.kind === "request") {
    return { kind: "request", path: selection.path, draft: draftForIpc() };
  }
  if (selection.kind === "folder") {
    return {
      kind: "folder",
      path: selection.path,
      name: state.draft.name,
      auth: state.draft.auth ?? "inherit",
      scripts: state.draft.scripts || { pre_request: "", post_response: "" },
    };
  }
  return {
    kind: "collection",
    name: state.draft.name,
    description: state.draft.description.trim() || null,
    variables: state.draft.variables.map((variable) => ({
      name: variable.name,
      // null on a secret means "leave the stored value alone".
      value: variable.secret ? variable.value : variable.value ?? "",
      secret: variable.secret,
    })),
    auth: state.draft.auth ?? "inherit",
    scripts: state.draft.scripts || { pre_request: "", post_response: "" },
  };
}

function findNode(slug, path) {
  const collection = (state.workspace?.collections || []).find(
    (candidate) => candidate.slug === slug
  );
  if (!collection) return null;
  if (!path.length) return { kind: "collection", node: collection, collection, path: [] };

  let items = collection.items;
  let node = null;
  for (let depth = 0; depth < path.length; depth += 1) {
    node = items?.[path[depth]];
    if (!node) return null;
    if (depth < path.length - 1) items = node.items;
  }
  return { kind: node.kind, node, collection, path };
}

async function save() {
  if (!state.draft || !state.selection || !isDirty()) return;

  const selection = state.selection;
  const kind = selection.kind;
  const path = [...selection.path];

  $("save").disabled = true;
  $("node-save").disabled = true;

  try {
    const result = await invoke("save", {
      collection: selection.slug,
      edit: editForIpc(),
    });

    dropDraft(kind, selection.slug, path);
    // A collection rename changes every node's slug, so no stashed draft can be
    // matched to a node any more.
    if (result.collection !== selection.slug) drafts.clear();

    if (result.collection !== selection.slug) {
      const activeEditorTab = state.requestTabs.find((tab) => tab.key === state.activeTab);
      for (const tab of state.requestTabs) {
        if (tab.slug !== selection.slug) continue;
        tab.slug = result.collection;
        tab.key = tab.kind === "request"
          ? requestTabKey(tab.slug, tab.path)
          : editorTabKey(tab.kind, tab.slug, tab.path);
      }
      if (state.activeRequestTab === requestTabKey(selection.slug, path)) {
        state.activeRequestTab = requestTabKey(result.collection, path);
      }
      if (activeEditorTab) state.activeTab = activeEditorTab.key;
    }

    state.workspace = result.workspace;
    renderWorkspace();

    const restored = findNode(result.collection, path);
    if (!restored) {
      state.selection = null;
      state.draft = null;
      showEmpty();
      return;
    }

    if (restored.kind === "request") {
      openRequest(restored.node, restored.collection, restored.path, true);
    } else if (restored.kind === "folder") {
      openFolder(restored.node, restored.collection, restored.path, true);
    } else {
      openCollection(restored.node, true);
    }

    const touched = result.written + result.removed;
    const note = touched
      ? `saved · ${touched} ${touched === 1 ? "file" : "files"}`
      : "saved";
    if (kind === "request") $("save-note").textContent = note;
    else $("node-note").textContent = note;
  } catch (error) {
    if (kind === "request") {
      $("save-note").textContent = String(error);
      $("save-note").classList.add("error-text");
      $("dirty").hidden = false;
      $("save").disabled = false;
    } else {
      $("node-note").textContent = String(error);
      $("node-note").classList.add("error-text");
      $("node-dirty").hidden = false;
      $("node-save").disabled = false;
    }
  }
}

/* ---------- command preview ---------- */

function setCurlCopyEnabled(enabled) {
  $("copy-curl").disabled = !enabled;
  $("copy-curl-secrets").disabled = !enabled;
}

function renderCurlPreview(text) {
  const host = $("curl");
  host.replaceChildren();
  for (const part of syntaxParts(text, "shell")) {
    const token = document.createElement("span");
    token.className = part.kind;
    token.textContent = part.text;
    host.append(token);
  }
}

async function refreshPreview() {
  clearTimeout(state.previewTimer);

  if (!state.selection) {
    state.preview = "";
    $("curl").textContent = "Select a request.";
    setCurlCopyEnabled(false);
    $("preview-masked").hidden = true;
    $("preview-problem").hidden = true;
    $("summary").replaceChildren();
    return;
  }

  if (state.selection.kind !== "request") {
    state.preview = "";
    $("curl").textContent =
      state.selection.kind === "collection"
        ? "This is a collection. Pick a request to see its command."
        : "This is a folder. Pick a request to see its command.";
    setCurlCopyEnabled(false);
    $("preview-masked").hidden = true;
    $("preview-problem").hidden = true;
    renderSummary();
    return;
  }

  try {
    const preview = await invoke("curl_preview", {
      collection: state.selection.slug,
      path: state.selection.path,
      draft: draftForIpc(),
      environment: state.env || null,
    });
    state.preview = preview.text;
    renderCurlPreview(preview.text);
    setCurlCopyEnabled(true);
    $("preview-masked").hidden = !preview.masked;
    $("preview-problem").hidden = !preview.problem;
    $("preview-problem").textContent = preview.problem || "";
  } catch (error) {
    state.preview = "";
    $("curl").textContent = String(error);
    setCurlCopyEnabled(false);
    $("preview-masked").hidden = true;
    $("preview-problem").hidden = true;
  }

  renderSummary();
}

function schedulePreview() {
  clearTimeout(state.previewTimer);
  state.previewTimer = setTimeout(refreshPreview, 180);
}

function renderSummary() {
  const summary = $("summary");
  summary.replaceChildren();
  const selection = state.selection;
  if (!selection) return;

  const add = (term, value) => {
    const dt = document.createElement("dt");
    dt.textContent = term;
    const dd = document.createElement("dd");
    dd.textContent = value;
    summary.append(dt, dd);
  };

  if (selection.kind === "request") {
    const draft = state.draft;
    const params = draft.params || [];
    const on = params.filter(
      (param) => !param.disabled && (param.name || param.value)
    ).length;
    const paths = pathTokens(draft.url).length;
    const headers = (draft.headers || []).filter((header) => !header.disabled);
    add("method", draft.method);
    add("params", params.length > on ? `${on} of ${params.length} sent` : String(on));
    if (paths) add("path", String(paths));
    add("headers", String(headers.length));
    add("auth", draftEffectiveAuth() || "none");
    add("body", draft.body?.text ? bytes(new TextEncoder().encode(draft.body.text).length) : "none");
  } else if (selection.kind === "collection") {
    add("requests", String(countRequests(selection.node.items)));
    add("variables", String(state.draft.variables.length));
    add("auth", authMode(selection.node.auth));
  } else {
    add("requests", String(countRequests(selection.node.items)));
    add(
      "auth",
      authMode(selection.node.auth) === "inherit"
        ? selection.node.inherited
          ? `inherits ${selection.node.inherited}`
          : "none"
        : authMode(selection.node.auth)
    );
  }
}

async function copyCurl(includeSecrets = false) {
  if (!state.preview || state.selection?.kind !== "request") return;
  if (includeSecrets && !window.confirm(
    "Copy cURL with actual secret values? Anyone with clipboard access, including clipboard history or sync, may be able to read them. Paste only into a trusted location."
  )) return;

  setCurlCopyEnabled(false);
  try {
    await invoke("copy_curl", {
      collection: state.selection.slug,
      path: state.selection.path,
      draft: draftForIpc(),
      environment: state.env || null,
      includeSecrets,
    });
    const message = includeSecrets ? "copied with secrets" : "copied protected";
    $("copy-note").textContent = message;
    setTimeout(() => {
      if ($("copy-note").textContent === message) $("copy-note").textContent = "";
    }, 1500);
  } catch (error) {
    $("copy-note").textContent = String(error);
  } finally {
    setCurlCopyEnabled(Boolean(state.preview) && state.selection?.kind === "request");
  }
}

/* ---------- sending ---------- */

async function send() {
  if (!state.selection || state.selection.kind !== "request") return;

  const tabKey = state.activeRequestTab;
  if (state.pendingRequests.has(tabKey)) {
    await cancelCurrentRequest(tabKey);
    return;
  }
  const tab = state.requestTabs.find((item) => item.key === tabKey);
  const requestId = `${tabKey}:${Date.now()}:${Math.random()}`;
  state.pendingRequests.add(tabKey);
  state.cancelRequests.set(tabKey, requestId);
  if (tab) {
    tab.error = null;
    tab.cancelled = false;
  }
  state.response = null;
  if (tab) tab.response = null;
  resetResponse("Waiting for response…");
  updateSendButton();

  try {
    const response = await invoke("send", {
      collection: state.selection.slug,
      path: state.selection.path,
      draft: draftForIpc(),
      environment: state.env || null,
      requestId,
    });
    if (tab && state.requestTabs.includes(tab)) {
      tab.response = response;
      tab.error = null;
      tab.cancelled = false;
    }
    try {
      state.workspace = await invoke("refresh_workspace");
      renderWorkspace();
      if (state.selection?.kind === "request") {
        const current = findNode(state.selection.slug, state.selection.path);
        if (current) {
          state.selection.node = current.node;
          state.selection.collection = current.collection;
        }
      }
      repaintVariables();
      refreshPreview();
    } catch {
      // The request response remains usable if reloading the saved workspace fails.
    }
    if (state.activeRequestTab === tabKey) {
      state.response = response;
      showResponseMetadata(response);
      await applyQuery();
    }
    if ($("history-section").open) void refreshHistory();
  } catch (error) {
    const message = String(error);
    if (tab && state.requestTabs.includes(tab)) {
      tab.response = null;
      tab.error = message;
      tab.cancelled = message === "Request cancelled";
    }
    if (state.activeRequestTab === tabKey) {
      state.response = null;
      if (message === "Request cancelled") showRequestCancelled();
      else showRequestError(message);
    }
  } finally {
    state.pendingRequests.delete(tabKey);
    if (state.cancelRequests.get(tabKey) === requestId) state.cancelRequests.delete(tabKey);
    updateSendButton();
  }
}

async function cancelCurrentRequest(tabKey = state.activeRequestTab) {
  const requestId = state.cancelRequests.get(tabKey);
  if (!requestId) return;
  try {
    await invoke("cancel_send", { requestId });
  } catch (error) {
    const tab = state.requestTabs.find((item) => item.key === tabKey);
    if (tab) tab.error = String(error);
  }
}

function showRequestError(message) {
  state.response = null;
  renderResponseEnvironment();
  $("status").textContent = "error";
  $("status").className = "status status-error";
  $("timing").textContent = "";
  $("sizes").textContent = "";
  $("tls").hidden = true;
  $("resolved").hidden = true;
  $("missing-vars").hidden = true;
  $("query-error").hidden = false;
  $("query-error").textContent = message;
  $("response-body").textContent = "Request failed.";
  $("response-body").scrollTop = 0;
}

function showRequestCancelled() {
  state.response = null;
  renderResponseEnvironment();
  $("status").textContent = "cancelled";
  $("status").className = "status status-warn";
  $("timing").textContent = "";
  $("sizes").textContent = "";
  $("tls").hidden = true;
  $("resolved").hidden = true;
  $("missing-vars").hidden = true;
  $("query-error").hidden = true;
  $("response-body").textContent = "Request cancelled.";
  $("response-body").scrollTop = 0;
}

function renderResponseText(text) {
  const host = $("response-body");
  host._jsonModel = null;
  host.scrollTop = 0;
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    const lines = text.split("\n").length;
    const content = document.createElement("div");
    content.className = "response-text";
    content.style.setProperty("--line-number-width", `${String(lines).length + 2}ch`);
    content.dataset.lineNumbers = Array.from({ length: lines }, (_, index) => index + 1).join("\n");
    if (/^\s*[\[{]/.test(text)) renderJsonTokens(content, text);
    else content.textContent = text;
    host.replaceChildren(content);
    return;
  }

  const model = createJsonModel(value);
  if (model.lineCount >= 5000) {
    renderVirtualJson(host, model);
    return;
  }

  host.replaceChildren();
  model.nodes = new Map();
  model.nodes.model = model;
  model.host = host;
  model.isVirtual = false;
  createJsonToolbar(model);
  host.append(model.navigation);
  const tree = document.createElement("div");
  tree.className = "json-tree";
  tree.style.setProperty("--line-number-width", `${String(model.lineCount).length + 2}ch`);
  host.append(tree);
  appendJsonEntry(tree, null, value, [], false, model.nodes);
  updateJsonNavigation(model.navigation, [], model.nodes);
}

function createJsonToolbar(model) {
  const navigation = document.createElement("div");
  navigation.className = "json-navigation";
  model.navigation = navigation;
  model.breadcrumbs = document.createElement("div");
  model.breadcrumbs.className = "json-breadcrumbs";
  navigation.append(model.breadcrumbs);

  const find = document.createElement("div");
  find.className = "json-find";
  model.searchInput = document.createElement("input");
  model.searchInput.className = "json-search-input";
  model.searchInput.type = "search";
  model.searchInput.placeholder = "Find in JSON";
  model.searchInput.setAttribute("aria-label", "Find in full JSON response");
  model.searchCount = document.createElement("span");
  model.searchCount.className = "json-search-count";
  model.searchCount.textContent = "0/0";
  const previous = document.createElement("button");
  previous.type = "button";
  previous.className = "json-find-button";
  previous.textContent = "↑";
  previous.title = "Previous match";
  previous.addEventListener("click", () => stepJsonSearch(model, -1));
  const next = document.createElement("button");
  next.type = "button";
  next.className = "json-find-button";
  next.textContent = "↓";
  next.title = "Next match";
  next.addEventListener("click", () => stepJsonSearch(model, 1));
  model.searchInput.addEventListener("input", () => {
    clearTimeout(model.searchTimer);
    model.searchTimer = setTimeout(() => searchJsonModel(model, model.searchInput.value), 160);
  });
  model.searchInput.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    if (model.searchInput.value.toLocaleLowerCase() !== model.searchQuery) {
      searchJsonModel(model, model.searchInput.value);
    } else {
      stepJsonSearch(model, event.shiftKey ? -1 : 1);
    }
  });
  find.append(model.searchInput, model.searchCount, previous, next);
  navigation.append(find);
}

function searchJsonModel(model, text) {
  const query = text.trim().toLocaleLowerCase();
  model.searchQuery = query;
  model.matches = [];
  model.matchIndex = -1;
  if (query) {
    const seen = new Set();
    const add = (path) => {
      const encoded = JSON.stringify(path);
      if (!seen.has(encoded)) {
        seen.add(encoded);
        model.matches.push(encoded);
      }
    };
    const visit = (value, path) => {
      if (value !== null && typeof value === "object") {
        for (const [key, child] of model.children.get(value)) {
          const childPath = [...path, key];
          if (String(key).toLocaleLowerCase().includes(query)) add(childPath);
          visit(child, childPath);
        }
      } else if (String(value).toLocaleLowerCase().includes(query)) {
        add(path);
      }
    };
    visit(model.value, []);
  }
  model.searchCount.textContent = model.matches.length ? `1/${model.matches.length}` : "0/0";
  if (model.matches.length) {
    model.matchIndex = 0;
    focusJsonMatch(model, model.matches[0]);
  } else {
    model.currentMatch = null;
    if (model.isVirtual) renderVirtualWindow(model);
    else clearJsonMatchHighlight(model);
  }
}

function stepJsonSearch(model, direction) {
  if (!model.matches?.length) return;
  model.matchIndex = (model.matchIndex + direction + model.matches.length) % model.matches.length;
  model.searchCount.textContent = `${model.matchIndex + 1}/${model.matches.length}`;
  focusJsonMatch(model, model.matches[model.matchIndex]);
}

function focusJsonMatch(model, encodedPath) {
  const path = JSON.parse(encodedPath);
  const parentPath = path.slice(0, -1);
  model.currentMatch = encodedPath;
  if (model.isVirtual) {
    navigateVirtualJson(model, parentPath);
    const target = virtualLineIndex(model, path);
    model.host.scrollTop = model.tree.offsetTop + Math.max(0, target - 5) * model.lineHeight;
    renderVirtualWindow(model);
    return;
  }
  navigateJson(parentPath, model.navigation, model.nodes);
  clearJsonMatchHighlight(model);
  const target = [...model.host.querySelectorAll("[data-json-path]")]
    .find((element) => element.dataset.jsonPath === encodedPath);
  target?.classList.add("json-search-current");
  target?.scrollIntoView({ block: "nearest" });
}

function clearJsonMatchHighlight(model) {
  for (const element of model.host.querySelectorAll(".json-search-current")) {
    element.classList.remove("json-search-current");
  }
}

function createJsonModel(value) {
  const parents = new WeakMap();
  const baseCounts = new WeakMap();
  const visibleCounts = new WeakMap();
  const children = new WeakMap();
  const offsets = new WeakMap();
  const collapsed = new WeakSet();

  function visit(node, parent = null, key = null, childIndex = null) {
    if (node === null || typeof node !== "object") return 1;
    if (parent) parents.set(node, { parent, key, childIndex });
    const entries = Array.isArray(node)
      ? node.map((value, index) => [index, value])
      : Object.entries(node);
    children.set(node, entries);
    const prefix = [0];
    let count = 2;
    for (let index = 0; index < entries.length; index += 1) {
      const [childKey, child] = entries[index];
      const childCount = visit(child, node, childKey, index);
      count += childCount;
      prefix.push(prefix[index] + childCount);
    }
    offsets.set(node, prefix);
    baseCounts.set(node, count);
    visibleCounts.set(node, count);
    return count;
  }

  const lineCount = visit(value);
  return { value, parents, baseCounts, visibleCounts, children, offsets, collapsed, lineCount };
}

function renderVirtualJson(host, model) {
  host.replaceChildren();
  model.host = host;
  model.isVirtual = true;
  createJsonToolbar(model);
  model.tree = document.createElement("div");
  model.tree.className = "json-virtual-tree";
  model.tree.style.setProperty("--line-number-width", `${String(model.lineCount).length + 2}ch`);
  host.append(model.navigation, model.tree);
  model.lineHeight = parseFloat(getComputedStyle(host).lineHeight) || 18.6;
  model.selected = model.value && typeof model.value === "object" ? model.value : null;
  host._jsonModel = model;
  if (!host._jsonScrollHandler) {
    host._jsonScrollHandler = () => {
      if (host._jsonScrollFrame) return;
      host._jsonScrollFrame = requestAnimationFrame(() => {
        host._jsonScrollFrame = 0;
        if (host._jsonModel) renderVirtualWindow(host._jsonModel);
      });
    };
    host.addEventListener("scroll", host._jsonScrollHandler, { passive: true });
  }
  updateVirtualNavigation(model);
  renderVirtualWindow(model);
}

function virtualVisibleCount(model, node) {
  if (node === null || typeof node !== "object") return 1;
  return model.visibleCounts.get(node) || 1;
}

function renderVirtualWindow(model) {
  const { host, tree, lineHeight, lineCount } = model;
  tree.style.height = `${lineCount * lineHeight}px`;
  tree.replaceChildren();
  const firstVisible = Math.max(0, Math.floor((host.scrollTop - tree.offsetTop) / lineHeight));
  const start = Math.max(0, firstVisible - 32);
  const end = Math.min(lineCount, firstVisible + Math.ceil(host.clientHeight / lineHeight) + 32);
  const rows = [];

  function collect(node, key, depth, path, index, comma) {
    const count = virtualVisibleCount(model, node);
    if (index >= end || index + count <= start) return;
    if (node === null || typeof node !== "object") {
      rows.push({ kind: "value", node, key, depth, path, index, comma });
      return;
    }

    const isArray = Array.isArray(node);
    const items = model.children.get(node);
    rows.push({ kind: "open", node, key, depth, path, index, comma, isArray, itemCount: items.length });
    if (model.collapsed.has(node)) return;

    const prefix = model.offsets.get(node);
    const relativeStart = Math.max(0, start - (index + 1));
    let firstChild = 0;
    let lastChild = items.length;
    while (firstChild < lastChild) {
      const middle = (firstChild + lastChild) >>> 1;
      if (prefix[middle + 1] <= relativeStart) firstChild = middle + 1;
      else lastChild = middle;
    }
    for (let childIndex = firstChild; childIndex < items.length; childIndex += 1) {
      const [childKey, child] = items[childIndex];
      const childIndexInView = index + 1 + prefix[childIndex];
      if (childIndexInView >= end) break;
      collect(child, childKey, depth + 1, [...path, childKey], childIndexInView,
        childIndex < items.length - 1);
    }
    const closeIndex = index + 1 + prefix[items.length];
    if (closeIndex >= start && closeIndex < end) {
      rows.push({ kind: "close", node, depth, index: closeIndex, comma, isArray });
    }
  }

  collect(model.value, null, 0, [], 0, false);
  const fragment = document.createDocumentFragment();
  for (const row of rows) {
    const element = document.createElement("div");
    element.className = `json-virtual-row json-depth-${Math.min(row.depth, 12)}`;
    element.style.top = `${row.index * lineHeight}px`;
    element.style.paddingLeft = `calc(var(--line-number-width) + ${row.depth * 18}px)`;
    element.dataset.lineNumber = row.index + 1;
    element.dataset.jsonPath = JSON.stringify(row.path || []);
    if (model.currentMatch === element.dataset.jsonPath) element.classList.add("json-search-current");
    if (row.kind === "open") {
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "json-virtual-toggle";
      toggle.textContent = model.collapsed.has(row.node) ? "▸" : "▾";
      toggle.setAttribute("aria-expanded", String(!model.collapsed.has(row.node)));
      toggle.title = model.collapsed.has(row.node) ? "Expand" : "Collapse";
      toggle.addEventListener("click", () => toggleJsonNode(model, row.node));
      element.append(toggle);
      if (row.key !== null) appendVirtualKey(element, row.key);
      element.append(jsonPunctuation(row.key !== null ? (typeof row.key === "number" ? " " : ": ") : ""));
      element.append(jsonPunctuation(row.isArray ? "[" : "{"));
      if (model.collapsed.has(row.node)) {
        const count = document.createElement("span");
        count.className = "json-collapsed";
        count.textContent = ` … ${row.itemCount} `;
        element.append(count, jsonPunctuation(row.isArray ? "]" : "}"));
        if (row.comma) element.append(jsonPunctuation(","));
      }
    } else if (row.kind === "close") {
      const spacer = document.createElement("span");
      spacer.className = "json-virtual-spacer";
      element.append(spacer);
      element.append(jsonPunctuation(row.isArray ? "]" : "}"));
      if (row.comma) element.append(jsonPunctuation(","));
    } else {
      const spacer = document.createElement("span");
      spacer.className = "json-virtual-spacer";
      element.append(spacer);
      if (row.key !== null) appendVirtualKey(element, row.key);
      if (row.key !== null && typeof row.key !== "number") element.append(jsonPunctuation(": "));
      element.append(jsonValueToken(row.node));
      if (row.comma) element.append(jsonPunctuation(","));
      element.addEventListener("click", () => {
        model.selected = resolveJsonPath(model.value, row.path.slice(0, -1));
        updateVirtualNavigation(model);
      });
    }
    fragment.append(element);
  }
  tree.append(fragment);
}

function appendVirtualKey(parent, key) {
  const token = document.createElement("span");
  token.className = typeof key === "number" ? "json-index" : "json-key";
  token.textContent = typeof key === "number" ? `[${key}]` : JSON.stringify(String(key));
  parent.append(token);
}

function jsonValueToken(value) {
  const token = document.createElement("span");
  if (typeof value === "string") {
    token.className = "json-string";
    token.textContent = JSON.stringify(value);
  } else if (typeof value === "number") {
    token.className = "json-number";
    token.textContent = String(value);
  } else {
    token.className = value === null ? "json-null" : "json-boolean";
    token.textContent = String(value);
  }
  return token;
}

function toggleJsonNode(model, node) {
  setJsonCollapsed(model, node, !model.collapsed.has(node));
  model.selected = node;
  updateVirtualNavigation(model);
  renderVirtualWindow(model);
}

function setJsonCollapsed(model, node, collapsed) {
  if (model.collapsed.has(node) === collapsed) return;
  const oldCount = virtualVisibleCount(model, node);
  if (collapsed) model.collapsed.add(node);
  else model.collapsed.delete(node);
  let nextCount = 1;
  if (!collapsed) {
    nextCount = 2;
    for (const [, child] of model.children.get(node)) nextCount += virtualVisibleCount(model, child);
  }
  let delta = nextCount - oldCount;
  model.visibleCounts.set(node, nextCount);
  let current = node;
  while (model.parents.has(current)) {
    const { parent, childIndex } = model.parents.get(current);
    const prefix = model.offsets.get(parent);
    for (let index = childIndex + 1; index < prefix.length; index += 1) prefix[index] += delta;
    model.visibleCounts.set(parent, virtualVisibleCount(model, parent) + delta);
    current = parent;
  }
  model.lineCount += delta;
}

function virtualPath(model, node) {
  const path = [];
  let current = node;
  while (model.parents.has(current)) {
    const parent = model.parents.get(current);
    path.push(parent.key);
    current = parent.parent;
  }
  return path.reverse();
}

function resolveJsonPath(root, path) {
  return path.reduce((value, key) => value?.[key], root);
}

function updateVirtualNavigation(model) {
  const host = model.breadcrumbs;
  const path = model.selected ? virtualPath(model, model.selected) : [];
  host.replaceChildren();
  const up = document.createElement("button");
  up.type = "button";
  up.className = "json-up";
  up.textContent = "↑";
  up.title = "Parent object";
  up.disabled = path.length === 0;
  up.addEventListener("click", () => navigateVirtualJson(model, path.slice(0, -1)));
  host.append(up);
  const root = document.createElement("button");
  root.type = "button";
  root.className = "json-crumb";
  root.textContent = "root";
  root.addEventListener("click", () => navigateVirtualJson(model, []));
  host.append(root);
  for (let depth = 0; depth < path.length; depth += 1) {
    host.append(jsonPunctuation(" / "));
    const crumb = document.createElement("button");
    crumb.type = "button";
    crumb.className = "json-crumb";
    crumb.textContent = typeof path[depth] === "number" ? `[${path[depth]}]` : String(path[depth]);
    const nextPath = path.slice(0, depth + 1);
    crumb.addEventListener("click", () => navigateVirtualJson(model, nextPath));
    host.append(crumb);
  }
}

function navigateVirtualJson(model, path) {
  let node = model.value;
  const ancestors = [node];
  for (const key of path) {
    node = node?.[key];
    if (node && typeof node === "object") ancestors.push(node);
  }
  if (!node || typeof node !== "object") return;
  for (const ancestor of ancestors) setJsonCollapsed(model, ancestor, false);
  model.selected = node;
  updateVirtualNavigation(model);
  renderVirtualWindow(model);
  const target = virtualLineIndex(model, path);
  model.host.scrollTop = model.tree.offsetTop + Math.max(0, target - 2) * model.lineHeight;
  renderVirtualWindow(model);
}

function virtualLineIndex(model, path) {
  let node = model.value;
  let line = 0;
  for (const key of path) {
    const entries = model.children.get(node);
    const childIndex = Array.isArray(node) ? Number(key) : entries.findIndex(([name]) => name === key);
    line += 1 + model.offsets.get(node)[childIndex];
    node = entries[childIndex][1];
  }
  return line;
}

function renderJsonTokens(host, text) {
  const code = document.createElement("code");
  code.className = "json-tokenizer";
  const token = /"(?:\\.|[^"\\])*"?|true\b|false\b|null\b|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?|[{}\[\],:]/gy;
  let cursor = 0;
  while (cursor < text.length) {
    token.lastIndex = cursor;
    const match = token.exec(text);
    if (!match) {
      code.append(document.createTextNode(text[cursor]));
      cursor += 1;
      continue;
    }
    if (match.index > cursor) code.append(document.createTextNode(text.slice(cursor, match.index)));
    const value = match[0];
    const span = document.createElement("span");
    if (value.startsWith('"')) {
      const next = text.slice(token.lastIndex).match(/^\s*([,:])/);
      span.className = next?.[1] === ":" ? "json-key" : "json-string";
    } else if (value === "true" || value === "false") {
      span.className = "json-boolean";
    } else if (value === "null") {
      span.className = "json-null";
    } else if (/^[\d-]/.test(value)) {
      span.className = "json-number";
    } else {
      span.className = "json-punctuation";
    }
    span.textContent = value;
    code.append(span);
    cursor = token.lastIndex;
  }
  host.replaceChildren(code);
}

function appendJsonEntry(parent, key, value, path, comma, nodes) {
  const complex = value !== null && typeof value === "object";
  if (complex) {
    const isArray = Array.isArray(value);
    const entries = isArray ? value.map((item, index) => [index, item]) : Object.entries(value);
    const details = document.createElement("details");
    details.className = "json-object";
    details.open = true;
    details.dataset.jsonPath = JSON.stringify(path);
    nodes.set(JSON.stringify(path), details);

    const summary = document.createElement("summary");
    summary.className = "json-summary";
    if (key !== null) {
      const keyToken = document.createElement("span");
      keyToken.className = "json-key";
      keyToken.textContent = JSON.stringify(String(key));
      summary.append(keyToken, jsonPunctuation(": "));
    }
    const opening = isArray ? "[" : "{";
    summary.append(jsonPunctuation(opening));
    if (entries.length) {
      const collapsed = document.createElement("span");
      collapsed.className = "json-collapsed";
      collapsed.textContent = ` … ${entries.length} `;
      summary.append(collapsed);
    }
    const collapsedClose = document.createElement("span");
    collapsedClose.className = "json-collapsed";
    collapsedClose.textContent = isArray ? "]" : "}";
    summary.append(collapsedClose);
    if (comma) {
      const collapsedComma = jsonPunctuation(",");
      collapsedComma.classList.add("json-collapsed");
      summary.append(collapsedComma);
    }
    summary.addEventListener("click", () => updateJsonNavigation(
      parent.closest("#response-body").querySelector(".json-navigation"), path, nodes
    ));
    details.append(summary);

    const children = document.createElement("div");
    children.className = "json-children";
    entries.forEach(([childKey, childValue], index) => {
      appendJsonEntry(children, isArray ? null : childKey, childValue,
        [...path, childKey], index < entries.length - 1, nodes);
    });
    const closing = document.createElement("div");
    closing.className = "json-close";
    closing.append(jsonPunctuation(isArray ? "]" : "}"));
    if (comma) closing.append(jsonPunctuation(","));
    children.append(closing);
    details.append(children);
    parent.append(details);
    return;
  }

  const row = document.createElement("div");
  row.className = "json-row";
  row.dataset.jsonPath = JSON.stringify(path);
  if (key !== null) {
    const keyToken = document.createElement("span");
    keyToken.className = "json-key";
    keyToken.textContent = JSON.stringify(String(key));
    row.append(keyToken, jsonPunctuation(": "));
  }
  const token = document.createElement("span");
  if (typeof value === "string") {
    token.className = "json-string";
    token.textContent = JSON.stringify(value);
  } else if (typeof value === "number") {
    token.className = "json-number";
    token.textContent = String(value);
  } else {
    token.className = value === null ? "json-null" : "json-boolean";
    token.textContent = String(value);
  }
  row.append(token);
  if (comma) row.append(jsonPunctuation(","));
  row.addEventListener("click", () => updateJsonNavigation(
    parent.closest("#response-body").querySelector(".json-navigation"), path.slice(0, -1), nodes
  ));
  parent.append(row);
}

function jsonPunctuation(text) {
  const token = document.createElement("span");
  token.className = "json-punctuation";
  token.textContent = text;
  return token;
}

function updateJsonNavigation(host, path, nodes) {
  const breadcrumbs = host.querySelector(".json-breadcrumbs") || host;
  breadcrumbs.replaceChildren();
  const parentPath = path.slice(0, -1);
  const up = document.createElement("button");
  up.type = "button";
  up.className = "json-up";
  up.textContent = "↑";
  up.title = "Parent object";
  up.disabled = path.length === 0;
  up.addEventListener("click", () => navigateJson(parentPath, host, nodes));
  breadcrumbs.append(up);

  const root = document.createElement("button");
  root.type = "button";
  root.className = "json-crumb";
  root.textContent = "root";
  root.addEventListener("click", () => navigateJson([], host, nodes));
  breadcrumbs.append(root);
  for (let depth = 0; depth < path.length; depth += 1) {
    breadcrumbs.append(jsonPunctuation(" / "));
    const crumb = document.createElement("button");
    crumb.type = "button";
    crumb.className = "json-crumb";
    crumb.textContent = typeof path[depth] === "number" ? `[${path[depth]}]` : String(path[depth]);
    const target = path.slice(0, depth + 1);
    crumb.addEventListener("click", () => navigateJson(target, host, nodes));
    breadcrumbs.append(crumb);
  }
}

function navigateJson(path, navigation, nodes) {
  for (let depth = 0; depth <= path.length; depth += 1) {
    const node = nodes.get(JSON.stringify(path.slice(0, depth)));
    if (node) node.open = true;
  }
  updateJsonNavigation(navigation, path, nodes);
  nodes.get(JSON.stringify(path))?.scrollIntoView({ block: "nearest" });
}

function renderResponseEnvironment() {
  const badge = $("response-environment");
  const response = state.response;
  $("save-response").disabled = !response || state.savingResponse;
  if (!response) $("response-save-note").hidden = true;
  badge.hidden = !response;
  if (!response) return;
  const environment = response.environment;
  const known = environment != null;
  const mismatch = known && environment !== (state.env || "");
  badge.textContent = known ? environment || "no environment" : "environment unknown";
  badge.className = mismatch ? "tag tag-warn" : "tag";
  badge.title = !known
    ? "This older response has no recorded environment."
    : mismatch
      ? `Sent using ${environment || "no environment"}; currently selected: ${state.env || "no environment"}. The stored response is still available.`
      : `Environment used for this response: ${environment || "no environment"}.`;
}

function changeEnvironment() {
  state.env = $("environment").value;
  $("environment-edit").disabled = !state.env;
  localStorage.setItem(environmentStorageKey(state.workspace.path), state.env);
  renderResponseEnvironment();
  repaintVariables();
  refreshPreview();
}

function showResponseMetadata(response) {
  $("response-save-note").hidden = true;
  renderResponseEnvironment();
  renderSavedQueries();
  const status = $("status");
  status.textContent = String(response.status);
  status.className = `status ${statusClass(response.status)}`;
  $("timing").textContent = `${elapsed(response.elapsed_ms)} · ${bytes(response.bytes)}`;
  const scriptErrors = $("script-errors");
  scriptErrors.hidden = !(response.script_errors && response.script_errors.length);
  scriptErrors.textContent = scriptErrors.hidden ? "" : `script error: ${response.script_errors.join("; ")}`;
  scriptErrors.title = response.script_errors?.join("\n") || "";
  $("tls").hidden = !response.insecure;
  $("resolved").hidden = false;
  $("resolved").textContent = response.url;
  $("resolved").title = response.url;

  const missing = $("missing-vars");
  if (response.missing && response.missing.length) {
    missing.hidden = false;
    missing.textContent = `undefined: ${response.missing.join(", ")}`;
  } else {
    missing.hidden = true;
  }
}

function resetResponse(message) {
  state.response = null;
  renderResponseEnvironment();
  renderSavedQueries();
  state.projectionVersion += 1;
  const activeTab = state.requestTabs.find((tab) => tab.key === state.activeRequestTab);
  if (activeTab) activeTab.response = null;
  $("status").textContent = "no response";
  $("status").className = "status status-idle";
  $("timing").textContent = "";
  $("script-errors").hidden = true;
  $("script-errors").textContent = "";
  $("sizes").textContent = "";
  $("tls").hidden = true;
  $("resolved").hidden = true;
  $("missing-vars").hidden = true;
  $("query-error").hidden = true;
  $("response-body").textContent = message;
  $("response-body").scrollTop = 0;
}

/* ---------- projection ---------- */

async function saveResponse() {
  const response = state.response;
  if (!response || state.savingResponse) return;
  const query = $("query").value;
  state.savingResponse = true;
  renderResponseEnvironment();
  const note = $("response-save-note");
  note.hidden = true;
  try {
    const saved = await invoke("save_response", { id: response.id, query });
    if (state.response !== response) return;
    note.className = "muted query-note";
    note.textContent = "Response saved.";
    note.hidden = !saved;
  } catch (error) {
    if (state.response !== response) return;
    note.className = "query-note error-text";
    note.textContent = String(error);
    note.hidden = false;
  } finally {
    state.savingResponse = false;
    renderResponseEnvironment();
  }
}

function renderSavedQueries() {
  const editable = state.selection?.kind === "request" && Boolean(state.draft);
  $("saved-query-bar").hidden = !editable;
  if (!editable) {
    $("query-note").hidden = true;
    return;
  }
  const queries = state.draft.queries || {};
  const select = $("saved-query");
  const selected = select.value;
  select.replaceChildren();
  const custom = document.createElement("option");
  custom.value = "";
  custom.textContent = "Custom query";
  select.append(custom);
  for (const name of Object.keys(queries).sort()) {
    const option = document.createElement("option");
    option.value = name;
    option.textContent = name;
    select.append(option);
  }
  select.value = Object.hasOwn(queries, selected) && queries[selected] === $("query").value
    ? selected
    : Object.keys(queries).find((name) => queries[name] === $("query").value) || "";
  select.disabled = !state.response || Object.keys(queries).length === 0;
  $("remove-query").disabled = !select.value;
  $("store-query").disabled = !$("query").value.trim() || !$("query-name").value.trim();
}

function queryEdited() {
  const tab = state.requestTabs.find((item) => item.key === state.activeTab);
  if (tab) tab.query = $("query").value;
  renderSavedQueries();
}

function selectSavedQuery() {
  if (state.selection?.kind !== "request" || !state.response) return;
  const name = $("saved-query").value;
  if (!Object.hasOwn(state.draft.queries || {}, name)) return;
  $("query").value = state.draft.queries[name];
  $("query-name").value = name;
  queryEdited();
  void applyQuery();
}

function storeQuery() {
  if (state.selection?.kind !== "request" || !state.draft) return;
  const name = $("query-name").value.trim();
  const query = $("query").value;
  if (!name || !query.trim()) return;
  const queries = state.draft.queries || {};
  if (Object.hasOwn(queries, name) && queries[name] !== query &&
      !window.confirm(`Replace the saved query "${name}"?`)) return;
  // Computed keys also safely support names such as "__proto__".
  state.draft.queries = { ...queries, [name]: query };
  $("saved-query").value = name;
  queryEdited();
  updateDirty();
  $("query-note").textContent = "Query kept in draft. Save the request to persist query changes.";
  $("query-note").hidden = false;
}

function removeSavedQuery() {
  if (state.selection?.kind !== "request" || !state.draft) return;
  const name = $("saved-query").value;
  if (!Object.hasOwn(state.draft.queries || {}, name)) return;
  delete state.draft.queries[name];
  renderSavedQueries();
  updateDirty();
  $("query-note").textContent = "Query removed from draft. Save the request to persist query changes.";
  $("query-note").hidden = false;
}

async function applyQuery() {
  if (!state.response) return;

  const response = state.response;
  const version = ++state.projectionVersion;
  const query = $("query").value;

  try {
    const projection = await invoke("project_body", {
      id: response.id,
      query,
    });
    if (version !== state.projectionVersion || state.response?.id !== response.id) return;

    $("query-error").hidden = true;
    renderResponseText(projection.text);

    if (projection.is_raw) {
      $("sizes").textContent = projection.truncated
        ? `raw · showing first ${bytes(512 * 1024)} of ${bytes(projection.bytes)}`
        : `raw · ${bytes(projection.bytes)}`;
    } else {
      const saved = state.response.bytes
        ? Math.round((projection.bytes / state.response.bytes) * 100)
        : 100;
      $("sizes").textContent = `${bytes(state.response.bytes)} → ${bytes(
        projection.bytes
      )} (${saved}%) · ${elapsed(projection.elapsed_ms)}`;
    }
  } catch (error) {
    if (version !== state.projectionVersion || state.response?.id !== response.id) return;
    $("query-error").hidden = false;
    $("query-error").textContent = String(error);
  }
}

async function suggest() {
  if (!state.response) return;

  try {
    $("query").value = await invoke("suggest_query", { id: state.response.id });
    queryEdited();
    await applyQuery();
  } catch (error) {
    $("query-error").hidden = false;
    $("query-error").textContent = String(error);
  }
}

/* ---------- environment and globals editor ---------- */

function variableDrafts(variables) {
  return (variables || []).map((variable) => ({
    name: variable.name,
    value: variable.secret ? "" : variable.value ?? "",
    secret: Boolean(variable.secret),
    stored: Boolean(variable.has_value),
  }));
}

function renderEnvironmentVariables() {
  const rows = $("environment-variables");
  rows.replaceChildren();
  state.environmentEditor.variables.forEach((variable, index) => {
    const row = document.createElement("div");
    row.className = "row row-variable";

    const name = document.createElement("input");
    name.className = "cell cell-name";
    name.placeholder = "name";
    name.value = variable.name;
    name.autocomplete = "off";

    const value = document.createElement("input");
    value.className = "cell cell-value";
    value.placeholder = variable.secret
      ? variable.stored ? "•••••• stored — type to replace" : "not set"
      : "value";
    value.value = variable.value;
    value.title = variable.secret
      ? "Secret values live in .secrets/ and are never shown here"
      : "";

    const secret = document.createElement("input");
    secret.type = "checkbox";
    secret.className = "cell-check";
    secret.title = "Keep this value out of git";
    secret.checked = variable.secret;

    const remove = iconButton("×", "Remove this variable");
    remove.addEventListener("click", () => {
      state.environmentEditor.variables.splice(index, 1);
      renderEnvironmentVariables();
    });
    name.addEventListener("input", () => { variable.name = name.value; });
    value.addEventListener("input", () => { variable.value = value.value; });
    secret.addEventListener("change", () => {
      variable.secret = secret.checked;
      if (!variable.secret && variable.stored && !variable.value) variable.value = "";
      renderEnvironmentVariables();
    });
    row.append(name, value, secret, remove);
    rows.append(row);
  });
}

function openEnvironmentEditor(environment = null, globals = false) {
  state.environmentEditor = {
    kind: globals ? "globals" : "environment",
    oldName: environment?.name || "",
    name: environment?.name || "",
    insecure: Boolean(environment?.insecure),
    variables: variableDrafts(globals ? state.workspace.globals : environment?.variables),
  };
  const isGlobals = globals;
  $("environment-dialog-title").textContent = isGlobals
    ? "Workspace globals"
    : environment ? "Edit environment" : "New environment";
  $("environment-name-field").hidden = isGlobals;
  $("environment-insecure-field").hidden = isGlobals;
  $("environment-name").value = environment?.name || "";
  $("environment-insecure").checked = Boolean(environment?.insecure);
  $("environment-delete").hidden = isGlobals || !environment;
  $("environment-dialog-note").textContent = isGlobals
    ? "Globals are the fallback layer when no environment is selected."
    : "Secret values stay in the workspace’s ignored .secrets/ folder.";
  renderEnvironmentVariables();
  $("environment-dialog").showModal();
  if (!isGlobals && !environment) $("environment-name").focus();
}

function environmentVariablesForIpc() {
  return state.environmentEditor.variables
    .filter((variable) => variable.name.trim())
    .map((variable) => ({
      name: variable.name.trim(),
      value: variable.secret && !variable.value ? null : variable.value,
      secret: variable.secret,
    }));
}

async function saveEnvironmentEditor() {
  const editor = state.environmentEditor;
  if (!editor) return;
  const variables = environmentVariablesForIpc();
  const button = $("environment-save");
  button.disabled = true;
  $("environment-dialog-note").textContent = "Saving…";
  try {
    let workspace;
    if (editor.kind === "globals") {
      workspace = await invoke("update_globals", { variables });
    } else {
      const name = $("environment-name").value.trim();
      if (!name) throw new Error("Environment name is required.");
      const payload = {
        name,
        insecure: $("environment-insecure").checked,
        variables,
      };
      workspace = editor.oldName
        ? await invoke("update_environment", { oldName: editor.oldName, ...payload })
        : await invoke("create_environment", payload);
      state.env = name;
      localStorage.setItem(environmentStorageKey(workspace.path), name);
    }
    state.workspace = workspace;
    state.environmentEditor = null;
    $("environment-dialog").close();
    renderWorkspace();
    repaintVariables();
    refreshPreview();
  } catch (error) {
    $("environment-dialog-note").textContent = String(error);
  } finally {
    button.disabled = false;
  }
}

async function deleteSelectedEnvironment() {
  const name = state.environmentEditor?.oldName;
  if (!name || !window.confirm(`Delete environment “${name}” and its stored secrets?`)) return;
  try {
    state.workspace = await invoke("delete_environment", { name });
    state.env = "";
    localStorage.removeItem(environmentStorageKey(state.workspace.path));
    state.environmentEditor = null;
    $("environment-dialog").close();
    renderWorkspace();
    repaintVariables();
    refreshPreview();
  } catch (error) {
    $("environment-dialog-note").textContent = String(error);
  }
}

/* ---------- workspace ---------- */

function openImportDialog() {
  if (!state.workspace) return;
  $("import-workspace").textContent = state.workspace.path;
  $("import-note").textContent = "";
  $("import-secrets").value = "";
  $("import-dialog").showModal();
}

async function importPostman() {
  const button = $("import-choose");
  button.disabled = true;
  $("import-note").textContent = "Choose a JSON file to import…";
  try {
    const result = await invoke("import_postman", {
      kind: $("import-kind").value,
      secretNames: $("import-secrets").value.split(",").map((name) => name.trim()).filter(Boolean),
    });
    if (!result) {
      $("import-note").textContent = "Import cancelled. No files changed.";
      return;
    }
    state.workspace = result.workspace;
    renderWorkspace();
    repaintVariables();
    refreshPreview();
    $("import-note").textContent = [
      "Imported into the workspace. YAML files are saved on disk.",
      ...result.warnings,
    ].join("\n");
  } catch (error) {
    $("import-note").textContent = String(error);
  } finally {
    button.disabled = false;
  }
}

function renderWorkspace() {
  const workspace = state.workspace;
  $("workspace-name").textContent =
    workspace.collections.map((collection) => collection.name).join(", ") ||
    "empty workspace";
  $("workspace-path").textContent = workspace.path;
  $("import-postman").disabled = false;

  const select = $("environment");
  select.replaceChildren();
  const none = document.createElement("option");
  none.value = "";
  none.textContent = "no environment";
  select.append(none);

  for (const environment of workspace.environments) {
    const option = document.createElement("option");
    option.value = environment.name;
    option.textContent = environment.insecure
      ? `${environment.name} (no TLS verify)`
      : environment.name;
    select.append(option);
  }

  const known = workspace.environments.some(
    (environment) => environment.name === state.env
  );
  if (!known) {
    state.env = "";
    localStorage.removeItem(environmentStorageKey(workspace.path));
  }
  select.value = state.env;
  $("environment-edit").disabled = !state.env;

  renderResponseEnvironment();
  renderTree();
}

function showEmpty() {
  setHistoryViewer(false);
  $("empty-state").hidden = false;
  $("request-view").hidden = true;
  $("node-view").hidden = true;
  state.preview = "";
  $("curl").textContent = "Select a request.";
  setCurlCopyEnabled(false);
  $("preview-masked").hidden = true;
  $("preview-problem").hidden = true;
  $("summary").replaceChildren();
}

async function open(path) {
  try {
    const loaded = await invoke("open_workspace", { path });
    state.workspace = loaded;
    state.env = localStorage.getItem(environmentStorageKey(loaded.path)) || "";
    state.selection = null;
    state.draft = null;
    state.saved = "";
    state.requestTabs = [];
    state.activeTab = null;
    state.activeRequestTab = null;
    state.pendingRequests.clear();
    state.cancelRequests.clear();
    drafts.clear();
    renderRequestTabs();
    renderWorkspace();
    showEmpty();

    // Land on something real instead of an empty pane.
    const first = $("tree").querySelector(".request-row");
    if (first) first.click();
  } catch (error) {
    window.alert(String(error));
  }
}

/// The picker is native and lives in Rust, so this is one command and no npm.
async function chooseWorkspace() {
  try {
    const path = await invoke("pick_workspace");
    if (path) await open(path);
  } catch (error) {
    window.alert(String(error));
  }
}

/* ---------- boot ---------- */

function showTab(name) {
  for (const tab of document.querySelectorAll("#tabs .tab")) {
    tab.classList.toggle("active", tab.dataset.tab === name);
  }
  for (const panel of document.querySelectorAll(".tab-panel")) {
    panel.hidden = panel.id !== `tab-${name}`;
  }
}

async function boot() {
  applyLayout();
  setupSplitter($("split-left"), "left");
  setupSplitter($("split-right"), "right");

  $("collapse-left").addEventListener("click", () => toggleLeft(false));
  $("collapse-right").addEventListener("click", () => toggleRight(false));
  $("reveal-left").addEventListener("click", () => toggleLeft(true));
  $("reveal-right").addEventListener("click", () => toggleRight(true));

  $("send").addEventListener("click", send);
  $("save").addEventListener("click", save);
  $("node-save").addEventListener("click", save);
  $("suggest").addEventListener("click", suggest);
  $("apply-query").addEventListener("click", applyQuery);
  $("save-response").addEventListener("click", saveResponse);
  $("copy-curl").addEventListener("click", () => copyCurl(false));
  $("copy-curl-secrets").addEventListener("click", () => copyCurl(true));
  $("tree-search").addEventListener("input", () => {
    state.treeSearch = $("tree-search").value;
    $("tree-search-clear").hidden = !state.treeSearch;
    renderTree();
  });
  $("history-section").addEventListener("toggle", () => {
    if ($("history-section").open) void refreshHistory();
  });
  $("request-history").addEventListener("click", () => {
    void showRequestHistory().catch((error) => window.alert(String(error)));
  });
  $("request-history-close").addEventListener("click", () => $("request-history-dialog").close());
  $("history-limit").addEventListener("change", async () => {
    const mb = Math.max(0, Number($("history-limit").value) || 0);
    try {
      await invoke("set_history_limit", { limit: Math.floor(mb * 1024 * 1024) });
      await refreshHistory();
    } catch (error) { window.alert(String(error)); }
  });
  $("history-clear").addEventListener("click", async () => {
    if (!window.confirm("Clear all saved request and response history?")) return;
    await invoke("clear_history");
    await refreshHistory();
  });
  $("tree-search-clear").addEventListener("click", () => {
    state.treeSearch = "";
    $("tree-search").value = "";
    $("tree-search-clear").hidden = true;
    renderTree();
    $("tree-search").focus();
  });
  $("query").addEventListener("input", queryEdited);
  $("query-name").addEventListener("input", renderSavedQueries);
  $("saved-query").addEventListener("change", selectSavedQuery);
  $("store-query").addEventListener("click", storeQuery);
  $("remove-query").addEventListener("click", removeSavedQuery);
  $("open-workspace").addEventListener("click", chooseWorkspace);
  $("import-postman").addEventListener("click", openImportDialog);
  $("import-choose").addEventListener("click", importPostman);
  $("import-close").addEventListener("click", () => $("import-dialog").close());
  $("import-done").addEventListener("click", () => $("import-dialog").close());
  $("environment-add").addEventListener("click", () => openEnvironmentEditor());
  $("environment-edit").addEventListener("click", () => {
    const environment = state.workspace.environments.find((item) => item.name === state.env);
    if (environment) openEnvironmentEditor(environment);
  });
  $("globals-edit").addEventListener("click", () => openEnvironmentEditor(null, true));
  $("environment-add-variable").addEventListener("click", () => {
    state.environmentEditor.variables.push({ name: "", value: "", secret: false, stored: false });
    renderEnvironmentVariables();
    const inputs = $("environment-variables").querySelectorAll(".cell-name");
    inputs[inputs.length - 1]?.focus();
  });
  $("environment-save").addEventListener("click", saveEnvironmentEditor);
  $("environment-delete").addEventListener("click", deleteSelectedEnvironment);
  $("environment-cancel").addEventListener("click", () => $("environment-dialog").close());
  $("environment-close").addEventListener("click", () => $("environment-dialog").close());

  for (const tab of document.querySelectorAll("#tabs .tab")) {
    tab.addEventListener("click", () => showTab(tab.dataset.tab));
  }
  showTab("params");

  renderMethodMenu();
  $("method-trigger").addEventListener("click", () => {
    const menu = $("method-menu");
    menu.hidden = !menu.hidden;
    $("method-trigger").setAttribute("aria-expanded", String(!menu.hidden));
    if (!menu.hidden) menu.querySelector('[aria-selected="true"]')?.focus();
  });
  $("method-menu").addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      closeMethodMenu();
      $("method-trigger").focus();
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (!event.target.closest(".method-picker")) closeMethodMenu();
  });

  $("method").addEventListener("change", () => {
    state.draft.method = $("method").value;
    updateMethodSelectStyle();
    const tab = state.requestTabs.find((item) => item.key === state.activeRequestTab);
    if (tab) tab.method = state.draft.method;
    renderRequestTabs();
    updateDirty();
    schedulePreview();
  });

  withVariables($("url"));
  withVariables($("body-text"));
  for (const input of document.querySelectorAll(".script-editor")) withVariables(input);

  $("url").addEventListener("input", () => {
    absorbUrlInput();
    const tab = state.requestTabs.find((item) => item.key === state.activeRequestTab);
    if (tab) tab.url = state.draft.url;
    renderRequestTabs();
    // `:id` segments change as you type, so the Path rows follow the URL.
    renderPathParams();
    updateParamsCount();
    updateDirty();
    schedulePreview();
  });

  $("url").addEventListener("blur", absorbQuery);

  $("request-name").addEventListener("input", () => {
    state.draft.name = $("request-name").value;
    const tab = state.requestTabs.find((item) => item.key === state.activeRequestTab);
    if (tab) tab.name = state.draft.name;
    renderRequestTabs();
    updateDirty();
  });

  $("node-name").addEventListener("input", () => {
    state.draft.name = $("node-name").value;
    const tab = state.requestTabs.find((item) => item.key === state.activeTab);
    if (tab) tab.name = state.draft.name;
    updateDirty();
    refreshPreview();
  });

  $("node-description").addEventListener("input", () => {
    state.draft.description = $("node-description").value;
    updateDirty();
  });

  for (const [id, key] of [["node-pre-script", "pre_request"], ["node-post-script", "post_response"]]) {
    $(id).addEventListener("input", () => {
      state.draft.scripts ||= { pre_request: "", post_response: "" };
      state.draft.scripts[key] = $(id).value;
      updateDirty();
    });
  }
  for (const [id, key] of [["request-pre-script", "pre_request"], ["request-post-script", "post_response"]]) {
    $(id).addEventListener("input", () => {
      if (!state.draft || state.selection?.kind !== "request") return;
      state.draft.scripts ||= { pre_request: "", post_response: "" };
      state.draft.scripts[key] = $(id).value;
      updateDirty();
    });
  }

  $("body-text").addEventListener("input", readBody);
  $("body-language").addEventListener("change", () => {
    paintVariables($("body-text"));
    readBody();
  });
  $("body-clear").addEventListener("click", () => {
    $("body-text").value = "";
    paintVariables($("body-text"));
    readBody();
  });

  $("environment").addEventListener("change", changeEnvironment);

  document.querySelector('[data-add="header"]').addEventListener("click", () => {
    state.draft.headers.push({ name: "", value: "", disabled: false });
    renderHeaders();
    updateDirty();
    schedulePreview();
    const inputs = $("headers").querySelectorAll(".cell-name");
    inputs[inputs.length - 1]?.focus();
  });

  document.querySelector('[data-add="param"]').addEventListener("click", () => {
    state.draft.params = state.draft.params || [];
    // Append and rebuild from the list: a new row must never depend on the URL
    // already containing it, which is what made the previous version drop it.
    state.draft.params.push({ name: "", value: "", disabled: false });
    renderParams();
    updateDirty();
    schedulePreview();
    const inputs = $("params").querySelectorAll(".cell-name");
    inputs[inputs.length - 1]?.focus();
  });

  document.querySelector('[data-add="variable"]').addEventListener("click", () => {
    state.draft.variables.push({ name: "", value: "", secret: false, stored: false });
    renderVariables();
    updateDirty();
    const inputs = $("variables").querySelectorAll(".cell-name");
    inputs[inputs.length - 1]?.focus();
  });

  const popover = $("var-popover");
  popover.addEventListener("mouseenter", () => clearTimeout(varHideTimer));
  popover.addEventListener("mouseleave", scheduleHideVariable);
  $("var-save").addEventListener("click", saveVariable);
  $("var-cancel").addEventListener("click", hideVariable);
  $("var-value").addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      saveVariable();
    } else if (event.key === "Escape") {
      event.preventDefault();
      hideVariable();
    }
  });

  // A click outside closes the hover card; a click inside it is the edit.
  document.addEventListener("pointerdown", (event) => {
    const card = $("var-popover");
    if (card.hidden || card.contains(event.target)) return;
    if (event.target.closest?.(".varfield")) return;
    hideVariable();
  });

  $("query").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      event.stopPropagation();
      applyQuery();
    }
  });

  document.addEventListener("keydown", (event) => {
    const ctrl = event.ctrlKey || event.metaKey;
    if (!ctrl) return;

    if (event.key.toLowerCase() === "f") {
      event.preventDefault();
      event.stopImmediatePropagation();
      const search = $("response-body").querySelector(".json-search-input");
      if (search) {
        search.focus();
        search.select();
      }
    } else if (event.key === "Enter") {
      event.preventDefault();
      send();
    } else if (event.key.toLowerCase() === "s") {
      event.preventDefault();
      save();
    } else if (event.key.toLowerCase() === "b") {
      event.preventDefault();
      toggleLeft();
    }
  });

  const initial = await invoke("initial_workspace");
  if (initial) await open(initial);
}

boot();
