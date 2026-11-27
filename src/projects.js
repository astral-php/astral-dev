// Page Projets : liste, création (Composer), console Astral (liste blanche), exposition .local.
// Contenu affiché via textContent uniquement.
const T = () => window.__TAURI__;
const invoke = (c, a) => T().core.invoke(c, a);
const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

const COMMANDS = ["list", "migrate", "migrate:status", "migrate:rollback", "cache:clear", "make:module", "help"];
let toast = (m) => console.error(m);
let currentJob = null;
let currentProject = null;
let onJobDone = null;
let ficheName = null;
let consoleUi = "job"; // « job » : fenêtre console ; « fiche » : console intégrée à la fiche
const cu = (s) => $(`${consoleUi}-${s}`);
let lastList = [];
let lastPhp = { installed: [], default: null, standalone: false, web_mode: "apache" };

export function initProjects(toastFn) {
  toast = toastFn;
  $("projects-refresh").addEventListener("click", load);
  $("project-new").addEventListener("click", openNew);
  $("projects-www").addEventListener("click", () => invoke("open_folder", { target: "www" }).catch(toast));
  $("fiche-close").addEventListener("click", () => $("fiche").close());
  $("fiche-open").addEventListener("click", () => invoke("open_site", { name: ficheName }).catch(toast));
  $("fiche-folder").addEventListener("click", () => invoke("open_folder", { target: ficheName }).catch(toast));
  $("fiche-www").addEventListener("click", () => invoke("open_folder", { target: "www" }).catch(toast));
  $("fiche-run").addEventListener("click", () => runConsole("fiche"));
  $("fiche-args").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); runConsole("fiche"); } });
  $("fiche").addEventListener("close", () => { ficheName = null; });
  $("np-template").addEventListener("change", updateTemplateHint);
  $("np-cancel").addEventListener("click", () => $("new-project").close());
  $("new-project-form").addEventListener("submit", createProject);
  $("job-close").addEventListener("click", () => $("job").close());
  $("cache-close").addEventListener("click", () => $("cache").close());
  $("job-run").addEventListener("click", () => runConsole("job"));
  for (const id of ["job-cmd", "fiche-cmd"]) $(id).replaceChildren(...COMMANDS.map((c) => { const o = el("option", null, c); o.value = c; return o; }));
  // F-016 : un nouveau dossier apparaît sous www/
  T().event.listen("www-new-project", (e) => {
    toast(`Nouveau dossier détecté : ${e.payload}`, false);
    if (!$("view-projects").hidden) load();
  });
  T().event.listen("console-line", (e) => {
    if (e.payload.job !== currentJob) return;
    const out = cu("out");
    out.textContent += (e.payload.stderr ? "! " : "") + e.payload.line + "\n";
    if (out.textContent.length > 400000) out.textContent = out.textContent.slice(-300000);
    out.scrollTop = out.scrollHeight;
  });
  T().event.listen("console-done", (e) => {
    if (e.payload.job !== currentJob) return;
    cu("status").textContent = e.payload.error ? `Échec : ${e.payload.error}` : e.payload.code === 0 ? "Terminé ✓" : `Terminé avec le code ${e.payload.code}`;
    cu("run").disabled = false;
    currentJob = null;
    if (onJobDone) onJobDone(e.payload.code);
  });
}

export function onProjectsShown(shown) { if (shown) load(); }

// state : true (vert) | false (gris) | "warn" (orange : configuré mais pas de confiance côté Windows)
function badge(text, state, title) {
  const b = el("span", "badge " + (state === "warn" ? "warn" : state ? "ok" : "off"), text);
  if (title) b.title = title;
  return b;
}

let httpsAvailable = false;
let caTrusted = false;
let sslPort = 443;

async function load() {
  try {
    try {
      const m = await invoke("mkcert_status");
      httpsAvailable = m.installed;
      caTrusted = m.ca_trusted;
      sslPort = m.ssl_port ?? 443;
    } catch (_) { httpsAvailable = false; caTrusted = false; }
    const list = await invoke("list_projects");
    let php = { installed: [], default: null, standalone: false, web_mode: "apache" };
    try { php = await invoke("list_php_versions"); } catch (_) { /* mode adopt : pas de sélecteur */ }
    lastList = list;
    lastPhp = php;
    $("projects-sub").textContent = `${list.length} dossiers · ${list.filter((p) => p.astral).length} projets Astral`;
    const body = $("projects-body");
    body.replaceChildren();
    for (const p of list) {
      const tr = el("tr");
      tr.append(el("td", "mono", p.name));
      tr.append(el("td", null, p.package ? `${p.package.replace("astral-php/", "")}${p.version ? " " + p.version : ""}` : "—"));
      tr.append(el("td", "mono", p.domain));
      tr.append(phpCell(p, php));
      tr.append(backendCell(p, php));
      tr.append(el("td", null, p.cache_driver ?? "—"));
      const acc = el("td");
      acc.append(badge("vhost", p.vhost), " ", badge("hosts", p.hosts), " ", badge(p.https && !caTrusted ? "https ⚠" : "https", p.https ? (caTrusted ? true : "warn") : false,
        p.https ? (caTrusted ? "HTTPS actif et approuvé par Windows" : "HTTPS configuré, mais l'autorité locale n'est pas approuvée : le navigateur affichera un avertissement (page Composants → HTTPS local)") : "HTTPS non configuré pour ce projet"), " ", badge("redis", p.cache_driver === "redis"));
      tr.append(acc);
      const act = el("td", "acts");
      const box = el("div", "row");
      act.append(box);
      const fb = el("button", "btn small primary", "Fiche");
      fb.title = "Ouvre la fiche du projet : site, serveur, PHP, HTTPS, cache, outils";
      fb.addEventListener("click", () => openFiche(p.name));
      box.append(fb);
      if (p.astral) {
        const c = el("button", "btn small", "Console");
        c.addEventListener("click", () => openConsole(p.name));
        const k = el("button", "btn small" + (p.cache_driver === "redis" ? " on" : ""), p.cache_driver === "redis" ? "Cache Redis ✓" : "Cache Redis");
        k.title = p.cache_driver === "redis" ? "Le cache Redis est actif pour ce projet (CACHE_DRIVER=redis)" : "Utiliser Redis pour le cache de ce projet";
        k.addEventListener("click", () => openCache(p.name));
        box.append(c, k);
      }
      if (!(p.vhost && p.hosts)) {
        const x = el("button", "btn small", "Activer");
        x.title = "Crée la configuration du serveur web et la ligne hosts du projet";
        x.addEventListener("click", () => expose(p.name, p.domain));
        box.append(x);
      }
      const hb = httpsButton(p);
      if (hb) box.append(hb);
      if (p.vhost || p.hosts) {
        const r = el("button", "btn small", "Retirer");
        r.title = "Retire le vhost, le certificat et la ligne hosts (les fichiers du projet sont conservés)";
        r.addEventListener("click", () => unexpose(p.name, p.domain));
        box.append(r);
      }
      tr.append(act);
      body.append(tr);
    }
    if (ficheName && $("fiche").open) renderFiche();
  } catch (e) { toast(e); }
}

function httpsButton(p) {
  if (!(p.vhost && httpsAvailable)) return null;
  const h = el("button", "btn small" + (p.https ? (caTrusted ? " on" : " warn") : ""), p.https ? (caTrusted ? "HTTPS ✓" : "HTTPS ⚠") : "HTTPS");
  h.title = p.https
    ? (caTrusted ? "HTTPS actif pour ce projet (cliquer pour régénérer le certificat)" : "HTTPS configuré mais autorité locale non approuvée : installez-la depuis la page Composants (HTTPS local) pour supprimer l'avertissement du navigateur")
    : "Crée le certificat mkcert et active HTTPS pour ce projet";
  // HTTPS inactif : activer ; HTTPS actif : boîte de dialogue (adresse, régénérer, DÉSACTIVER).
  h.addEventListener("click", () => (p.https ? openHttpsDialog(p.name, p.domain) : enableHttps(p.name)));
  return h;
}

// ---------- Phase 5 : fiche projet (ADR-020) ----------
function openFiche(name) {
  ficheName = name;
  consoleUi = "fiche";
  $("fiche-out").textContent = "";
  $("fiche-status").textContent = "";
  $("fiche-args").value = "";
  $("fiche-run").disabled = false;
  renderFiche();
  $("fiche").showModal();
}

function quickCommand(cmd) {
  $("fiche-cmd").value = cmd;
  $("fiche-args").value = "";
  runConsole("fiche");
}

function renderFiche() {
  const p = lastList.find((x) => x.name === ficheName);
  if (!p) { $("fiche").close(); return; }
  const live = p.vhost && p.hosts;
  const url = `${p.https ? "https" : "http"}://${p.domain}${p.https && sslPort !== 443 ? ":" + sslPort : ""}`;
  $("fiche-title").textContent = p.name;
  $("fiche-url").textContent = url;
  $("fiche-sub").textContent = p.package ? p.package.replace("astral-php/", "") + (p.version ? " " + p.version : "") : "Projet PHP";
  const open = $("fiche-open");
  open.disabled = !live;
  open.title = live ? "Ouvre le site dans le navigateur par défaut" : "Activez d'abord le projet (crée la configuration du serveur web et la ligne hosts)";
  const field = (label, ...nodes) => { const f = el("div", "fiche-field"); f.append(el("span", null, label), ...nodes); return f; };
  // Configuration technique : serveur, version de PHP
  $("fiche-config").replaceChildren(
    field("Serveur", ...Array.from(backendCell(p, lastPhp).childNodes)),
    field("Version de PHP", ...Array.from(phpCell(p, lastPhp).childNodes)),
  );
  // Options : HTTPS, cache
  const hb = httpsButton(p);
  const cacheNodes = [el("span", "mono", p.cache_driver ?? "—")];
  if (p.astral) {
    const k = el("button", "btn small" + (p.cache_driver === "redis" ? " on" : ""), "Cache Redis…");
    k.addEventListener("click", () => openCache(p.name));
    cacheNodes.push(k);
  }
  $("fiche-options").replaceChildren(
    field("HTTPS", hb ?? el("span", "muted", p.vhost ? "mkcert non installé (page Composants)" : "Activez d'abord le projet")),
    field("Cache", ...cacheNodes),
  );
  // Console : outils sans argument en un clic ; la console elle-même reste en place
  $("fiche-console").hidden = !p.astral;
  if (p.astral && !$("fiche-quick").childElementCount) {
    const group = (label, cmds) => {
      const g = el("div", "grp");
      g.append(el("span", null, label));
      for (const c of cmds) { const b = el("button", "btn small", c); b.addEventListener("click", () => quickCommand(c)); g.append(b); }
      return g;
    };
    $("fiche-quick").append(group("Général", ["list", "cache:clear"]), group("Base de données", ["migrate", "migrate:rollback", "migrate:status"]));
  }
  // Pied : Retirer / Activer, en bas à droite
  const foot = $("fiche-foot");
  foot.replaceChildren();
  if (p.vhost || p.hosts) {
    const r = el("button", "btn small", "Retirer");
    r.title = "Retire la configuration du serveur web, le certificat et la ligne hosts (les fichiers du projet sont conservés)";
    r.addEventListener("click", () => { $("fiche").close(); unexpose(p.name, p.domain); });
    foot.append(r);
  }
  if (!live) {
    const x = el("button", "btn small primary", "Activer");
    x.title = "Crée la configuration du serveur web et la ligne hosts du projet";
    x.addEventListener("click", () => expose(p.name, p.domain));
    foot.append(x);
  }
  foot.hidden = !foot.childElementCount;
}

// Version de PHP du projet : sélecteur pour un site exposé (mode autonome), texte sinon.
function phpCell(p, php) {
  const td = el("td", "mono");
  if (!php.standalone || !php.installed.length) { td.textContent = "—"; return td; }
  if (!p.vhost) { td.textContent = php.default ? `${php.default} (défaut)` : "—"; td.title = "Activez le projet pour choisir sa version de PHP"; return td; }
  const sel = document.createElement("select");
  sel.className = "btn small";
  const def = document.createElement("option");
  def.value = ""; def.textContent = `Par défaut (${php.default ?? "?"})`;
  sel.append(def);
  for (const v of php.installed) { const o = document.createElement("option"); o.value = v; o.textContent = `PHP ${v}`; sel.append(o); }
  sel.value = p.php_own ? p.php : "";
  sel.title = "Version de PHP de ce site (console, Composer et serveur web)";
  sel.addEventListener("change", async () => {
    sel.disabled = true;
    try {
      const r = await invoke("set_project_php", { name: p.name, series: sel.value });
      toast(`PHP ${sel.value || "par défaut"} pour « ${p.name} »${r.restart_needed ? await restartApache() : ""}`, false);
    } catch (e) { toast(e); }
    load();
  });
  td.append(sel);
  return td;
}

// Serveur du projet : sélecteur Caddy / Apache en topologie « Caddy ➔ Apache », nom du serveur sinon.
function backendCell(p, php) {
  const td = el("td", "mono");
  const mode = php.web_mode ?? "apache";
  if (mode !== "caddy+apache") { td.textContent = mode === "caddy" ? "Caddy" : "Apache"; return td; }
  if (!p.vhost) { td.textContent = "—"; td.title = "Activez le projet pour choisir son serveur"; return td; }
  const sel = document.createElement("select");
  sel.className = "btn small";
  for (const [v, t] of [["apache", "Apache (.htaccess)"], ["caddy", "Caddy"]]) { const o = document.createElement("option"); o.value = v; o.textContent = t; sel.append(o); }
  sel.value = p.backend ?? "apache";
  sel.title = "Serveur qui exécute ce site : Apache lit les .htaccess, Caddy sert PHP directement";
  sel.addEventListener("change", async () => {
    sel.disabled = true;
    try {
      const r = await invoke("set_project_backend", { name: p.name, backend: sel.value });
      toast(`« ${p.name} » servi par ${sel.value === "caddy" ? "Caddy" : "Apache"}${r.restart_needed ? await restartApache() : ""}`, false);
    } catch (e) { toast(e); }
    load();
  });
  td.append(sel);
  return td;
}

function updateTemplateHint() {
  $("np-hint").textContent = $("np-template").value === "vide"
    ? "Crée www/<nom>/public/index.php (phpinfo), sans Composer. Minuscules, chiffres et tirets."
    : "Créé dans www/ via composer create-project. Minuscules, chiffres et tirets.";
}

async function openNew() {
  try {
    const t = await invoke("list_templates");
    const opts = t.map(([k, pkg]) => { const o = el("option", null, `${k} — ${pkg}`); o.value = k; return o; });
    const blank = el("option", null, "vide — projet PHP vide (sans Composer)"); blank.value = "vide";
    $("np-template").replaceChildren(...opts, blank);
    updateTemplateHint();
    $("new-project").showModal();
  } catch (e) { toast(e); }
}

function openJob(title, withControls) {
  consoleUi = "job";
  $("job-title").textContent = title;
  $("job-controls").hidden = !withControls;
  $("job-out").textContent = "";
  $("job-status").textContent = "";
  $("job").showModal();
}

async function createProject(ev) {
  ev.preventDefault();
  const name = $("np-name").value.trim();
  const template = $("np-template").value;
  $("new-project").close();
  if (template === "vide") {
    try { await invoke("create_blank_project", { name }); toast(`Projet vide « ${name} » créé dans www/ — activez-le pour obtenir son adresse`, false); load(); }
    catch (e) { toast(e); }
    return;
  }
  openJob(`Création de « ${name} » (${template})`, false);
  $("job-status").textContent = "Composer en cours…";
  onJobDone = (code) => { onJobDone = null; if (code === 0) load(); };
  try { currentJob = await invoke("create_project", { name, template }); }
  catch (e) { $("job-status").textContent = typeof e === "string" ? e : (e?.message ?? "Erreur"); toast(e); }
}

function openConsole(project) {
  currentProject = project;
  onJobDone = null;
  openJob(`Console Astral — ${project}`, true);
}

async function runConsole(ui = "job") {
  consoleUi = ui;
  if (ui === "fiche") currentProject = ficheName;
  const extra = cu("args").value.trim().split(/\s+/).filter(Boolean);
  const args = [cu("cmd").value, ...extra];
  cu("out").textContent = `$ php bin/console ${args.join(" ")}\n`;
  cu("status").textContent = "En cours…";
  cu("run").disabled = true;
  try { currentJob = await invoke("run_console", { project: currentProject, args }); }
  catch (e) { cu("run").disabled = false; cu("status").textContent = ""; toast(e); }
}

function unexpose(name, domain) {
  const d = $("unexpose");
  $("ux-title").textContent = `Retirer « ${domain} »`;
  $("ux-delete").checked = false;
  $("ux-confirm").hidden = true;
  $("ux-name").value = "";
  $("ux-delete").onchange = () => { $("ux-confirm").hidden = !$("ux-delete").checked; };
  $("ux-cancel").onclick = () => d.close();
  $("ux-ok").onclick = async () => {
    const del = $("ux-delete").checked;
    if (del && $("ux-name").value.trim() !== name) return toast(`Retapez exactement « ${name} » pour confirmer la suppression du dossier.`);
    d.close();
    try {
      const r = await invoke("unexpose_project", { name, deleteFiles: del });
      let msg = `vhost : ${r.vhost} · hosts : ${r.hosts} · dossier : ${r.files}`;
      if (r.restart_needed) msg += await restartApache();
      toast(msg, false);
      load();
    } catch (e) { toast(e); }
  };
  d.showModal();
}

// Le moteur web prend en compte la nouvelle configuration : rechargement à chaud (sans couper les sites),
// repli sur un redémarrage, démarrage s'il est arrêté.
async function restartApache() {
  try {
    const r = await invoke("reload_web");
    return { reload: " · serveur web rechargé sans coupure", restart: " · serveur web redémarré", start: " · serveur web démarré" }[r.mode] ?? "";
  } catch (e) { return " — redémarrez le serveur web pour l'appliquer"; }
}

function openHttpsDialog(name, domain) {
  const d = $("https-dlg");
  const url = `https://${domain}${sslPort === 443 ? "" : ":" + sslPort}/`;
  $("hd-title").textContent = `HTTPS actif — ${name}`;
  $("hd-text").textContent = `Adresse : ${url} · ` + (caTrusted
    ? "autorité locale approuvée : aucun avertissement du navigateur."
    : "autorité locale NON approuvée : le navigateur affichera un avertissement (page Composants → HTTPS local).");
  $("hd-regen").onclick = () => { d.close(); enableHttps(name); };
  $("hd-off").onclick = async () => {
    if (!confirm(`Désactiver HTTPS pour « ${name} » ?\n\nLe site restera accessible en http://. Le certificat est conservé : vous pourrez réactiver HTTPS à tout moment.`)) return;
    d.close();
    try {
      await invoke("disable_https", { name });
      toast(`HTTPS désactivé pour « ${name} »${await restartApache()}`, false);
      load();
    } catch (e) { toast(e); }
  };
  $("hd-close").onclick = () => d.close();
  d.showModal();
}

async function enableHttps(name) {
  try {
    const r = await invoke("enable_https", { name });
    const warn = r.ca_trusted ? "" : " (autorité mkcert non approuvée : voir la page Composants)";
    if (r.app_restart_needed) {
      if (confirm(`HTTPS prêt pour « ${name} » : ${r.url}${warn}\n\nUn redémarrage de Rustral Studio est nécessaire pour ouvrir le port HTTPS. Redémarrer maintenant ?`)) await invoke("restart_app");
      else toast(`HTTPS configuré : ${r.url} — redémarrez Rustral Studio pour l'activer${warn}`, false);
    } else {
      toast(`HTTPS : ${r.url}${warn}${await restartApache()}`, false);
    }
  } catch (e) { toast(e); }
}

async function expose(name, domain) {
  if (!confirm(`Activer « ${domain} » ?\n\n• crée la configuration du serveur web (sans écraser l'existant)\n• ajoute ${domain} au fichier hosts : Windows va demander une autorisation administrateur (UAC).`)) return;
  try {
    const r = await invoke("expose_project", { name });
    toast(`vhost : ${r.vhost} · hosts : ${r.hosts} · SSL : ${r.ssl}${r.restart_needed ? await restartApache() : ""}`, false);
    load();
  } catch (e) { toast(e); }
}
// ---------- Phase 4 : « Utiliser Redis pour ce projet » ----------
const SUPPORT = { "astral-cache": "astral-cache installé", integre: "cache intégré au projet", absent: "aucun cache compatible" };

function check(ok, text, warn = false) {
  const li = el("li", ok ? "ok" : warn ? "warn" : "ko");
  li.append(el("span", "mark", ok ? "✓" : warn ? "!" : "✗"), document.createTextNode(" " + text));
  return li;
}

async function openCache(project) {
  currentProject = project;
  $("cache-title").textContent = `Cache Redis — ${project}`;
  try {
    const s = await invoke("cache_status", { project });
    const items = [
      check(s.astral, "Projet Astral (bin/console)"),
      check(s.has_env, ".env présent"),
      check(s.redis_up, `Redis répond sur 127.0.0.1:${s.redis_port}`),
      check(s.ext_redis, "Extension PHP redis (ext-redis) dans le PHP actif"),
      check(s.support !== "absent", `Support cache : ${SUPPORT[s.support] ?? s.support}`),
    ];
    if (s.support === "astral-cache") items.push(check(s.provider_registered, "CacheServiceProvider enregistré (config/dependencies.php)", true));
    items.push(el("li", "info", `CACHE_DRIVER actuel : ${s.driver ?? "(non défini)"}`));
    $("cache-checks").replaceChildren(...items);
    $("cache-hint").textContent = s.support === "astral-cache" && !s.provider_registered
      ? "« Activer » enregistrera Astral\\Cache\\CacheServiceProvider dans config/dependencies.php (sauvegarde config/dependencies.php.astral-dev.bak)."
      : "Le .env d'origine est sauvegardé une fois dans .env.astral-dev.bak.";
    const acts = [];
    if (s.driver === "redis") {
      const b = el("button", "btn small", "Revenir à file");
      b.addEventListener("click", () => disableCache(project));
      acts.push(b);
    }
    if (s.support === "absent") {
      const b = el("button", "btn small", "Installer astral-cache");
      b.disabled = !s.astral;
      b.addEventListener("click", () => installCachePackage(project));
      acts.push(b);
    }
    if (s.driver === "redis") {
      const t = el("button", "btn small", "Tester le cache");
      t.title = "PHP écrit puis relit une clé (astral:cache:astral-dev-test) avec les paramètres du .env";
      t.addEventListener("click", async () => {
        t.disabled = true;
        try { toast(await invoke("cache_test", { project }), false); } catch (e) { toast(e); }
        t.disabled = false;
      });
      acts.push(t);
    }
    const on = el("button", "btn small primary", s.driver === "redis" ? "Réappliquer" : "Activer Redis");
    on.disabled = !(s.astral && s.has_env && s.redis_up && s.ext_redis && s.support !== "absent");
    on.addEventListener("click", () => enableCache(project));
    acts.push(on);
    $("cache-actions").replaceChildren(...acts);
    if (!$("cache").open) $("cache").showModal();
  } catch (e) { toast(e); }
}

async function enableCache(project) {
  if (!confirm(`Activer Redis pour « ${project} » ?\n\nLe .env sera modifié : CACHE_DRIVER=redis et REDIS_* (sauvegarde dans .env.astral-dev.bak), puis « cache:clear » sera lancé.`)) return;
  try {
    await invoke("cache_enable", { project });
    $("cache").close();
    currentProject = project;
    onJobDone = null;
    openJob(`Activation Redis — ${project}`, false);
    $("job-out").textContent = "$ php bin/console cache:clear\n";
    $("job-status").textContent = "En cours…";
    currentJob = await invoke("run_console", { project, args: ["cache:clear"] });
    onJobDone = () => { onJobDone = null; load(); };
  } catch (e) { toast(e); }
}

async function disableCache(project) {
  if (!confirm(`Remettre le cache de « ${project} » sur son driver d'origine (file) ?`)) return;
  try {
    const d = await invoke("cache_disable", { project });
    toast(`CACHE_DRIVER=${d}`, false);
    load();
    openCache(project);
  } catch (e) { toast(e); }
}

async function installCachePackage(project) {
  if (!confirm(`Lancer « composer require astral-php/astral-cache » dans ${project} ?`)) return;
  $("cache").close();
  openJob(`composer require astral-cache — ${project}`, false);
  $("job-status").textContent = "Composer en cours…";
  onJobDone = () => { onJobDone = null; $("job").close(); openCache(project); };
  try { currentJob = await invoke("add_cache_package", { project }); } catch (e) { toast(e); }
}