// Pages Logs et Réglages (v0.2, lot 7.8). Tout le contenu affiché passe par textContent.
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
let timer = null;
let current = null;

export function initPages(toastFn) {
  toast = toastFn;
  $("logs-refresh").addEventListener("click", loadLogs);
  $("logs-filter").addEventListener("input", renderLog);
  $("logs-follow").addEventListener("change", () => { if ($("logs-follow").checked) tail(); });
  $("settings-form").addEventListener("submit", save);
  $("settings-open").addEventListener("click", () => invoke("open_config").catch(toast));
  $("settings-restart").addEventListener("click", () => invoke("restart_app"));
}

export function onLogsShown(shown) {
  clearInterval(timer);
  if (!shown) return;
  loadLogs();
  timer = setInterval(() => { if ($("logs-follow").checked) tail(); }, 2000);
}

export function onSettingsShown(shown) {
  if (shown) loadSettings();
}

// ---------- Logs ----------

let lines = [];

async function loadLogs() {
  let files;
  try { files = await invoke("list_logs"); } catch (e) { return toast(e); }
  const sel = $("logs-file");
  const keep = current;
  sel.replaceChildren(...files.map(([n, size]) => {
    const o = el("option", null, `${n} (${(size / 1024).toFixed(0)} Ko)`);
    o.value = n;
    return o;
  }));
  sel.onchange = () => { current = sel.value; tail(); };
  if (files.length === 0) {
    lines = [];
    $("logs-body").textContent = "(aucun journal : démarrez un service)";
    return;
  }
  current = files.some(([n]) => n === keep) ? keep : files[0][0];
  sel.value = current;
  tail();
}

async function tail() {
  if (!current) return;
  try { lines = await invoke("tail_log", { name: current, lines: 500 }); } catch (e) { return toast(e); }
  renderLog();
}

function renderLog() {
  const f = $("logs-filter").value.trim().toLowerCase();
  const shown = f ? lines.filter((l) => l.toLowerCase().includes(f)) : lines;
  const body = $("logs-body");
  const bottom = body.scrollTop + body.clientHeight >= body.scrollHeight - 20;
  body.textContent = shown.length ? shown.join("\n") : "(vide)";
  if (bottom) body.scrollTop = body.scrollHeight;
}

// ---------- Réglages ----------

async function loadSettings() {
  let s;
  try { s = await invoke("settings_get"); } catch (e) { return toast(e); }
  const f = $("settings-form");
  const php = f.elements["php_version"];
  const versions = s.php_installed.length ? s.php_installed : [s.php_version];
  php.replaceChildren(...versions.map((v) => { const o = el("option", null, v); o.value = v; return o; }));
  if (!versions.includes(s.php_version)) { const o = el("option", null, s.php_version); o.value = s.php_version; php.append(o); }
  php.value = s.php_version;
  for (const k of ["web_port", "mysql_port", "redis_port", "mailpit_http", "mailpit_smtp", "php_workers"]) f.elements[k].value = s[k];
  f.elements["redis_version"].value = s.redis_version;
  f.elements["web_mode"].value = s.web_mode;
  f.elements["web_mode"].disabled = !s.standalone;
  f.elements["apache_port"].value = s.apache_port;
  f.elements["caddy_admin_port"].value = s.caddy_admin_port;
  const hint = $("web-mode-hint");
  const needCaddy = s.web_mode !== "apache" && !s.caddy_installed;
  hint.textContent = !s.standalone
    ? "Mode adopt (Laragon) : seul Apache est utilisé."
    : needCaddy ? "⚠ Caddy n'est pas installé : installez-le depuis la page Composants, sinon le serveur web ne démarrera pas."
    : s.web_mode === "caddy+apache" ? "Caddy sert le HTTPS et relaie vers Apache (port interne) pour les sites qui ont besoin de .htaccess ; le choix se fait par projet (page Projets)."
    : s.web_mode === "caddy" ? "Caddy seul : HTTPS avec les certificats mkcert, rechargement sans coupure. Les .htaccess ne sont pas lus."
    : "Apache seul : .htaccess pris en charge, PHP en FastCGI.";
  hint.className = needCaddy ? "warn hint" : "muted hint";
  f.elements["close_to_tray"].checked = s.close_to_tray;
  f.elements["start_minimized"].checked = s.start_minimized;
  f.elements["autostart_services"].checked = s.autostart_services;
  f.elements["mailpit_window"].checked = s.mailpit_window;
  $("settings-info").textContent = `Fichier : ${s.file ?? "—"} · domaines : *.${s.tld} · projets : ${s.www}`;
  $("settings-hint").hidden = s.standalone;
  $("settings-hint").textContent = s.standalone ? "" : "Mode adopt (Laragon) : la version de PHP est celle de Laragon ; seuls les ports et l'interface sont lus ici.";
}

async function save(e) {
  e.preventDefault();
  const f = e.target.elements;
  const n = (k) => Number.parseInt(f[k].value, 10);
  const input = {
    php_version: f["php_version"].value,
    web_port: n("web_port"), mysql_port: n("mysql_port"), redis_port: n("redis_port"),
    mailpit_http: n("mailpit_http"), mailpit_smtp: n("mailpit_smtp"),
    php_workers: n("php_workers"), redis_version: f["redis_version"].value,
    web_mode: f["web_mode"].disabled ? null : f["web_mode"].value,
    apache_port: n("apache_port") || null, caddy_admin_port: n("caddy_admin_port") || null,
    close_to_tray: f["close_to_tray"].checked, start_minimized: f["start_minimized"].checked,
    autostart_services: f["autostart_services"].checked,
    mailpit_window: f["mailpit_window"].checked,
  };
  try {
    await invoke("settings_save", { input });
    toast("Réglages enregistrés. Redémarrez astral-dev pour les appliquer.", false);
    $("settings-restart").hidden = false;
  } catch (err) { toast(err); }
}
