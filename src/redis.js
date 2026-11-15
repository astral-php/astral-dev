// Page Redis : stats, explorateur (SCAN), détail, suppression, purge de préfixe.
// Tout le contenu venant de Redis est affiché via textContent (jamais innerHTML — SECURITE.md).
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
let cursor = "0";
let timer = null;

export function initRedis(toastFn) {
  toast = toastFn;
  $("redis-refresh").addEventListener("click", () => { refreshInfo(); search(true); });
  $("redis-search").addEventListener("submit", (e) => { e.preventDefault(); search(true); });
  $("redis-more").addEventListener("click", () => search(false));
  $("redis-purge").addEventListener("click", purge);
  $("redis-cmd-form").addEventListener("submit", runCmd);
  $("redis-adv").addEventListener("change", (e) => {
    if (e.target.checked && !confirm("Activer le mode avancé ?\nLes commandes d'écriture et dangereuses (FLUSHALL, CONFIG, SHUTDOWN…) seront exécutées sans autre confirmation.")) e.target.checked = false;
  });
}

export function onRedisShown(shown) {
  clearInterval(timer);
  if (!shown) return;
  refreshInfo();
  search(true);
  timer = setInterval(refreshInfo, 2000); // F-046 : stats ≤ 2 s
}

function fmtUptime(s) {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d} j ${h} h` : h ? `${h} h ${m} min` : `${m} min`;
}

async function refreshInfo() {
  try {
    const i = await invoke("redis_info");
    $("redis-offline").hidden = true;
    $("redis-online").hidden = false;
    $("redis-sub").textContent = `Redis ${i.version} · 127.0.0.1 · actif depuis ${fmtUptime(i.uptime_seconds)}`;
    const total = i.keyspace_hits + i.keyspace_misses;
    const rate = total ? `${Math.round((i.keyspace_hits / total) * 100)} %` : "—";
    const tiles = [
      ["Mémoire", i.used_memory_human], ["Clients", String(i.connected_clients)],
      ["Hit rate", rate], ["Ops/s", String(i.ops_per_sec)], ["Clés", String(i.dbsize)],
    ];
    const root = $("redis-tiles");
    root.replaceChildren(...tiles.map(([k, v]) => {
      const t = el("div", "tile");
      t.append(el("small", null, k), el("strong", null, v));
      return t;
    }));
  } catch {
    $("redis-offline").hidden = false;
    $("redis-online").hidden = true;
    $("redis-sub").textContent = "arrêté";
  }
}

function fmtTtl(t) { return t === -1 ? "∞" : t === -2 ? "—" : t < 120 ? `${t} s` : t < 7200 ? `${Math.round(t / 60)} min` : `${Math.round(t / 3600)} h`; }

async function search(reset) {
  if (reset) { cursor = "0"; $("redis-keys").replaceChildren(); }
  try {
    const page = await invoke("redis_scan", { pattern: $("redis-pattern").value || "*", cursor, count: 200 });
    cursor = page.cursor;
    const body = $("redis-keys");
    for (const k of page.keys) {
      const tr = el("tr");
      tr.append(el("td", "mono", k.key), el("td", null, k.kind), el("td", null, fmtTtl(k.ttl)));
      tr.addEventListener("click", () => show(k.key));
      body.append(tr);
    }
    $("redis-more").hidden = cursor === "0";
  } catch (e) { if (reset) toast(e); }
}

async function show(key) {
  const box = $("redis-detail");
  try {
    const d = await invoke("redis_get", { key });
    box.replaceChildren();
    box.append(el("h3", "mono", d.key));
    box.append(el("p", "muted", `${d.kind} · ${d.size} élément(s)/octet(s) · TTL ${fmtTtl(d.ttl)}${d.truncated ? " · tronqué" : ""}`));
    const v = typeof d.value === "string" ? d.value : JSON.stringify(d.value, null, 2);
    box.append(el("pre", null, v));
    const del = el("button", "btn small danger", "Supprimer la clé");
    del.addEventListener("click", async () => {
      if (!confirm(`Supprimer « ${d.key} » ?`)) return;
      try { await invoke("redis_del", { keys: [d.key] }); box.replaceChildren(el("p", "muted", "Clé supprimée.")); search(true); refreshInfo(); } catch (e) { toast(e); }
    });
    box.append(del);
  } catch (e) { toast(e); }
}

async function purge() {
  const prefix = $("redis-prefix").value.trim();
  if (!prefix) return toast("Saisissez un préfixe explicite (la purge totale est refusée).");
  if (!confirm(`Supprimer toutes les clés commençant par « ${prefix} » ?\nOpération irréversible.`)) return;
  try {
    const n = await invoke("redis_flush_prefix", { prefix });
    toast(`${n} clé(s) supprimée(s).`, false);
    search(true); refreshInfo();
  } catch (e) { toast(e); }
}
async function runCmd(ev) {
  ev.preventDefault();
  const line = $("redis-cmd").value.trim();
  if (!line) return;
  const out = $("redis-cmd-out");
  try {
    const res = await invoke("redis_exec", { line, advanced: $("redis-adv").checked });
    out.textContent = `> ${line}\n${res}`;
    refreshInfo();
    if ($("redis-adv").checked) search(true);
  } catch (e) {
    out.textContent = `> ${line}\n(erreur) ${typeof e === "string" ? e : (e?.message ?? JSON.stringify(e))}`;
  }
}