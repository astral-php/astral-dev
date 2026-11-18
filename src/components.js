// Page Composants (v0.2, ADR-012) : catalogue, installation vérifiée (SHA-256), progression, annulation.
// Tout le contenu affiché passe par textContent.
const T = () => window.__TAURI__;
const invoke = (c, a) => T().core.invoke(c, a);
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

let toast = (m) => console.error(m);
let data = null;
let queue = [];
let running = null; // { id, version }
let installedSomething = false;
let restartWhenDone = false;
let log = null; // fonction facultative (bandeau de première installation)

const mb = (n) => (n / 1048576).toFixed(n < 10485760 ? 1 : 0) + " Mo";

function say(line) {
  $("comp-log").hidden = false;
  $("comp-log").textContent += line + "\n";
  $("comp-log").scrollTop = $("comp-log").scrollHeight;
  if (log) log(line);
}

export function initComponents(toastFn, opts = {}) {
  toast = toastFn;
  log = opts.log ?? null;
  $("comp-recommended").addEventListener("click", () => installRecommended(false));
  $("comp-cancel").addEventListener("click", () => invoke("cancel_install").catch(toast));
  $("comp-restart").addEventListener("click", () => invoke("restart_app"));
  T().event.listen("component-progress", (e) => progress(e.payload));
  T().event.listen("component-done", (e) => done(e.payload));
}

export function onComponentsShown(shown) {
  if (shown) refresh();
}

async function refresh() {
  try { data = await invoke("components_list"); } catch (e) { return toast(e); }
  render();
}

function render() {
  const root = $("comp-list");
  root.replaceChildren();
  let total = 0, ok = 0;
  for (const c of data.components) {
    const card = el("article", "card comp");
    const h = el("h3", null, c.name + (c.required ? "" : " (facultatif)"));
    card.append(h);
    for (const v of c.versions) {
      total++;
      if (v.installed) ok++;
      const row = el("div", "comp-row");
      const label = v.version + (v.variant ? ` · ${v.variant.toUpperCase()}` : "") + (v.size ? ` · ${mb(v.size)}` : "");
      row.append(el("span", "comp-ver", label));
      row.append(el("span", "comp-state" + (v.installed ? " ok" : ""), v.installed ? "Installé" : "Absent"));
      const busy = running !== null || queue.length > 0;
      const b = el("button", "btn small" + (v.installed ? "" : " primary"), v.installed ? "Réinstaller" : "Installer");
      b.disabled = busy || (v.installed && !isRefreshable(c));
      b.title = v.installed ? "Retélécharge et vérifie à nouveau" : "Télécharge, vérifie (SHA-256) puis installe";
      b.addEventListener("click", () => start([{ id: c.id, version: v.version }]));
      row.append(b);
      card.append(row);
    }
    root.append(card);
  }
  $("comp-sub").textContent = `${ok} installé(s) sur ${total} entrée(s) du catalogue`;
  $("comp-recommended").disabled = running !== null || queue.length > 0;
}

// Réinstaller écraserait un dossier versionné : seuls les fichiers uniques (Composer, CA, extensions) sont rafraîchissables.
function isRefreshable(c) {
  return ["composer", "cacert", "ext-redis", "mod_fcgid"].includes(c.id);
}

export async function installRecommended(restartAfter) {
  if (!data) await refresh();
  const todo = [];
  for (const c of data?.components ?? []) {
    const v = c.versions[0];
    if (v && !v.installed) todo.push({ id: c.id, version: v.version });
  }
  restartWhenDone = restartAfter;
  if (todo.length === 0) {
    say("Tout le jeu recommandé est déjà installé.");
    if (restartAfter) invoke("restart_app");
    return;
  }
  start(todo);
}

function start(items) {
  $("comp-log").textContent = "";
  installedSomething = false;
  queue = items.slice();
  next();
}

function next() {
  if (running || queue.length === 0) return;
  running = queue.shift();
  $("comp-progress").hidden = false;
  $("comp-cancel").hidden = false;
  $("comp-bar").value = 0;
  $("comp-bar").removeAttribute("max");
  $("comp-status").textContent = `${running.id} ${running.version} : démarrage…`;
  invoke("install_component", running).catch((e) => {
    toast(e);
    say(`✗ ${running.id} ${running.version} : ${e?.message ?? e}`);
    running = null;
    queue = [];
    finish();
  });
  render();
}

function progress(p) {
  if (!running || p.id !== running.id) return;
  const bar = $("comp-bar");
  if (p.total) { bar.max = p.total; bar.value = p.received; } else { bar.removeAttribute("value"); }
  $("comp-status").textContent = `${p.id} ${p.version} : ${mb(p.received)}${p.total ? " / " + mb(p.total) : ""}`;
}

function done(p) {
  if (p.ok) {
    installedSomething = true;
    say(`✓ ${p.note}`);
  } else {
    say(`✗ ${p.id} ${p.version} : ${p.error}`);
    toast(p.error);
    queue = []; // on s'arrête à la première erreur : rien n'est installé à moitié
  }
  running = null;
  if (queue.length > 0) { next(); } else { finish(); }
}

async function finish() {
  $("comp-progress").hidden = true;
  $("comp-cancel").hidden = true;
  await refresh();
  if (installedSomething) {
    say("Terminé. Redémarrez astral-dev pour prendre en compte les nouveaux composants.");
    if (restartWhenDone) {
      restartWhenDone = false;
      setTimeout(() => invoke("restart_app"), 1200);
    } else {
      $("comp-restart").hidden = false;
    }
  }
}
