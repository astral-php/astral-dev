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
  $("https-uninstall-ca").addEventListener("click", async () => {
    if (!confirm("Retirer l'autorité locale mkcert de vos autorités racines Windows ?\n\nLes sites en https:// afficheront de nouveau un avertissement tant qu'elle n'est pas réinstallée. Windows peut demander une confirmation.")) return;
    const b = $("https-uninstall-ca");
    b.disabled = true;
    try { toast(await invoke("mkcert_uninstall_ca"), false); } catch (e) { toast(e); }
    b.disabled = false;
    loadHttps();
  });
  $("https-install-ca").addEventListener("click", async () => {
    if (!confirm("Installer l'autorité de certification locale mkcert ?\n\nElle est ajoutée aux autorités racines de VOTRE compte Windows (pas de droits administrateur). Windows affichera une fenêtre de sécurité à accepter. Elle permet de signer des certificats pour vos domaines .test ; ne partagez jamais son dossier (rootCA-key.pem).")) return;
    const b = $("https-install-ca");
    b.disabled = true;
    try { toast(await invoke("mkcert_install_ca"), false); } catch (e) { toast(e); }
    b.disabled = false;
    loadHttps();
  });
  T().event.listen("component-progress", (e) => progress(e.payload));
  T().event.listen("component-done", (e) => done(e.payload));
}

export function onComponentsShown(shown) {
  if (shown) { refresh(); loadHttps(); }
}

// HTTPS local : mkcert (composant du catalogue) + autorité de confiance (mkcert -install, après confirmation).
async function loadHttps() {
  let s;
  try { s = await invoke("mkcert_status"); } catch (e) { return toast(e); }
  const t = $("https-text");
  const b = $("https-install-ca");
  b.hidden = true;
  $("https-uninstall-ca").hidden = !(s.installed && s.ca_trusted);
  if (!s.installed) {
    t.textContent = "mkcert n'est pas installé : installez-le ci-dessous (ligne « mkcert (HTTPS local) »), puis revenez ici. Il permet d'ouvrir vos projets en https:// sans avertissement du navigateur.";
  } else if (!s.ca_trusted) {
    t.textContent = "mkcert est installé, mais son autorité locale n'est pas encore approuvée : les navigateurs afficheront un avertissement. L'étape suivante ajoute cette autorité à vos autorités racines (Windows affichera une fenêtre de confirmation, à accepter).";
    b.hidden = false;
  } else {
    t.textContent = `HTTPS prêt. Sur la page Projets, « Activer HTTPS » crée le certificat et le vhost ; adresse : https://<projet>.test${s.ssl_port === 443 ? "" : ":" + s.ssl_port}/.`;
  }
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
      if (v.installed) {
        const d = el("button", "btn small", "Supprimer");
        d.disabled = busy;
        d.title = "Supprime ce composant de C:\\astral-dev\\bin (vos projets et les données MySQL ne sont pas touchés)";
        d.addEventListener("click", () => removeVersion(c, v));
        row.append(d);
      }
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

async function removeVersion(c, v) {
  const label = `${c.name} ${v.version}`;
  if (!confirm(`Supprimer « ${label} » ?\n\nLe dossier du composant est effacé de C:\\astral-dev\\bin (réinstallable à tout moment). Vos projets et les données MySQL ne sont pas touchés.\n\nArrêtez d'abord le service concerné.`)) return;
  try {
    const msg = await invoke("remove_component", { id: c.id, version: v.version });
    say(`✓ ${msg}`);
    toast(msg, false);
    $("comp-restart").hidden = false;
  } catch (e) { toast(e); }
  refresh();
}

export async function installRecommended(restartAfter, onlyIds = null) {
  if (!data) await refresh();
  const todo = [];
  for (const c of data?.components ?? []) {
    if (onlyIds && !onlyIds.includes(c.id)) continue;
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
  $("comp-cancel").disabled = false;
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
  const finished = p.total && p.received >= p.total;
  $("comp-status").textContent = finished
    ? `${p.id} ${p.version} : téléchargé (${mb(p.total)}) — vérification et installation en cours…`
    : `${p.id} ${p.version} : ${mb(p.received)}${p.total ? " / " + mb(p.total) : ""}`;
  $("comp-cancel").disabled = !!finished; // l'extraction ne s'interrompt plus
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
  loadHttps();
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
