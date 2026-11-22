"""Banc d'essai de l'interface sans Tauri : génère tests/ui/_mock.html (fausse API Tauri).

Usage, depuis la racine du dépôt :
    python tests/ui/make_mock.py
    python -m http.server 8765 --bind 127.0.0.1
Ouvrir http://127.0.0.1:8765/tests/ui/_mock.html, parcourir chaque page du menu, puis lire `window.__errors`
(doit rester vide). Leçon de la 0.7.0-dev : une erreur dans `show()` avait vidé toutes les pages.
_mock.html n'est pas versionné.
"""
import pathlib

root = pathlib.Path(__file__).resolve().parents[2]
t = (root / "src" / "index.html").read_text(encoding="utf-8")

mock = r"""<script>
const svc = (id, name, meta, state, extra = {}) => ({ id, name, meta, state, managed: true, orphan: false, external: false, conflict: null, error: null, runtime: false, pool: null, ...extra });
const MOCK = {
  list_services: () => [svc("caddy","Caddy","port 80 · caddy-v2","running"), svc("apache","Apache","port 8081 · httpd","running"), svc("php","PHP","8.3.33","running",{runtime:true}), svc("mysql","MySQL","port 3306","running"), svc("redis","Redis","127.0.0.1:6379","running"), svc("mailpit","Mailpit","SMTP 1025 · UI 8025","stopped"), svc("php-8.3-1","PHP 8.3 · worker 1","port 9083","running",{pool:"8.3"}), svc("php-8.3-2","PHP 8.3 · worker 2","port 9183","running",{pool:"8.3"})],
  startup_warnings: () => [],
  setup_status: () => ({ standalone: true, ready: true, missing: [], total: 6, laragon: null }),
  settings_get: () => ({ standalone: true, file: "C:/astral-dev/astral-dev.toml", php_version: "8.3", php_installed: ["8.3","8.4"], php_workers: 2, redis_version: "", web_mode: "caddy+apache", apache_port: 8081, caddy_admin_port: 2019, caddy_installed: true, web_port: 80, tld: "test", mysql_port: 3306, redis_port: 6379, mailpit_http: 8025, mailpit_smtp: 1025, close_to_tray: false, start_minimized: false, autostart_services: false, mailpit_window: true, www: "C:/astral-dev/www" }),
  mkcert_status: () => ({ installed: true, ca_trusted: true, ssl_port: 443 }),
  list_projects: () => [
    { name: "demo", astral: true, package: "astral-php/astral-starter", version: "1.0", domain: "demo.test", vhost: true, hosts: true, https: true, cache_driver: "redis", php: "8.3", php_own: false, backend: "apache" },
    { name: "htest", astral: false, package: null, version: null, domain: "htest.test", vhost: false, hosts: false, https: false, cache_driver: null, php: "8.3", php_own: false, backend: null }],
  list_php_versions: () => ({ default: "8.3", installed: ["8.3","8.4"], workers: 2, standalone: true, web_mode: "caddy+apache" }),
  list_templates: () => [["starter","astral-php/astral-starter"]],
  components_list: () => ({ components: [{ id: "caddy", name: "Caddy", required: false, versions: [{ version: "latest", installed: true, size: 0 }] }] }),
  list_logs: () => ["caddy.log", "apache.log"],
  tail_log: () => ["ligne 1", "ligne 2"],
  redis_info: () => ({ version: "5.0.14", uptime_seconds: 100, used_memory_human: "1K", connected_clients: 1, keyspace_hits: 1, keyspace_misses: 1, ops_per_sec: 1, dbsize: 3 }),
  redis_scan: () => ({ cursor: "0", keys: [{ key: "astral:cache:a", kind: "string", ttl: 60 }] }),
};
window.__calls = [];
window.__TAURI__ = { core: { invoke: async (c, a) => { window.__calls.push(c); if (MOCK[c]) return MOCK[c](a); return null; } }, event: { listen: async () => () => {} }, app: { getVersion: async () => "test" } };
window.__errors = [];
window.addEventListener("error", (e) => window.__errors.push(e.message));
window.addEventListener("unhandledrejection", (e) => window.__errors.push("promise: " + (e.reason?.message ?? e.reason)));
</script>
"""

t = t.replace('<script type="module" src="main.js"></script>', mock + '<script type="module" src="main.js"></script>')
t = t.replace("<head>", '<head>\n  <base href="/src/">', 1)
(root / "tests" / "ui" / "_mock.html").write_text(t, encoding="utf-8")
print("ok : tests/ui/_mock.html")
