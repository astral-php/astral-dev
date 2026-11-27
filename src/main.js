// Phase 1 : état réel via le superviseur Rust (événement "service-state" + commandes). Phase 2 : page Redis.
import { initRedis, onRedisShown } from "./redis.js";
import { initProjects, onProjectsShown } from "./projects.js";
import { initComponents, onComponentsShown, installRecommended } from "./components.js";
import { initPages, onLogsShown, onSettingsShown } from "./pages.js";
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

// Topologie du serveur web : navigateur → (Caddy) → (Apache) → workers php-cgi, d'après les services réels.
function renderTopology(list) {
  const box = document.getElementById("topology");
  const apache = list.find((s) => s.id === "apache");
  const caddy = list.find((s) => s.id === "caddy");
  if (!apache && !caddy) { box.hidden = true; return; }
  const port = (s) => (s?.meta?.match(/port (\d+)/) ?? [])[1];
  const node = (label, state) => {
    const n = el("span", "node " + (state ?? ""));
    if (state) n.append(el("span", "dot " + state));
    n.append(document.createTextNode(label));
    return n;
  };
  const parts = [el("span", "lbl", "Topologie")];
  const add = (n) => { if (parts.length > 1) parts.push(el("span", "arrow", "→")); parts.push(n); };
  add(node("Navigateur"));
  if (caddy) add(node(`Caddy :${port(caddy) ?? "?"}`, caddy.state));
  if (apache) add(node(`Apache :${port(apache) ?? "?"}${caddy ? " (interne)" : ""}`, apache.state));
  const by = {};
  for (const w of list.filter((s) => s.pool)) (by[w.pool] ??= []).push(w);
  for (const [k, ws] of Object.entries(by)) {
    const up = ws.filter((w) => w.state === "running").length;
    add(node(`PHP ${k} · ${up}/${ws.length}`, up === ws.length ? "running" : up ? "starting" : "stopped"));
  }
  box.replaceChildren(...parts);
  box.hidden = false;
}

function render(list) {
  current = list;
  renderTopology(list);
  const root = document.getElementById("services");
  root.replaceChildren();
  // Les workers php-cgi (plusieurs par série de PHP) ne sont pas des cartes : ils se résument sur la carte PHP.
  const workers = list.filter((s) => s.pool);
  list = list.filter((s) => !s.pool);
  for (const s of list) {
    const card = el("article", "card" + (s.id === "redis" ? " redis" : ""));
    const h = el("h3", null, s.name);
    // Port tenu par un autre programme : pas « En cours » (ce n'est pas notre service) mais « Port occupé » en orange.
    h.append(el("span", "dot " + (s.conflict ? "warn" : s.state)));
    const meta = el("div", "meta", s.meta);
    const label = STATE_LABEL[s.state] ?? s.state;
    const st = el("div", "state " + (s.conflict ? "warn" : s.state), s.conflict ? "Port occupé" : s.orphan ? `${label} (session précédente)` : s.external ? `${label} (externe)` : label);
    card.append(h, meta, st);
    if (s.id === "php" && workers.length) {
      const by = {};
      for (const w of workers) { (by[w.pool] ??= []).push(w); }
      const txt = Object.entries(by).map(([k, ws]) => `${k} : ${ws.filter((w) => w.state === "running").length}/${ws.length}`).join(" · ");
      const line = el("div", "meta", `Workers php-cgi — ${txt}`);
      line.title = "Chaque série de PHP tourne sur plusieurs processus php-cgi (FastCGI) : un seul traiterait une requête à la fois";
      card.append(line);
      const bad = workers.find((w) => w.error && w.state !== "running");
      if (bad) card.append(el("div", "err", `${bad.name} : ${bad.error}`));
    }
    if (s.error && s.state !== "running") card.append(el("div", "err", s.error));
    if (s.conflict) {
      card.append(el("div", "warn", "⚠ " + s.conflict));
      const fix = button("Choisir un port libre", () => fixPorts(s.id), true);
      fix.title = "Choisit automatiquement un port libre, l'enregistre dans le fichier de configuration puis propose de redémarrer";
      card.append(fix);
    }
    const row = el("div", "row");
    if (!s.runtime && s.state !== "disabled") {
      const busy = s.state === "starting" || s.state === "stopping";
      if (s.state === "running") {
        const stop = button("Arrêter", () => act("stop_service", s.id));
        stop.disabled = busy || !(s.managed || s.orphan);
        if (!s.managed && !s.orphan) stop.title = "Lancé hors de Rustral Studio (ex. Laragon)";
        if (s.orphan) stop.title = "Processus de Rustral Studio resté d'une session précédente : il sera arrêté";
        row.append(stop, button("↻", () => act("restart_service", s.id)));
        row.lastChild.disabled = busy || !(s.managed || s.orphan);
      } else {
        const start = button("Démarrer", () => act("start_service", s.id), true);
        start.disabled = busy;
        row.append(start);
      }
      row.append(button("Logs", () => showLogs(s)));
    }
    if (s.id === "mailpit") {
      const o = button("Ouvrir Mailpit", () => invoke("open_mailpit").catch(toast));
      o.title = "Ouvre l'interface de Mailpit (navigateur par défaut, ou fenêtre de Rustral Studio : Réglages)";
      row.append(o);
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
    document.getElementById("logs-body").textContent = lines.length ? lines.join("\n") : "(vide — service jamais lancé par Rustral Studio)";
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

async function fixPorts(id) {
  try {
    const r = await invoke("fix_port_conflict", { id });
    const txt = r.changes.map((c) => `${c.key} : ${c.from} → ${c.to}`).join(", ");
    if (confirm(`Ports modifiés (${txt}).

Redémarrer Rustral Studio maintenant pour les appliquer ?`)) await invoke("restart_app");
    else toast(`Ports enregistrés (${txt}) — redémarrez Rustral Studio pour les appliquer.`, false);
  } catch (e) { toast(e); }
}

document.getElementById("start-all").addEventListener("click", () => bulk("start_all"));
document.getElementById("stop-all").addEventListener("click", () => bulk("stop_all"));
document.getElementById("open-config").addEventListener("click", () => invoke("open_config").catch(toast));
document.getElementById("quit-app").addEventListener("click", () => invoke("quit_app"));
document.getElementById("logs-close").addEventListener("click", () => document.getElementById("logs").close());

function show(view) {
  for (const v of document.querySelectorAll(".view")) v.hidden = v.id !== `view-${view}`;
  for (const a of document.querySelectorAll("#nav a[data-view]")) {
    a.classList.toggle("active", a.dataset.view === view);
    a.setAttribute("aria-current", a.dataset.view === view ? "page" : "false");
  }
  onRedisShown(view === "redis");
  onProjectsShown(view === "projects");
  onComponentsShown(view === "components");
  onLogsShown(view === "logs");
  onSettingsShown(view === "settings");
}
for (const a of document.querySelectorAll("#nav a[data-view]")) {
  a.addEventListener("click", () => show(a.dataset.view));
  // atteignable au clavier : tabulation, Entrée ou Espace
  a.setAttribute("tabindex", "0");
  a.setAttribute("role", "button");
  a.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); a.click(); } });
}

async function initSetup() {
  const box = document.getElementById("setup");
  let st;
  try { st = await invoke("setup_status"); } catch (e) { return toast(e); }
  if (!st.standalone || st.ready) return;
  box.hidden = false;
  const txt = document.getElementById("setup-text");
  const btn = document.getElementById("setup-import");
  const log = document.getElementById("setup-log");
  const partial = st.total && st.missing.length < st.total; // installation existante à laquelle il manque un composant
  const missingIds = st.missing.map((m) => m.split(" ")[0]);
  box.querySelector("h3").textContent = partial ? "Composants manquants" : "Première installation";
  const dl = document.getElementById("setup-download");
  if (partial) {
    dl.textContent = `Installer les composants manquants (${st.missing.join(", ")})`;
    document.getElementById("setup-import").hidden = true;
  }
  txt.textContent = partial
    ? `Il manque : ${st.missing.join(", ")}. Le reste de votre installation est conservé ; seuls ces composants seront téléchargés (SHA-256 vérifié).`
    : `Composants manquants : ${st.missing.join(", ")}. ` +
      (st.laragon ? `Laragon détecté (${st.laragon}) : les binaires sont copiés vers Rustral Studio (≈ 0,5 Go, quelques minutes). Laragon n'est pas modifié.`
                  : "Laragon n'a pas été trouvé : placez les binaires dans le dossier bin/ de Rustral Studio.");
  btn.disabled = !st.laragon;
  document.getElementById("setup-download").addEventListener("click", () => {
    show("components");
    installRecommended(true, partial ? missingIds : null);
  });
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    log.hidden = false;
    log.textContent = "";
    try { await invoke("import_laragon"); } catch (e) { toast(e); btn.disabled = false; }
  });
  tauri.event.listen("import-line", (e) => {
    log.textContent += e.payload + "\n";
    log.scrollTop = log.scrollHeight;
  });
  tauri.event.listen("import-done", async (e) => {
    if (e.payload.ok) {
      log.textContent += "Terminé. Redémarrage de Rustral Studio…\n";
      setTimeout(() => invoke("restart_app"), 1200);
    } else {
      toast(e.payload.error);
      btn.disabled = false;
    }
  });
}

// Version affichée = version réelle de l'application (tauri.conf.json), jamais écrite en dur.
async function showVersion() {
  try {
    const v = await tauri.app.getVersion();
    document.getElementById("app-version").textContent = `v${v} · Tauri + Rust`;
  } catch (_) { /* repli : « Tauri + Rust » */ }
}

// Avertissements du démarrage (ex. data/sites.toml illisible) : bandeau en haut du tableau de bord.
async function showWarnings() {
  try {
    const w = await invoke("startup_warnings");
    const box = document.getElementById("warnings");
    if (!w || w.length === 0) return;
    box.hidden = false;
    box.replaceChildren(...w.map((t) => el("div", null, "⚠ " + t)));
  } catch (_) { /* pas d'avertissement */ }
}

if (tauri?.core) {
  showVersion();
  showWarnings();
  initSetup();
  initRedis(toast);
  initProjects(toast);
  initComponents(toast);
  initPages(toast);
  tauri.event.listen("service-state", (e) => render(e.payload));
  refresh();
} else {
  document.getElementById("summary").textContent = "Hors Tauri : lancez `npm run dev`.";
}