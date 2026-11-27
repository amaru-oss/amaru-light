// Amaru Light web page: tickets, one ticket's timeline, and projects.
// Plain DOM, no framework; every value from the API goes in as text.
"use strict";

const view = document.getElementById("view");
const pulse = document.getElementById("pulse");
const LIST_EVERY = 5000;
const TICKET_EVERY = 3000;

let timer = null;
let info = null;

// ---------- helpers ----------

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else if (key === "class") el.className = value;
    else el.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : String(child));
  }
  return el;
}

function storage(action, key, value) {
  try {
    if (action === "get") return localStorage.getItem(key);
    if (action === "set") localStorage.setItem(key, value);
    if (action === "remove") localStorage.removeItem(key);
  } catch (_) {
    // Private windows may refuse storage; the page works without it.
  }
  return null;
}

class Unauthorized extends Error {}

async function api(path, options = {}) {
  const headers = { Accept: "application/json" };
  const token = storage("get", "amaru-token");
  if (token) headers.Authorization = `Bearer ${token}`;
  if (options.method && options.method !== "GET") {
    headers["Content-Type"] = "application/json";
    headers["X-Amaru-Action"] = "1";
  }
  let response;
  try {
    response = await fetch(path, { ...options, headers });
  } catch (error) {
    setPulse("down", "The daemon is not reachable");
    throw error;
  }
  if (response.status === 401) throw new Unauthorized();
  const body = await response.json().catch(() => ({}));
  setPulse("live", "Connected");
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

function setPulse(state, title) {
  pulse.className = `pulse ${state}`;
  pulse.title = title;
}

function relative(iso) {
  if (!iso) return "";
  const seconds = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
  if (seconds < 45) return "just now";
  const units = [["d", 86400], ["h", 3600], ["min", 60]];
  for (const [unit, size] of units) {
    if (seconds >= size) return `${Math.floor(seconds / size)} ${unit} ago`;
  }
  return "1 min ago";
}

function clock(iso) {
  const d = new Date(iso);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}

function stamp(iso) {
  return iso ? new Date(iso).toLocaleString() : "";
}

const GROUPS = {
  QUEUED: "active", PREPARING: "active", IMPLEMENTING: "active", CHECKING: "active",
  SELF_REVIEW: "active", PUBLISHING: "active", ADDRESSING_REVIEW: "active",
  AWAITING_REVIEW: "review", BLOCKED: "blocked", DONE: "done", CANCELLED: "cancelled",
};

function label(state) {
  return state.toLowerCase().replace(/_/g, " ");
}

function badge(state) {
  return h("span", { class: `state ${GROUPS[state] || "cancelled"}` }, label(state));
}

function firstLine(text, limit = 160) {
  const line = String(text || "").split("\n")[0];
  return line.length > limit ? `${line.slice(0, limit)}…` : line;
}

/** Replace an element's children, skipping the empty ones. */
function fill(el, ...nodes) {
  el.replaceChildren(...nodes.flat().filter((n) => n !== null && n !== undefined && n !== false));
}

function render(...nodes) {
  fill(view, ...nodes);
}

function schedule(fn, every) {
  clearTimeout(timer);
  timer = setTimeout(async () => {
    if (document.hidden) return schedule(fn, every);
    try {
      await fn();
    } catch (error) {
      if (error instanceof Unauthorized) return showLogin();
    }
    schedule(fn, every);
  }, every);
}

// ---------- tickets ----------

const FILTERS = [
  ["active", "In progress", (t) => GROUPS[t.state] === "active"],
  ["review", "Awaiting review", (t) => t.state === "AWAITING_REVIEW"],
  ["blocked", "Needs you", (t) => t.state === "BLOCKED"],
  ["done", "Finished", (t) => t.state === "DONE" || t.state === "CANCELLED"],
];

let filter = storage("get", "amaru-filter") || "all";

async function showTickets() {
  const message = h("div");
  const paused = h("div");
  const counts = h("div", { class: "counts" });
  const list = h("div", { class: "card list" }, h("div", { class: "empty" }, "Loading…"));
  const input = h("input", {
    type: "text", name: "issue", placeholder: "owner/name#42, issue URL or Jira key",
    "aria-label": "Issue to claim", autocomplete: "off", spellcheck: "false",
  });
  const button = h("button", { type: "submit" }, "Claim");
  const form = h("form", { class: "claim", onsubmit: (e) => claim(e, input, button, message) }, input, button);
  render(
    h("div", { class: "head" },
      h("div", {}, h("h1", {}, "Tickets"),
        h("p", { class: "sub" }, "Issues the daemon is working on, newest first.")),
      form),
    paused, message, counts, list);

  const refresh = async () => {
    const [tickets, about] = await Promise.all([api("/api/tickets?limit=300"), api("/api/info")]);
    drawPaused(paused, about);
    drawCounts(counts, tickets, () => drawList(list, tickets));
    drawList(list, tickets);
  };
  await refresh().catch(handle(list));
  schedule(refresh, LIST_EVERY);
}

// Projects whose agent is signed out: the daemon takes no work for them.
function drawPaused(container, about) {
  const waiting = about.projects.filter((p) => p.waiting_for_sign_in.length);
  container.replaceChildren(...waiting.map((p) => {
    const names = p.waiting_for_sign_in.join(" and ");
    const verb = p.waiting_for_sign_in.length === 1 ? "is" : "are";
    return h("p", { class: "notice bad" },
      `Not taking work for ${p.repo}: agent ${names} ${verb} not signed in. `,
      "Sign in on the ", h("a", { href: "#/agents" }, "Agents"), " page; the daemon picks work up again on its next poll.");
  }));
}

function drawCounts(container, tickets, redraw) {
  const cards = FILTERS.map(([key, name, test]) => {
    const n = tickets.filter(test).length;
    return h("button", {
      type: "button",
      class: `count${filter === key ? " on" : ""}`,
      "aria-pressed": filter === key ? "true" : "false",
      onclick: () => {
        filter = filter === key ? "all" : key;
        storage("set", "amaru-filter", filter);
        drawCounts(container, tickets, redraw);
        redraw();
      },
    }, h("b", {}, n), h("span", {}, name));
  });
  container.replaceChildren(...cards);
}

function drawList(container, tickets) {
  const rule = FILTERS.find(([key]) => key === filter);
  const shown = rule ? tickets.filter(rule[2]) : tickets;
  if (!shown.length) {
    const text = tickets.length
      ? "No tickets in this group. Select it again to see them all."
      : "No tickets yet. Claim an issue above, or label one for the daemon to pick up.";
    container.replaceChildren(h("div", { class: "empty" }, text));
    return;
  }
  const header = h("div", { class: "row header" },
    h("span", {}, "#"), h("span", {}, "Issue"), h("span", {}, "State"),
    h("span", {}, "PR"), h("span", { class: "when" }, "Updated"));
  container.replaceChildren(header, ...shown.map(row));
}

function row(t) {
  return h("a", { class: "row", href: `#/tickets/${t.id}` },
    h("span", { class: "id" }, t.id),
    h("span", { class: "title" },
      h("b", {}, t.title || "(untitled)"),
      h("small", {}, `${t.reference}`),
      t.state === "BLOCKED" && t.reason ? h("span", { class: "why" }, firstLine(t.reason)) : null),
    badge(t.state),
    h("span", { class: "pr" }, t.pr_number ? `#${t.pr_number}` : "—"),
    h("span", { class: "when", title: stamp(t.updated_at) }, relative(t.updated_at)));
}

async function claim(event, input, button, message) {
  event.preventDefault();
  const issue = input.value.trim();
  if (!issue) return;
  button.disabled = true;
  try {
    const result = await api("/api/tickets", { method: "POST", body: JSON.stringify({ issue }) });
    const t = result.ticket;
    const text = result.outcome === "queued"
      ? `Queued ${t.reference} as ticket ${t.id}.`
      : `${t.reference} already has ticket ${t.id} (${label(t.state)}).`;
    message.replaceChildren(h("p", { class: "notice" }, text, " ", h("a", { href: `#/tickets/${t.id}` }, "Open it")));
    input.value = "";
  } catch (error) {
    if (error instanceof Unauthorized) return showLogin();
    message.replaceChildren(h("p", { class: "notice bad" }, error.message));
  } finally {
    button.disabled = false;
  }
}

// ---------- one ticket ----------

async function showTicket(id) {
  const top = h("div", {}, h("p", { class: "empty" }, "Loading…"));
  const side = h("aside");
  const events = h("ol", { class: "timeline" });
  const follow = h("input", { type: "checkbox", checked: true });
  const timeline = h("section", {},
    h("div", { class: "timeline-head" }, h("h2", {}, "Timeline"),
      h("label", {}, follow, "Follow new events")),
    h("div", { class: "card" }, events));
  const guidance = h("div");
  render(top, guidance, h("div", { class: "grid" }, timeline, side));

  let after = 0;
  let lastState = null;
  const refresh = async () => {
    const t = await api(`/api/tickets/${id}`);
    if (t.state !== lastState || !top.dataset.drawn) {
      drawTop(top, t, refresh);
      lastState = t.state;
      drawGuide(guidance, t, refresh);
    }
    drawSide(side, t);
    const fresh = await api(`/api/tickets/${id}/events?after=${after}`);
    if (fresh.length) {
      after = fresh[fresh.length - 1].id;
      events.append(...fresh.map(eventItem));
      if (follow.checked && after && events.lastChild) {
        events.lastChild.scrollIntoView({ block: "nearest" });
      }
    }
    if (!events.children.length) {
      events.replaceChildren(h("li", { class: "empty" }, "No events yet."));
    }
  };
  await refresh().catch(handle(top));
  schedule(refresh, TICKET_EVERY);
}

function drawTop(container, t, refresh) {
  container.dataset.drawn = "1";
  const actions = h("div", { class: "actions" });
  if (t.state === "BLOCKED") {
    actions.append(h("button", { type: "button", onclick: (e) => act(e, t, "unblock", refresh) }, "Unblock"));
  }
  if (t.state !== "DONE" && t.state !== "CANCELLED") {
    actions.append(h("button", { type: "button", class: "danger", onclick: (e) => act(e, t, "cancel", refresh) }, "Cancel"));
  }
  fill(container,
    h("div", { class: "crumbs" }, h("a", { href: "#/" }, "Tickets"), ` / ticket ${t.id}`),
    h("div", { class: "head" },
      h("div", {},
        h("h1", {}, t.title || "(untitled)"),
        h("p", { class: "sub" }, `${t.reference} · `, badge(t.state)),
        h("div", { class: "links" },
          h("a", { href: t.issue_url, target: "_blank", rel: "noopener" }, "Issue ↗"),
          t.pr_url ? h("a", { href: t.pr_url, target: "_blank", rel: "noopener" }, `Pull request #${t.pr_number} ↗`) : null,
          t.branch_url ? h("a", { href: t.branch_url, target: "_blank", rel: "noopener" }, "Branch ↗") : null)),
      actions),
    h("div", { class: "act-message" }),
    t.state === "BLOCKED" && t.reason
      ? h("div", { class: "card reason" },
        h("p", {}, "Blocked: the daemon needs you"),
        h("pre", {}, t.reason))
      : null);
}

/** The guidance form: free-form directions for the ticket's agent. Drawn
 *  again only when the state changes, so a draft survives the refreshes. */
function drawGuide(container, t, refresh) {
  if (t.state === "DONE" || t.state === "CANCELLED") return fill(container);
  const hint = {
    BLOCKED: "The ticket continues with your directions.",
    AWAITING_REVIEW: "The agent follows them, the checks run, and the branch is pushed again.",
    QUEUED: "The agent gets them with the issue when it starts.",
    PREPARING: "The agent gets them with the issue when it starts.",
  }[t.state] || "A running agent is stopped and resumed with them.";
  const field = h("textarea", { rows: 3, placeholder: "Tell the agent what to do next…", "aria-label": "Guidance for the agent" });
  const button = h("button", { type: "submit" }, "Send to agent");
  const message = h("p", { class: "guide-note" }, `${hint} The daemon picks them up at its next poll.`);
  const send = async (event) => {
    event.preventDefault();
    if (!field.value.trim()) return field.focus();
    button.disabled = true;
    try {
      await api(`/api/tickets/${t.id}/guide`, { method: "POST", body: JSON.stringify({ text: field.value }) });
      field.value = "";
      message.replaceChildren("Sent. The agent gets it at the daemon's next poll.");
      await refresh();
    } catch (error) {
      if (error instanceof Unauthorized) return showLogin();
      message.replaceChildren(h("span", { class: "bad" }, error.message));
    } finally {
      button.disabled = false;
    }
  };
  const keys = (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) send(event);
  };
  field.addEventListener("keydown", keys);
  fill(container, h("form", { class: "card guide", onsubmit: send },
    h("label", {}, h("span", { class: "guide-title" }, "Guide the agent"), field),
    h("div", { class: "guide-foot" }, message, button)));
}

async function act(event, t, action, refresh) {
  const button = event.currentTarget;
  if (action === "cancel" && !window.confirm(`Cancel ticket ${t.id}? A running agent is stopped.`)) return;
  button.disabled = true;
  const message = view.querySelector(".act-message");
  try {
    await api(`/api/tickets/${t.id}/${action}`, { method: "POST", body: "{}" });
    message.replaceChildren();
    await refresh();
  } catch (error) {
    if (error instanceof Unauthorized) return showLogin();
    message.replaceChildren(h("p", { class: "notice bad" }, error.message));
    button.disabled = false;
  }
}

function fact(name, value) {
  if (value === null || value === undefined || value === "") return null;
  return h("div", {}, h("dt", {}, name), h("dd", {}, value));
}

function drawSide(container, t) {
  fill(container,
    h("h2", {}, "Details"),
    h("div", { class: "card facts" }, h("dl", {},
      fact("Branch", t.branch ? h("code", {}, t.branch) : null),
      fact("Base", t.base ? h("code", {}, t.base) : null),
      fact("Stacked on", t.stacked_on ? `#${t.stacked_on}` : null),
      fact("Origin", t.origin),
      fact("Fix rounds", String(t.fix_rounds)),
      fact("Review rounds", String(t.review_rounds)),
      fact("Self-reviewed", t.self_reviewed ? "yes" : null),
      fact("Agent session", t.session_id ? h("code", {}, t.session_id) : null),
      fact("Created", stamp(t.created_at)),
      fact("Updated", stamp(t.updated_at)))),
    t.summary ? h("h2", {}, "Agent summary") : null,
    t.summary ? h("div", { class: "card summary" }, t.summary) : null);
}

/** "Some(Done)" from the daemon reads as "done". */
function outcome(exit) {
  const text = String(exit || "").replace(/^Some\((.*)\)$/, "$1");
  return !text || text === "None" ? "no result reported" : text.toLowerCase();
}

function eventItem(e) {
  const p = e.payload || {};
  let cls = "quiet";
  let body;
  switch (e.kind) {
    case "STATE":
      cls = "state-change";
      body = [p.from ? `${label(p.from)} → ` : "", label(p.to || "")];
      break;
    case "CLAIMED":
      cls = "state-change";
      body = `Claimed (${p.origin || "?"})`;
      break;
    case "UNBLOCKED":
      cls = "state-change";
      body = `Unblocked, continues from ${label(p.to || "")}`;
      break;
    case "CANCELLED":
      cls = "bad";
      body = "Cancelled by the operator";
      break;
    case "GUIDANCE":
      cls = "guidance";
      body = [`Guidance for the agent${p.by ? ` (from the ${p.by})` : ""}`, h("pre", {}, p.text || "")];
      break;
    case "GUIDANCE_DELIVERED":
      body = "Guidance given to the agent";
      break;
    case "INTERRUPTED":
      body = "Stopped the agent to give it the guidance; it resumes at the next poll";
      break;
    case "AGENT_TEXT":
      cls = "agent-text";
      body = p.text;
      break;
    case "AGENT_TOOL": {
      cls = "tool";
      const full = String(p.tool || "");
      body = full.includes("\n") || full.length > 160
        ? h("details", {}, h("summary", {}, h("code", {}, firstLine(full))), h("pre", {}, full))
        : h("code", {}, full);
      break;
    }
    case "AGENT_TURN":
      body = `Agent turn ended: ${outcome(p.exit)}${p.turns ? `, ${p.turns} turns` : ""}${p.cost_usd ? `, $${Number(p.cost_usd).toFixed(2)}` : ""}`;
      if (p.error) cls = "bad";
      break;
    case "AGENT_SESSION":
      body = h("span", {}, "Agent session ", h("code", {}, p.session || ""));
      break;
    case "CHECK_FAILED":
      cls = "bad";
      body = [`Check ${p.name || ""} failed (${p.exit || ""})`, p.output ? h("pre", {}, p.output) : null];
      break;
    case "ERROR":
      cls = "bad";
      body = [`Error ${p.count ? `(${p.count} in a row)` : ""}`, h("pre", {}, p.error || "")];
      break;
    case "SELF_REVIEW":
      cls = "agent-text";
      body = `Self-review: ${outcome(p.status)}${p.summary ? `. ${p.summary}` : ""}`;
      break;
    case "SESSION_LOST":
      body = "The agent session could not be resumed; started a new one";
      break;
    case "LABEL_SKIPPED":
      body = `Could not add the ${p.label} label; going on without it`;
      break;
    case "REACTION_SKIPPED":
      body = `Could not react to ${p.feedback}; going on without it`;
      break;
    default:
      body = h("code", {}, JSON.stringify(p));
  }
  return h("li", { class: `ev ${cls}` },
    h("time", { datetime: e.at, title: stamp(e.at) }, clock(e.at)),
    h("span", { class: "dot", "aria-hidden": "true" }),
    h("div", { class: "body" },
      cls === "quiet" || cls === "tool" ? h("span", { class: "kind" }, e.kind.toLowerCase().replace(/_/g, " ")) : null,
      body));
}

// ---------- projects ----------

async function showProjects() {
  clearTimeout(timer);
  const grid = h("div", { class: "projects" });
  render(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "Projects"),
      h("p", { class: "sub" }, "From the daemon's settings files. Edit them and restart to change."))),
    grid);
  try {
    info = await api("/api/info");
  } catch (error) {
    return handle(grid)(error);
  }
  grid.replaceChildren(...info.projects.map((p) => h("section", { class: "card project" },
    h("h2", {}, p.repo),
    h("p", { class: "sub" }, p.fork ? `Pushes to the fork ${p.fork}` : "Pushes to the repository"),
    h("div", { class: "facts" }, h("dl", {},
      fact("Base branch", h("code", {}, p.base_branch)),
      fact("Tickets from", p.jira ? `Jira ${p.jira}` : "GitHub issues"),
      fact("Agent", p.waiting_for_sign_in.length
        ? [p.agent, " ", h("span", { class: "state blocked" }, `${p.waiting_for_sign_in.join(", ")} not signed in`)]
        : p.agent),
      fact("Picks up labels", p.labels.length ? p.labels.map((l) => h("span", { class: "tag" }, l)) : "—"),
      fact("Picks up assignee", p.assignee || "—"),
      fact("Checks", p.checks.length ? p.checks.map((c) => h("span", { class: "tag" }, c)) : "—"),
      fact("Self-review", label(p.self_review.replace(/([a-z])([A-Z])/g, "$1_$2"))),
      fact("Reviewers", p.reviewers.length ? p.reviewers.join(", ") : "—"))))));
}

// ---------- agents ----------

const LOGIN_EVERY = 1500;

async function showAgents() {
  clearTimeout(timer);
  const list = h("div", { class: "agents" }, h("p", { class: "empty" }, "Loading…"));
  render(
    h("div", { class: "head" }, h("div", {}, h("h1", {}, "Agents"),
      h("p", { class: "sub" }, "The agent CLIs from the daemon's settings. Amaru does not ship them or their accounts: sign each one in with its own command, here or in a shell."))),
    list);
  let agents;
  try {
    agents = await api("/api/agents");
  } catch (error) {
    return handle(list)(error);
  }
  if (!agents.length) return fill(list, h("p", { class: "empty" }, "No agents are set up."));
  const cards = agents.map(agentCard);
  fill(list, cards.map((c) => c.el));
  const poll = async () => {
    await Promise.all(cards.filter((c) => c.running()).map((c) => c.refresh()));
  };
  schedule(poll, LOGIN_EVERY);
}

function agentCard(a) {
  const chip = h("span", { class: "state" }, "checking…");
  const detail = h("pre", { class: "status-detail" });
  const session = h("div", { class: "login-session" });
  const signIn = h("button", { type: "button", onclick: () => start() }, "Sign in");
  const check = h("button", { type: "button", class: "ghost", onclick: () => checkStatus() }, "Check again");
  const keyField = h("input", { type: "password", name: "api-key", placeholder: "API key", autocomplete: "off", spellcheck: "false", "aria-label": `API key for ${a.name}` });
  const keyForm = h("form", {
    class: "login-input key-form", hidden: true,
    onsubmit: (e) => {
      e.preventDefault();
      const key = keyField.value.trim();
      keyField.value = "";
      if (!key) return;
      keyForm.hidden = true;
      call("", { key });
    },
  }, h("p", { class: "sub key-note" }, "Runs ", h("code", {}, a.key_login_command || ""),
    " with the key on its input. Amaru does not store the key or show it again; the CLI keeps its own login."),
  keyField, h("button", { type: "submit" }, "Sign in with key"),
  h("button", { type: "button", class: "ghost", onclick: () => { keyField.value = ""; keyForm.hidden = true; } }, "Close"));
  const useKey = h("button", {
    type: "button", class: "ghost",
    onclick: () => { keyForm.hidden = !keyForm.hidden; if (!keyForm.hidden) keyField.focus(); },
  }, "Use an API key");
  const el = h("section", { class: "card agent" },
    h("div", { class: "agent-head" },
      h("div", {},
        h("h2", {}, a.name, " ", chip),
        h("p", { class: "sub" }, `${a.driver} driver · `, h("code", {}, a.command),
          a.used_by.length ? ` · used by ${a.used_by.join(", ")}` : " · not used by a project")),
      h("div", { class: "actions" }, check, a.key_login_command ? useKey : null, a.login_command ? signIn : null)),
    a.key_login_command
      ? keyForm
      : null,
    h("details", {}, h("summary", {}, "Status output"), detail),
    session);
  let login = a.login;

  async function checkStatus() {
    fill(chip, "checking…");
    chip.className = "state";
    try {
      const st = await api(`/api/agents/${encodeURIComponent(a.name)}/status`);
      const [cls, text] = st.signed_in === true ? ["ok", "signed in"]
        : st.signed_in === false ? ["blocked", "not signed in"] : ["cancelled", "unknown"];
      chip.className = `state ${cls}`;
      fill(chip, text);
      fill(detail, `$ ${a.status_command || "(none)"}\n${st.detail || "(no output)"}`);
      signIn.textContent = st.signed_in ? "Sign in again" : "Sign in";
    } catch (error) {
      if (error instanceof Unauthorized) return showLogin();
      chip.className = "state cancelled";
      fill(chip, "unknown");
      fill(detail, error.message);
    }
  }

  async function call(path, body) {
    try {
      login = await api(`/api/agents/${encodeURIComponent(a.name)}/login${path}`,
        { method: "POST", body: JSON.stringify(body || {}) });
      draw();
    } catch (error) {
      if (error instanceof Unauthorized) return showLogin();
      fill(session, h("p", { class: "notice bad" }, error.message));
    }
  }

  const start = () => call("");

  async function refresh() {
    try {
      login = await api(`/api/agents/${encodeURIComponent(a.name)}/login`);
    } catch (error) {
      if (error instanceof Unauthorized) throw error;
      return;
    }
    draw();
    if (!login.running) checkStatus();
  }

  function draw() {
    if (!login) return fill(session);
    const out = session.querySelector(".login-output");
    const atEnd = !out || out.scrollTop + out.clientHeight >= out.scrollHeight - 4;
    const output = h("pre", { class: "login-output" }, linkify(login.output || "Waiting for output…"));
    const parts = [
      h("div", { class: "login-head" },
        h("strong", {}, login.running ? "Signing in" : `Sign-in ${login.exit}`),
        h("span", { class: "sub" }, ` · started ${relative(login.started_at)}`)),
      output,
    ];
    if (login.running) {
      const field = h("input", { type: "text", name: "code", placeholder: "Paste the code here if the CLI asks for one", autocomplete: "off", "aria-label": "Input for the sign-in" });
      parts.push(h("form", { class: "login-input", onsubmit: (e) => { e.preventDefault(); call("/input", { text: field.value }); } },
        field,
        h("button", { type: "submit" }, "Send"),
        h("button", { type: "button", class: "danger", onclick: () => call("/cancel") }, "Stop")));
    }
    const typing = session.querySelector(".login-input input");
    if (typing && login.running && document.activeElement === typing) {
      // Keep what the operator is typing; only the output changes.
      session.querySelector(".login-output").replaceWith(output);
    } else {
      fill(session, parts);
    }
    if (atEnd) output.scrollTop = output.scrollHeight;
  }

  draw();
  checkStatus();
  return { el, refresh, running: () => Boolean(login && login.running) };
}

/** Text with its web links made clickable. */
function linkify(text) {
  return text.split(/(https?:\/\/[^\s"'<>]+)/g).map((part, i) =>
    i % 2 ? h("a", { href: part, target: "_blank", rel: "noopener noreferrer" }, part) : part);
}

// ---------- token ----------

function showLogin() {
  clearTimeout(timer);
  setPulse("down", "A token is required");
  const input = h("input", { type: "password", name: "token", placeholder: "Token", "aria-label": "Token", autocomplete: "current-password" });
  render(h("div", { class: "card login" },
    h("h1", {}, "Sign in"),
    h("p", { class: "sub" }, "This daemon requires the token from AMARU_WEB_TOKEN."),
    h("form", {
      onsubmit: (e) => {
        e.preventDefault();
        storage("set", "amaru-token", input.value.trim());
        route();
      },
    }, input, h("button", { type: "submit" }, "Continue"))));
  input.focus();
}

function handle(container) {
  return (error) => {
    if (error instanceof Unauthorized) return showLogin();
    container.replaceChildren(h("p", { class: "notice bad" }, error.message || String(error)));
  };
}

// ---------- routing and theme ----------

function route() {
  clearTimeout(timer);
  const hash = location.hash || "#/";
  const ticket = hash.match(/^#\/tickets\/(\d+)/);
  const page = hash.startsWith("#/projects") ? "projects"
    : hash.startsWith("#/agents") ? "agents" : "tickets";
  for (const link of document.querySelectorAll("[data-nav]")) {
    link.classList.toggle("on", link.dataset.nav === page);
  }
  window.scrollTo(0, 0);
  if (ticket) return showTicket(ticket[1]);
  if (page === "projects") return showProjects();
  if (page === "agents") return showAgents();
  return showTickets();
}

function setupTheme() {
  const saved = storage("get", "amaru-theme");
  if (saved) document.documentElement.dataset.theme = saved;
  document.getElementById("theme").addEventListener("click", () => {
    const dark = document.documentElement.dataset.theme
      ? document.documentElement.dataset.theme === "dark"
      : window.matchMedia("(prefers-color-scheme: dark)").matches;
    const next = dark ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    storage("set", "amaru-theme", next);
  });
}

async function footer() {
  try {
    const i = await api("/api/info");
    document.getElementById("foot").textContent =
      `Amaru Light ${i.version} · polls GitHub every ${i.poll_interval_seconds}s · up to ${i.max_parallel} ${i.max_parallel === 1 ? "ticket" : "tickets"} at once`;
  } catch (_) {
    // The footer is optional; the pages report errors themselves.
  }
}

setupTheme();
window.addEventListener("hashchange", route);
route();
footer();
