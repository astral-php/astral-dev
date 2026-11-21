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

export function initProjects(toastFn) {
  toast = toastFn;
  $("projects-refresh").addEventListener("click", load);
  $("project-new").addEventListener("click", openNew);
  $("np-cancel").addEventListener("click", () => $("new-project").close());
  $("new-project-form").addEventListener("submit", createProject);
  $("job-close").addEventListener("click", () => $("job").close());
  $("cache-close").addEventListener("click", () => $("cache").close());
  $("job-run").addEventListener("click", runConsole);
  $("job-cmd").replaceChildren(...COMMANDS.map((c) => { const o = el("option", null, c); o.value = c; return o; }));
  // F-016 : un nouveau dossier apparaît sous www/
  T().event.listen("www-new-project", (e) => {
    toast(`Nouveau dossier détecté : ${e.payload}`, false);
    if (!$("view-projects").hidden) load();
  });
  T().event.listen("console-line", (e) => {
    if (e.payload.job !== currentJob) return;
    const out = $("job-out");
    out.textContent += (e.payload.stderr ? "! " : "") + e.payload.line + "\n";
    if (out.textContent.length > 400000) out.textContent = out.textContent.slice(-300000);
    out.scrollTop = out.scrollHeight;
  });
  T().event.listen("console-done", (e) => {
    if (e.payload.job !== currentJob) return;
    $("job-status").textContent = e.payload.error ? `Échec : ${e.payload.error}` : e.payload.code === 0 ? "Terminé ✓" : `Terminé avec le code ${e.payload.code}`;
    $("job-run").disabled = false;
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
    $("projects-sub").textContent = `${list.length} dossiers · ${list.filter((p) => p.astral).length} projets Astral`;
    const body = $("projects-body");
    body.replaceChildren();
    for (const p of list) {
      const tr = el("tr");
      tr.append(el("td", "mono", p.name));
      tr.append(el("td", null, p.package ? `${p.package.replace("astral-php/", "")}${p.version ? " " + p.version : ""}` : "—"));
      tr.append(el("td", "mono", p.domain));
      tr.append(el("td", null, p.cache_driver ?? "—"));
      const acc = el("td");
      acc.append(badge("vhost", p.vhost), " ", badge("hosts", p.hosts), " ", badge(p.https && !caTrusted ? "https ⚠" : "https", p.https ? (caTrusted ? true : "warn") : false,
        p.https ? (caTrusted ? "HTTPS actif et approuvé par Windows" : "HTTPS configuré, mais l'autorité locale n'est pas approuvée : le navigateur affichera un avertissement (page Composants → HTTPS local)") : "HTTPS non configuré pour ce projet"), " ", badge("redis", p.cache_driver === "redis"));
      tr.append(acc);
      const act = el("td");
      const box = el("div", "row");
      act.append(box);
      if (p.astral) {
        const c = el("button", "btn small", "Console");
        c.addEventListener("click", () => openConsole(p.name));
        const k = el("button", "btn small" + (p.cache_driver === "redis" ? " on" : ""), p.cache_driver === "redis" ? "Cache Redis ✓" : "Cache Redis");
        k.title = p.cache_driver === "redis" ? "Le cache Redis est actif pour ce projet (CACHE_DRIVER=redis)" : "Utiliser Redis pour le cache de ce projet";
        k.addEventListener("click", () => openCache(p.name));
        box.append(c, k);
      }
      if (!(p.vhost && p.hosts)) {
        const x = el("button", "btn small", "Exposer");
        x.addEventListener("click", () => expose(p.name, p.domain));
        box.append(x);
      }
      if (p.vhost && httpsAvailable) {
        const h = el("button", "btn small" + (p.https ? (caTrusted ? " on" : " warn") : ""), p.https ? (caTrusted ? "HTTPS ✓" : "HTTPS ⚠") : "HTTPS");
        h.title = p.https
          ? (caTrusted ? "HTTPS actif pour ce projet (cliquer pour régénérer le certificat)" : "HTTPS configuré mais autorité locale non approuvée : installez-la depuis la page Composants (HTTPS local) pour supprimer l'avertissement du navigateur")
          : "Crée le certificat mkcert et ajoute le bloc HTTPS au vhost";
        // HTTPS inactif : activer ; HTTPS actif : boîte de dialogue (adresse, régénérer, DÉSACTIVER).
        h.addEventListener("click", () => (p.https ? openHttpsDialog(p.name, p.domain) : enableHttps(p.name)));
        box.append(h);
      }
      if (p.vhost || p.hosts) {
        const r = el("button", "btn small", "Retirer");
        r.title = "Retire le vhost, le certificat et la ligne hosts (les fichiers du projet sont conservés)";
        r.addEventListener("click", () => unexpose(p.name, p.domain));
        box.append(r);
      }
      tr.append(act);
      body.append(tr);
    }
  } catch (e) { toast(e); }
}

async function openNew() {
  try {
    const t = await invoke("list_templates");
    $("np-template").replaceChildren(...t.map(([k, pkg]) => { const o = el("option", null, `${k} — ${pkg}`); o.value = k; return o; }));
    $("new-project").showModal();
  } catch (e) { toast(e); }
}

function openJob(title, withControls) {
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

async function runConsole() {
  const extra = $("job-args").value.trim().split(/\s+/).filter(Boolean);
  const args = [$("job-cmd").value, ...extra];
  $("job-out").textContent = `$ php bin/console ${args.join(" ")}\n`;
  $("job-status").textContent = "En cours…";
  $("job-run").disabled = true;
  try { currentJob = await invoke("run_console", { project: currentProject, args }); }
  catch (e) { $("job-run").disabled = false; $("job-status").textContent = ""; toast(e); }
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
      if (confirm(`HTTPS prêt pour « ${name} » : ${r.url}${warn}\n\nUn redémarrage d'astral-dev est nécessaire pour ouvrir le port HTTPS. Redémarrer maintenant ?`)) await invoke("restart_app");
      else toast(`HTTPS configuré : ${r.url} — redémarrez astral-dev pour l'activer${warn}`, false);
    } else {
      toast(`HTTPS : ${r.url}${warn}${await restartApache()}`, false);
    }
  } catch (e) { toast(e); }
}

async function expose(name, domain) {
  if (!confirm(`Exposer « ${domain} » ?\n\n• crée le vhost Apache (sans écraser l'existant)\n• ajoute ${domain} au fichier hosts : Windows va demander une autorisation administrateur (UAC).`)) return;
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