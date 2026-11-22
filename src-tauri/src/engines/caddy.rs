//! Moteur Caddy (ADR-016). Un fichier par site dans `data/caddy/sites/<domaine>.caddy` ; le `Caddyfile` principal
//! (options globales + `import`) est dérivé de la configuration et réécrit avant chaque validation / lancement.
//! Mêmes certificats mkcert que pour Apache : aucun `local_certs`, `skip_install_trust` (Caddy ne touche jamais au
//! magasin de certificats de Windows). Rien n'est écrit dans `%APPDATA%` : `XDG_*` pointe vers `data/caddy`.
use super::{Capabilities, EngineCtx, RenderedFile, Validation, WebEngine};
use crate::config::{Config, WebMode};
use crate::error::AppError;
use crate::paths::{slash, AppPaths};
use crate::services::{exe, Launch, ServiceRole, ServiceSpec, Stop};
use crate::sites::{root_abs, SiteConfig};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub struct CaddyEngine;

pub fn caddy_dir(paths: &AppPaths) -> PathBuf {
    paths.data.join("caddy")
}

pub fn caddyfile(paths: &AppPaths) -> PathBuf {
    caddy_dir(paths).join("Caddyfile")
}

pub fn site_path(paths: &AppPaths, domain: &str) -> PathBuf {
    caddy_dir(paths).join("sites").join(format!("{domain}.caddy"))
}

/// Variables d'environnement de Caddy : données et configuration dans `data/caddy`.
pub fn env(paths: &AppPaths) -> Vec<(String, String)> {
    let d = caddy_dir(paths);
    vec![
        ("XDG_DATA_HOME".into(), d.join("xdg-data").to_string_lossy().to_string()),
        ("XDG_CONFIG_HOME".into(), d.join("xdg-config").to_string_lossy().to_string()),
    ]
}

/// `Caddyfile` principal. La directive `admin` est présente dans TOUTE configuration : un rechargement refusé ferait
/// sinon revenir l'API d'administration sur `localhost:2019` (mesuré en phase 0).
pub fn main_caddyfile(cfg: &Config, paths: &AppPaths) -> String {
    let d = caddy_dir(paths);
    format!(
        "# Généré par astral-dev — ne pas modifier : mettez vos réglages dans custom/*.caddy\n{{\n    admin 127.0.0.1:{admin}\n    default_bind 127.0.0.1\n    skip_install_trust\n    auto_https disable_redirects\n    servers {{\n        protocols h1 h2\n    }}\n}}\n\nimport {sites}/*.caddy\nimport {custom}/*.caddy\n",
        admin = cfg.web.caddy_admin_port,
        sites = slash(&d.join("sites")),
        custom = slash(&d.join("custom")),
    )
}

/// Serveur qui exécute un site : `apache` (reverse proxy vers Apache) ou `caddy` (FastCGI direct).
pub fn backend_of(cfg: &Config, site: &SiteConfig) -> &'static str {
    match cfg.web_mode() {
        WebMode::Apache => "apache",
        WebMode::Caddy => "caddy",
        WebMode::CaddyApache => match site.backend.as_deref() {
            Some("caddy") => "caddy",
            _ => "apache",
        },
    }
}

/// Rendu pur d'un site. `php_ports` = ports des workers de la série du site (vide : pas de PHP).
pub fn site_block(
    cfg: &Config,
    domain: &str,
    root: &Path,
    certs: Option<(&Path, &Path)>,
    backend: &str,
    php_ports: &[u16],
) -> String {
    let mut body = String::new();
    if backend == "apache" {
        body.push_str(&format!(
            "    reverse_proxy 127.0.0.1:{} {{\n        header_up X-Forwarded-Proto {{scheme}}\n    }}\n",
            cfg.web.apache_port
        ));
    } else {
        body.push_str(&format!("    root * {}\n", slash(root)));
        if !php_ports.is_empty() {
            let upstreams: Vec<String> = php_ports.iter().map(|p| format!("127.0.0.1:{p}")).collect();
            body.push_str(&format!("    php_fastcgi {} {{\n        lb_policy least_conn\n        try_files {{path}} {{path}}/index.php {{path}}/index.html index.php\n    }}\n", upstreams.join(" ")));
        }
        body.push_str("    file_server\n");
    }
    // Deux blocs : Caddy refuse `tls` dans un bloc qui écoute aussi en HTTP (mesuré en phase 4).
    let mut s = format!("http://{domain}:{} {{\n{body}}}\n", cfg.web.port);
    if let Some((crt, key)) = certs {
        s.push_str(&format!("\nhttps://{domain}:{} {{\n    tls {} {}\n{body}}}\n", cfg.ssl_port(), slash(crt), slash(key)));
    }
    s
}

fn hide(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    #[cfg(not(windows))]
    let _ = cmd;
}

/// `(caddy.exe, Caddyfile)` si Caddy est installé (mode autonome).
fn caddy_parts(ctx: &EngineCtx<'_>) -> Option<(PathBuf, PathBuf)> {
    if !ctx.paths.standalone || ctx.cfg.web_mode() == WebMode::Apache {
        return None;
    }
    let (_, program) = exe(ctx.paths.latest_in("caddy"), "caddy.exe", "Caddy").ok()?;
    Some((program, caddyfile(ctx.paths)))
}

fn run_caddy(program: &Path, paths: &AppPaths, args: &[&str]) -> Result<(bool, String), AppError> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(caddy_dir(paths)).stdin(Stdio::null());
    for (k, v) in env(paths) {
        cmd.env(k, v);
    }
    hide(&mut cmd);
    let out = cmd.output()?;
    Ok((out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

/// Dernière ligne utile d'une sortie d'erreur de Caddy.
fn caddy_error(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| l.contains("Error:") || l.contains("\"error\""))
        .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Écrit le `Caddyfile` principal et crée les dossiers attendus (idempotent).
fn ensure_main(ctx: &EngineCtx<'_>) -> Result<PathBuf, AppError> {
    let d = caddy_dir(ctx.paths);
    for sub in ["sites", "custom", "xdg-data", "xdg-config"] {
        std::fs::create_dir_all(d.join(sub))?;
    }
    let f = caddyfile(ctx.paths);
    let text = main_caddyfile(ctx.cfg, ctx.paths);
    if std::fs::read_to_string(&f).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&f, text)?;
    }
    Ok(f)
}

/// Les ports (`:8080`, `127.0.0.1:9083`) sont normalisés : un fichier ne diffère de celui produit que par ses ports ⇒ généré.
fn norm_ports(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ':' && chars.peek().map_or(false, |n| n.is_ascii_digit()) {
            while chars.peek().map_or(false, |n| n.is_ascii_digit()) {
                chars.next();
            }
            out.push('0');
        }
    }
    out
}

/// Texte sans les lignes que le moteur produit lui-même dans le corps d'un site (proxy vers Apache ou FastCGI direct) :
/// un fichier généré pour une autre topologie reste « généré », une ligne ajoutée par l'utilisateur le rend « personnalisé ».
fn strip_known_bodies(t: &str) -> String {
    t.split_inclusive('\n')
        .filter(|l| {
            let nested = l.starts_with("    ");
            let s = l.trim();
            !(nested
                && (s == "}"
                    || s == "file_server"
                    || s == "header_up X-Forwarded-Proto {scheme}"
                    || s == "lb_policy least_conn"
                    || s.starts_with("reverse_proxy 127.0.0.1:")
                    || s.starts_with("php_fastcgi 127.0.0.1:")
                    || s.starts_with("root * ")
                    || s.starts_with("try_files {path}")))
        })
        .collect()
}

impl WebEngine for CaddyEngine {
    fn id(&self) -> &'static str {
        "caddy"
    }

    fn display_name(&self) -> &'static str {
        "Caddy"
    }

    fn capabilities(&self) -> Capabilities {
        // .htaccess : non (Caddy seul) ; disponible via le backend Apache en `caddy+apache`.
        Capabilities { htaccess: false, http3: false, auto_https: false, reverse_proxy: true, hot_reload: true }
    }

    fn sites_dir(&self, ctx: &EngineCtx<'_>) -> PathBuf {
        caddy_dir(ctx.paths).join("sites")
    }

    fn service_spec(&self, ctx: &EngineCtx<'_>) -> ServiceSpec {
        let (cfg, paths) = (ctx.cfg, ctx.paths);
        let launch = exe(paths.latest_in("caddy"), "caddy.exe", "Caddy").map(|(_, program)| {
            let d = caddy_dir(paths);
            let f = caddyfile(paths);
            Launch {
                args: vec!["run".into(), "--config".into(), slash(&f), "--adapter".into(), "caddyfile".into()],
                cwd: d.clone(),
                program,
                write_files: vec![
                    (f, main_caddyfile(cfg, paths)),
                    (d.join("custom").join(".keep"), String::new()),
                    (d.join("sites").join(".keep"), String::new()),
                    (d.join("xdg-data").join(".keep"), String::new()),
                    (d.join("xdg-config").join(".keep"), String::new()),
                ],
                init: None,
                env: env(paths),
            }
        });
        // `caddy validate` avant chaque démarrage : la cause exacte (certificat absent, syntaxe) s'affiche.
        let (pf_cfg, pf_paths) = (cfg.clone(), paths.clone());
        let preflight: crate::services::Preflight = std::sync::Arc::new(move || {
            CaddyEngine.validate(&EngineCtx { cfg: &pf_cfg, paths: &pf_paths }).map(|_| ()).map_err(|e| e.to_string())
        });
        let dir_name = paths.latest_in("caddy").and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
        ServiceSpec {
            id: self.id(),
            name: self.display_name(),
            port: Some(cfg.web.port),
            meta: format!("port {} · {}", cfg.web.port, dir_name.unwrap_or_default()),
            enabled: true,
            start_timeout_secs: 15,
            runtime: false,
            extra_ports: vec![("admin", cfg.web.caddy_admin_port)],
            role: if cfg.web_mode() == WebMode::CaddyApache { ServiceRole::FrontProxy } else { ServiceRole::Web },
            pool: None,
            preflight: Some(preflight),
            launch,
            stop: Stop::Kill,
        }
    }

    fn render(&self, ctx: &EngineCtx<'_>, sites: &[SiteConfig]) -> Vec<RenderedFile> {
        let pools = crate::php_pool::pools(ctx.cfg, ctx.paths);
        let default = crate::php_pool::default_series(ctx.cfg, &pools).map(str::to_string);
        sites
            .iter()
            .map(|s| {
                let certs = if s.ssl { super::apache::cert_files(ctx, &s.domain) } else { None };
                let certs_ref = certs.as_ref().map(|(c, k)| (c.as_path(), k.as_path()));
                let series = s.php.clone().filter(|v| pools.iter().any(|p| &p.series == v)).or_else(|| default.clone());
                let ports: Vec<u16> = series.and_then(|v| pools.iter().find(|p| p.series == v).map(|p| p.ports.clone())).unwrap_or_default();
                RenderedFile {
                    domain: s.domain.clone(),
                    path: site_path(ctx.paths, &s.domain),
                    content: site_block(ctx.cfg, &s.domain, &root_abs(ctx.paths, s), certs_ref, backend_of(ctx.cfg, s), &ports),
                }
            })
            .collect()
    }

    fn is_generated(&self, existing: &str, desired: &str) -> bool {
        norm_ports(&strip_known_bodies(existing)) == norm_ports(&strip_known_bodies(desired))
    }

    fn validate(&self, ctx: &EngineCtx<'_>) -> Result<Validation, AppError> {
        let Some((program, _)) = caddy_parts(ctx) else { return Ok(Validation::Skipped) };
        let f = ensure_main(ctx)?;
        let (ok, text) = run_caddy(&program, ctx.paths, &["validate", "--config", &slash(&f), "--adapter", "caddyfile"])?;
        if ok {
            Ok(Validation::Valid)
        } else {
            Err(AppError::Config(format!("Caddy (validate) : {}", caddy_error(&text))))
        }
    }

    fn reload(&self, ctx: &EngineCtx<'_>) -> Result<(), AppError> {
        let Some((program, _)) = caddy_parts(ctx) else {
            return Err(AppError::NotStartable("Caddy n'est pas installé".into()));
        };
        let f = ensure_main(ctx)?;
        // Jamais de rechargement sans validation préalable (ADR-016, leçon de la phase 0).
        let (ok, text) = run_caddy(&program, ctx.paths, &["validate", "--config", &slash(&f), "--adapter", "caddyfile"])?;
        if !ok {
            return Err(AppError::Config(format!("Caddy (validate) : {}", caddy_error(&text))));
        }
        let addr = format!("127.0.0.1:{}", ctx.cfg.web.caddy_admin_port);
        let (ok, text) = run_caddy(&program, ctx.paths, &["reload", "--config", &slash(&f), "--adapter", "caddyfile", "--address", &addr])?;
        if ok {
            Ok(())
        } else {
            Err(AppError::NotStartable(format!("Caddy (reload) : {}", caddy_error(&text))))
        }
    }

    fn remove_site(&self, ctx: &EngineCtx<'_>, domain: &str) -> Result<bool, AppError> {
        let p = site_path(ctx.paths, domain);
        if p.is_file() {
            std::fs::remove_file(&p)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn restart_needed_after_new_site(&self) -> bool {
        true // « à appliquer » : `reload_web` recharge Caddy à chaud (sans coupure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: &str) -> Config {
        let mut c = Config::standalone("C:/rustral", false, |_| false);
        c.web.mode = mode.into();
        c
    }

    #[test]
    fn main_caddyfile_always_has_admin_and_never_touches_the_windows_trust_store() {
        let c = cfg("caddy");
        let p = AppPaths::from_config(&c);
        let t = main_caddyfile(&c, &p);
        assert!(t.contains("default_bind 127.0.0.1") && t.contains("protocols h1 h2"), "boucle locale seulement, sans HTTP/3 (UDP)");
        assert!(t.contains("admin 127.0.0.1:2019") && t.contains("skip_install_trust") && t.contains("auto_https disable_redirects"));
        assert!(!t.contains("local_certs"));
        assert!(t.contains("import C:/rustral/data/caddy/sites/*.caddy") && t.contains("/custom/*.caddy"));
    }

    #[test]
    fn php_site_uses_the_worker_pool_and_the_shared_mkcert_certificate() {
        let c = cfg("caddy");
        let s = site_block(&c, "demo.test", Path::new("C:/www/demo/public"), Some((Path::new("C:/ssl/demo.test.pem"), Path::new("C:/ssl/demo.test-key.pem"))), "caddy", &[9083, 9183]);
        assert!(s.starts_with("http://demo.test:80 {") && s.contains("\nhttps://demo.test:443 {\n    tls C:/ssl/demo.test.pem C:/ssl/demo.test-key.pem\n"), "{s}");
        assert_eq!(s.matches("php_fastcgi").count(), 2, "même corps en HTTP et en HTTPS");
        assert_eq!(s.matches("tls ").count(), 1, "tls seulement dans le bloc HTTPS");
        assert!(s.contains("root * C:/www/demo/public") && s.contains("php_fastcgi 127.0.0.1:9083 127.0.0.1:9183") && s.contains("lb_policy least_conn"));
        assert!(s.contains("file_server") && !s.contains("reverse_proxy"));
    }

    #[test]
    fn http_only_site_has_no_tls_and_a_site_without_php_is_static() {
        let c = cfg("caddy");
        let s = site_block(&c, "a.test", Path::new("C:/a"), None, "caddy", &[]);
        assert!(s.starts_with("http://a.test:80 {") && !s.contains("tls") && !s.contains("php_fastcgi") && s.contains("file_server"));
    }

    #[test]
    fn apache_backend_is_a_reverse_proxy_that_announces_the_scheme() {
        let mut c = cfg("caddy+apache");
        c.web.apache_port = 8081;
        let s = site_block(&c, "shop.test", Path::new("C:/shop"), None, "apache", &[9083]);
        assert!(s.contains("reverse_proxy 127.0.0.1:8081") && s.contains("header_up X-Forwarded-Proto {scheme}"));
        assert!(!s.contains("php_fastcgi") && !s.contains("root *"));
    }

    #[test]
    fn backend_follows_the_global_topology() {
        let site = |b: Option<&str>| SiteConfig { name: "a".into(), domain: "a.test".into(), root: "www/a/public".into(), php: None, ssl: false, backend: b.map(str::to_string) };
        assert_eq!(backend_of(&cfg("apache"), &site(Some("caddy"))), "apache");
        assert_eq!(backend_of(&cfg("caddy"), &site(Some("apache"))), "caddy");
        assert_eq!(backend_of(&cfg("caddy+apache"), &site(None)), "apache", "défaut : Apache (compatibilité .htaccess)");
        assert_eq!(backend_of(&cfg("caddy+apache"), &site(Some("caddy"))), "caddy");
    }

    #[test]
    fn generated_detection_ignores_ports_but_not_content() {
        let c = cfg("caddy");
        let a = site_block(&c, "a.test", Path::new("C:/a"), None, "caddy", &[9083, 9183]);
        let mut c2 = cfg("caddy");
        c2.web.port = 8080;
        let moved = site_block(&c2, "a.test", Path::new("C:/a"), None, "caddy", &[9083, 9183]);
        assert!(CaddyEngine.is_generated(&a, &moved));
        assert!(!CaddyEngine.is_generated(&a.replace("file_server", "file_server\n    header X-Perso 1"), &moved));
        // changement de topologie : le même site passe de FastCGI direct à reverse proxy → toujours « généré »
        let mut c3 = cfg("caddy+apache");
        c3.web.port = 8080;
        let proxied = site_block(&c3, "a.test", Path::new("C:/a"), None, "apache", &[]);
        assert!(CaddyEngine.is_generated(&a, &proxied) && CaddyEngine.is_generated(&proxied, &a));
        assert!(!CaddyEngine.is_generated(&proxied.replace("reverse_proxy", "header X-Perso 1\n    reverse_proxy"), &a));
    }

    #[test]
    fn spec_declares_front_or_web_role_and_the_admin_port() {
        let c = cfg("caddy+apache");
        let p = AppPaths::from_config(&c);
        let s = CaddyEngine.service_spec(&EngineCtx { cfg: &c, paths: &p });
        assert!(s.role == ServiceRole::FrontProxy && s.id == "caddy" && s.extra_ports == vec![("admin", 2019)]);
        let c = cfg("caddy");
        let p = AppPaths::from_config(&c);
        assert!(CaddyEngine.service_spec(&EngineCtx { cfg: &c, paths: &p }).role == ServiceRole::Web);
    }

    #[test]
    fn capabilities_are_declared() {
        let c = CaddyEngine.capabilities();
        assert!(!c.htaccess && c.reverse_proxy && c.hot_reload && CaddyEngine.restart_needed_after_new_site());
    }

    // ---------- réel ----------

    fn get(port: u16, host: &str, path: &str) -> String {
        use std::io::{Read, Write};
        let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return "ERR connexion".into() };
        s.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
        let _ = s.write_all(format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes());
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn https(port: u16, host: &str, path: &str) -> String {
        let out = Command::new("curl.exe")
            .args(["-sk", "--max-time", "20", "--resolve", &format!("{host}:{port}:127.0.0.1"), &format!("https://{host}:{port}{path}")])
            .output()
            .expect("curl.exe");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Certificat de test (OpenSSL d'Apache) sous le nom attendu par `mkcert::cert_paths`.
    fn fake_cert(paths: &AppPaths, domain: &str) {
        let (crt, key) = crate::mkcert::cert_paths(&paths.ssl_dir(), domain);
        if crt.is_file() && key.is_file() {
            return;
        }
        let apache = paths.latest_in("apache").expect("apache");
        let conf = apache.join("conf").join("openssl.cnf");
        // l'openssl.exe d'Apache dépend de DLL absentes du PATH : on prend celui de Git (ou ASTRAL_TEST_OPENSSL)
        let openssl = std::env::var("ASTRAL_TEST_OPENSSL").unwrap_or_else(|_| "C:/Program Files/Git/usr/bin/openssl.exe".into());
        let out = Command::new(openssl)
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", &format!("/CN={domain}"), "-keyout", &key.to_string_lossy(), "-out", &crt.to_string_lossy(), "-config", &conf.to_string_lossy()])
            .output()
            .expect("openssl");
        assert!(crt.is_file(), "openssl : {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Réel (phase 4) : Caddy seul (PHP FastCGI, HTTPS, charge, rechargement à chaud sans perte) puis Caddy ➔ Apache
    /// (`.htaccess`, schéma HTTPS transmis). Caddy est installé depuis le catalogue.
    /// `cargo test real_caddy -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_caddy_topologies() {
        use crate::catalog::Catalog;
        use crate::engines::{from_config, update_sites, ApplyOpts};
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-p3".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.web.port = 18095;
        cfg.web.ssl_port = 18449;
        cfg.web.apache_port = 18096;
        cfg.web.caddy_admin_port = 18019;
        cfg.mysql.port = 13327;
        cfg.redis.port = 16395;
        cfg.mailpit.http = 18046;
        cfg.mailpit.smtp = 11046;
        let paths = AppPaths::from_config(&cfg);
        assert!(paths.php_series().len() >= 2 && paths.latest_in("apache").is_some(), "lancez d'abord standalone_real_fastcgi_pool (import)");
        if paths.latest_in("caddy").is_none() {
            let cat = Catalog::embedded().unwrap();
            let c = cat.component("caddy").unwrap();
            crate::components::install(&paths.root, &paths.data, c, &c.versions[0], &crate::components::Options { allow_local_http: false },
                &std::sync::atomic::AtomicBool::new(false), |_, _| {}, None).unwrap();
        }
        println!("Caddy : {:?}", paths.latest_in("caddy"));
        let _ = std::fs::remove_dir_all(paths.sites_enabled());
        let _ = std::fs::remove_dir_all(caddy_dir(&paths).join("sites"));
        std::fs::create_dir_all(paths.sites_enabled()).unwrap();
        let _ = std::fs::remove_file(crate::sites::path(&paths));
        std::fs::create_dir_all(paths.ssl_dir()).unwrap();
        fake_cert(&paths, "demo.test");
        fake_cert(&paths, "shop.test");

        let mk = |name: &str, file: &str, code: &str| {
            let public = paths.www.join(name).join("public");
            std::fs::create_dir_all(&public).unwrap();
            std::fs::write(public.join(file), code).unwrap();
            public
        };
        let info = "<?php echo 'v=' . PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION . ';sapi=' . php_sapi_name() . ';https=' . ($_SERVER['HTTPS'] ?? 'off') . ';script=' . basename($_SERVER['SCRIPT_FILENAME']);";
        let demo = mk("demo", "index.php", info);
        std::fs::write(demo.join("sleep.php"), "<?php sleep(1); echo 'slept';").unwrap();
        mk("demo84", "index.php", info);
        mk("late", "index.html", "LATE-STATIC");
        let shop = mk("shop", "index.php", info);
        std::fs::write(shop.join(".htaccess"), "RewriteEngine On\nRewriteRule ^pretty$ index.php [L]\nRewriteRule ^autre$ page2.php [L]\n").unwrap();
        std::fs::write(shop.join("page2.php"), "<?php echo 'PAGE2';").unwrap();
        let site = |n: &str, php: Option<&str>, ssl: bool, backend: Option<&str>| SiteConfig {
            name: n.into(), domain: format!("{n}.test"), root: format!("www/{n}/public"), php: php.map(str::to_string), ssl, backend: backend.map(str::to_string),
        };
        let wanted = |c: &Config, p: &AppPaths| -> std::sync::Arc<dyn Fn() -> std::collections::BTreeSet<String> + Send + Sync> {
            let (c, p) = (c.clone(), p.clone());
            std::sync::Arc::new(move || crate::php_pool::wanted_series(&c, &p))
        };

        // ---------- A. Caddy seul ----------
        cfg.web.mode = "caddy".into();
        cfg.validate().unwrap();
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        let engine = from_config(&cfg).unwrap();
        let opts = ApplyOpts { validate: true, ..Default::default() };
        let (_, rep) = update_sites(&ctx, engine.as_ref(), |s| { s.upsert(site("demo", None, true, None)); s.upsert(site("demo84", Some("8.4"), false, None)); }, &opts).unwrap();
        println!("rapport A : {rep:?}");
        assert!(rep.validated, "caddy validate doit avoir tourné");
        println!("--- demo.test.caddy ---\n{}", std::fs::read_to_string(site_path(&paths, "demo.test")).unwrap());
        // un réglage utilisateur invalide (custom/*.caddy, certificat introuvable) est refusé par validate, avec la cause
        let bad = caddy_dir(&paths).join("custom").join("bad.caddy");
        std::fs::write(&bad, "https://x.test:18449 {\n    tls C:/nope/a.pem C:/nope/a-key.pem\n}\n").unwrap();
        let err = CaddyEngine.validate(&ctx).unwrap_err().to_string();
        println!("custom invalide : {err}");
        assert!(err.contains("Caddy (validate)"), "{err}");
        std::fs::remove_file(&bad).unwrap();
        assert_eq!(CaddyEngine.validate(&ctx).unwrap(), Validation::Valid);

        let sup = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs()).with_own_root(Some(root.as_str())).with_wanted_series(wanted(&cfg, &paths));
        for r in sup.start_workers() {
            assert!(r.ok, "{r:?}");
        }
        sup.start("caddy").unwrap();
        assert!(sup.statuses().iter().all(|s| s.id != "apache"), "pas de carte Apache en mode caddy");
        let a = get(cfg.web.port, "demo.test", "/index.php");
        let b = get(cfg.web.port, "demo84.test", "/index.php");
        println!("demo -> {}\ndemo84 -> {}", a.lines().last().unwrap_or(""), b.lines().last().unwrap_or(""));
        assert!(a.contains("v=8.3;sapi=cgi-fcgi") && b.contains("v=8.4;sapi=cgi-fcgi"), "{a}\n{b}");
        assert!(a.contains("script=index.php"), "SCRIPT_FILENAME : {a}");
        let s = https(cfg.ssl_port(), "demo.test", "/index.php");
        println!("https demo -> {s}");
        assert!(s.contains("v=8.3") && s.contains("https=on"), "{s}");
        // charge : 4 requêtes d'une seconde sur 2 workers (least_conn)
        let t = std::time::Instant::now();
        let hs: Vec<_> = (0..4).map(|_| { let p = cfg.web.port; std::thread::spawn(move || get(p, "demo.test", "/sleep.php")) }).collect();
        let outs: Vec<String> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        let el = t.elapsed().as_secs_f32();
        println!("4 × 1 s en parallèle : {el:.1} s");
        assert!(outs.iter().all(|o| o.contains("slept")) && el < 3.6, "{el:.1} s");
        // rechargement à chaud : 30 requêtes pendant l'ajout d'un site
        let port = cfg.web.port;
        let spam = std::thread::spawn(move || (0..30).map(|_| { let r = get(port, "demo.test", "/index.php"); std::thread::sleep(std::time::Duration::from_millis(40)); r.contains("200 OK") }).filter(|ok| *ok).count());
        let (_, rep) = update_sites(&ctx, engine.as_ref(), |s| s.upsert(site("late", None, false, None)), &opts).unwrap();
        assert!(rep.written == ["late.test"]);
        engine.reload(&ctx).expect("rechargement à chaud");
        let ok = spam.join().unwrap();
        println!("requêtes réussies pendant le rechargement : {ok}/30");
        assert_eq!(ok, 30);
        let late = get(cfg.web.port, "late.test", "/");
        println!("late -> {late}\n--- late.test.caddy ---\n{}", std::fs::read_to_string(site_path(&paths, "late.test")).unwrap_or_default());
        assert!(late.contains("LATE-STATIC"), "le nouveau site est servi sans redémarrage : {late}");
        // Caddy n'a rien écrit hors de data/caddy
        assert!(caddy_dir(&paths).join("xdg-data").is_dir());
        sup.stop_everything();
        std::thread::sleep(std::time::Duration::from_millis(600));
        for p in [cfg.web.port, cfg.web.caddy_admin_port] {
            assert!(!crate::probe::port_open(p), "port {p} libéré");
        }

        // ---------- B. Caddy ➔ Apache ----------
        cfg.web.mode = "caddy+apache".into();
        cfg.validate().unwrap();
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        let engine = from_config(&cfg).unwrap();
        let (_, rep) = update_sites(&ctx, engine.as_ref(), |s| {
            s.upsert(site("demo", None, true, Some("caddy")));
            s.upsert(site("shop", None, true, Some("apache")));
        }, &opts).unwrap();
        println!("rapport B : {rep:?}");
        assert!(rep.kept_custom.is_empty(), "les fichiers générés pour l'ancienne topologie sont régénérés : {:?}", rep.kept_custom);
        let shop_caddy = std::fs::read_to_string(site_path(&paths, "shop.test")).unwrap();
        assert!(shop_caddy.contains("reverse_proxy 127.0.0.1:18096"), "{shop_caddy}");
        let shop_apache = std::fs::read_to_string(crate::vhosts::conf_path(&paths.sites_enabled(), "shop.test")).unwrap();
        assert!(shop_apache.contains("<VirtualHost *:18096>") && !shop_apache.contains("SSLEngine"), "{shop_apache}");
        assert!(!crate::vhosts::conf_path(&paths.sites_enabled(), "demo.test").is_file(), "demo (backend caddy) n'a pas de vhost Apache");

        let sup = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs()).with_own_root(Some(root.as_str())).with_wanted_series(wanted(&cfg, &paths));
        for r in sup.start_workers() {
            assert!(r.ok, "{r:?}");
        }
        sup.start("apache").unwrap();
        sup.start("caddy").unwrap();
        let sh = https(cfg.ssl_port(), "shop.test", "/index.php");
        println!("https shop (Caddy ➔ Apache) -> {sh}");
        assert!(sh.contains("sapi=cgi-fcgi") && sh.contains("https=on"), "Apache doit connaître le schéma HTTPS : {sh}");
        let pretty = https(cfg.ssl_port(), "shop.test", "/pretty");
        println!(".htaccess -> {pretty}");
        assert!(pretty.contains("script=index.php"), ".htaccess (RewriteRule) via Apache : {pretty}");
        let d = https(cfg.ssl_port(), "demo.test", "/index.php");
        assert!(d.contains("v=8.3") && d.contains("https=on"), "{d}");
        assert!(get(cfg.web.apache_port, "shop.test", "/index.php").contains("https=off"), "en direct sur Apache : HTTP");
        // Apache lit le .htaccess : /autre → page2.php
        let autre = https(cfg.ssl_port(), "shop.test", "/autre");
        println!("Apache /autre -> {autre}");
        assert!(autre.contains("PAGE2"), "{autre}");
        // Le même site servi par Caddy : pas de .htaccess ; une adresse inexistante retombe sur index.php (comportement
        // « front controller » de php_fastcgi), jamais sur page2.php
        update_sites(&ctx, engine.as_ref(), |s| s.sites.iter_mut().find(|x| x.domain == "shop.test").unwrap().backend = Some("caddy".into()), &ApplyOpts { validate: true, force: vec!["shop.test".into()], ..Default::default() }).unwrap();
        CaddyEngine.reload(&ctx).expect("rechargement Caddy");
        let autre = https(cfg.ssl_port(), "shop.test", "/autre");
        let joli = https(cfg.ssl_port(), "shop.test", "/n-importe-quoi");
        println!("Caddy /autre -> {autre}\nCaddy /n-importe-quoi -> {joli}");
        assert!(!autre.contains("PAGE2") && autre.contains("https=on"), ".htaccess ignoré, repli sur index.php : {autre}");
        assert!(joli.contains("https=on"), "toute adresse inexistante retombe sur index.php : {joli}");
        sup.stop_everything();
        std::thread::sleep(std::time::Duration::from_millis(600));
        for p in [cfg.web.port, cfg.web.apache_port, cfg.web.caddy_admin_port] {
            assert!(!crate::probe::port_open(p), "port {p} libéré");
        }
    }
}
