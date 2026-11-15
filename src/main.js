// Phase 1 : état réel via le superviseur Rust (événement "service-state" + commandes). Phase 2 : page Redis.
import { initRedis, onRedisShown } from "./redis.js";
import { initProjects, onProjectsShown } from "./projects.js";
const tauri = window.__TAURI__;
const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
const STATE_LABEL = {
  stopped: "Arrêté", starting: "Démarrage…", running: "En cours",
  stopping: "Arrêt…", failed: "Échec", disabled: "Désactivé",
};
let current = [];

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

function toast(msg, bad = true) {
  const t = document.getElementById("toast");
  t.textContent = typeof msg === "string" ? msg : (msg?.message ?? JSON.stringify(msg));
  t.className = "toast show" + (bad ? " bad" : "");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (t.className = "toast"), 6000);
}

async function act(cmd, id) {
  try { await invoke(cmd, { id }); } catch (e) { toast(e); }
  refresh();
}

function button(label, onclick, primary = false) {
  const b = el("button", "btn small" + (primary ? " primary" : ""), label);
  b.addEventListener("click", onclick);
  return b;
}

function render(list) {
  current = list;
  const root = document.getElementById("services");
  root.replaceChildren();
  for (const s of list) {
    const card = el("article", "card" + (s.id === "redis" ? " redis" : ""));
    const h = el("h3", null, s.name);
    h.append(el("span", "dot " + s.state));
    const meta = el("div", "meta", s.meta);
    const label = STATE_LABEL[s.state] ?? s.state;
    const st = el("div", "state " + s.state, s.external ? `${label} (externe)` : label);
    card.append(h, meta, st);
    if (s.error && s.state !== "running") card.append(el("div", "err", s.error));
    const row = el("div", "row");
    if (!s.runtime && s.state !== "disabled") {
      const busy = s.state === "starting" || s.state === "stopping";
      if (s.state === "running") {
        const stop = button("Arrêter", () => act("stop_service", s.id));
        stop.disabled = busy || !s.managed;
        if (!s.managed) stop.title = "Lancé hors d'astral-dev (ex. Laragon)";
        row.append(stop, button("↻", () => act("restart_service", s.id)));
        row.lastChild.disabled = busy || !s.managed;
      } else {
        const start = button("Démarrer", () => act("start_service", s.id), true);
        start.disabled = busy;
        row.append(start);
      }
      row.append(button("Logs", () => showLogs(s)));
    }
    card.append(row);
    root.append(card);
  }
  const n = list.filter((s) => s.state === "running").length;
  document.getElementById("summary").textContent = `${list.length} services · ${n} actifs`;
}

async function showLogs(s) {
  const dlg = document.getElementById("logs");
  document.getElementById("logs-title").textContent = `Logs — ${s.name}`;
  try {
    const lines = await invoke("service_logs", { id: s.id, lines: 300 });
    document.getElementById("logs-body").textContent = lines.length ? lines.join("\n") : "(vide — service jamais lancé par astral-dev)";
  } catch (e) { toast(e); }
  dlg.showModal();
}

async function refresh() {
  try { render(await invoke("list_services")); } catch (e) { toast(e); }
}

async function bulk(cmd) {
  try {
    const res = await invoke(cmd);
    const bad = res.filter((r) => !r.ok);
    if (bad.length) toast(bad.map((r) => `${r.id} : ${r.note}`).join(" · "));
  } catch (e) { toast(e); }
  refresh();
}

document.getElementById("start-all").addEventListener("click", () => bulk("start_all"));
document.getElementById("stop-all").addEventListener("click", () => bulk("stop_all"));
document.getElementById("logs-close").addEventListener("click", () => document.getElementById("logs").close());

function show(view) {
  for (const v of document.querySelectorAll(".view")) v.hidden = v.id !== `view-${view}`;
  for (const a of document.querySelectorAll("#nav a[data-view]")) a.classList.toggle("active", a.dataset.view === view);
  onRedisShown(view === "redis");
  onProjectsShown(view === "projects");
}
for (const a of document.querySelectorAll("#nav a[data-view]")) a.addEventListener("click", () => show(a.dataset.view));

if (tauri?.core) {
  initRedis(toast);
  initProjects(toast);
  tauri.event.listen("service-state", (e) => render(e.payload));
  refresh();
} else {
  document.getElementById("summary").textContent = "Hors Tauri : lancez `npm run dev`.";
}