//! Moteur Apache. Reprend à l'identique la construction du service qui se trouvait dans `services::build` et délègue
//! le rendu des vhosts au module `vhosts` (format « auto » de Laragon).
use super::{Capabilities, EngineCtx, RenderedFile, Validation, WebEngine};
use crate::error::AppError;
use crate::paths::slash;
use crate::services::{exe, Launch, ServiceRole, ServiceSpec, Stop};
use crate::sites::{root_abs, SiteConfig};
use crate::vhosts;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub struct ApacheEngine;

/// Certificat à utiliser pour un site : celui de mkcert s'il existe, sinon celui de Laragon (comportement historique).
fn cert_files(ctx: &EngineCtx<'_>, domain: &str) -> Option<(PathBuf, PathBuf)> {
    let (crt, key) = crate::mkcert::cert_paths(&ctx.paths.ssl_dir(), domain);
    if crt.is_file() && key.is_file() {
        return Some((crt, key));
    }
    let (crt, key) = (ctx.paths.ssl_dir().join("laragon.crt"), ctx.paths.ssl_dir().join("laragon.key"));
    (crt.is_file() && key.is_file()).then_some((crt, key))
}

/// Contrôle `httpd -t` / `httpd -k` : `(exe, dir, conf)` en mode autonome si Apache est installé.
fn httpd_parts(ctx: &EngineCtx<'_>) -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !ctx.paths.standalone {
        return None;
    }
    let (dir, program) = exe(ctx.paths.latest_in("apache"), "bin/httpd.exe", "Apache").ok()?;
    let conf = ctx.paths.httpd_conf();
    conf.is_file().then_some((program, dir, conf))
}

fn run_httpd(program: &Path, dir: &Path, conf: &Path, extra: &[&str]) -> Result<(bool, String), AppError> {
    let mut cmd = Command::new(program);
    cmd.args(["-d", &slash(dir), "-f", &slash(conf)]).args(extra).stdin(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = cmd.output()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

impl WebEngine for ApacheEngine {
    fn id(&self) -> &'static str {
        "apache"
    }

    fn display_name(&self) -> &'static str {
        "Apache"
    }

    fn capabilities(&self) -> Capabilities {
        // .htaccess : AllowOverride All + mod_rewrite. mod_proxy ne sert pas encore (phase 3 : FastCGI).
        Capabilities { htaccess: true, http3: false, auto_https: false, reverse_proxy: false, hot_reload: false }
    }

    fn sites_dir(&self, ctx: &EngineCtx<'_>) -> PathBuf {
        ctx.paths.sites_enabled()
    }

    fn service_spec(&self, ctx: &EngineCtx<'_>) -> ServiceSpec {
        let (cfg, paths) = (ctx.cfg, ctx.paths);
        let launch = exe(paths.latest_in("apache"), "bin/httpd.exe", "Apache").map(|(dir, program)| {
            if paths.standalone {
                let conf = paths.httpd_conf();
                let text = crate::provision::httpd_conf_text(cfg, paths, &dir, paths.active_php().as_deref());
                Launch {
                    args: vec!["-d".into(), slash(&dir), "-f".into(), slash(&conf)],
                    cwd: dir,
                    program,
                    write_files: vec![(conf, text), (paths.data.join("run").join(".keep"), String::new())],
                    init: None,
                }
            } else {
                Launch { args: vec!["-d".into(), slash(&dir)], cwd: dir, program, write_files: vec![], init: None }
            }
        });
        // `httpd -t` avant chaque démarrage : la cause exacte d'une configuration invalide s'affiche dans l'interface.
        let (pf_cfg, pf_paths) = (cfg.clone(), paths.clone());
        let preflight: crate::services::Preflight = std::sync::Arc::new(move || {
            ApacheEngine
                .validate(&EngineCtx { cfg: &pf_cfg, paths: &pf_paths })
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let dir_name = paths.latest_in("apache").and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));
        ServiceSpec {
            id: self.id(),
            name: self.display_name(),
            port: Some(cfg.web.port),
            meta: format!("port {} · {}", cfg.web.port, dir_name.unwrap_or_default()),
            enabled: true,
            start_timeout_secs: 15,
            runtime: false,
            extra_ports: vec![],
            role: ServiceRole::Web,
            preflight: Some(preflight),
            launch,
            stop: Stop::Kill,
        }
    }

    fn render(&self, ctx: &EngineCtx<'_>, sites: &[SiteConfig]) -> Vec<RenderedFile> {
        let dir = self.sites_dir(ctx);
        sites
            .iter()
            .map(|s| {
                let certs = if s.ssl { cert_files(ctx, &s.domain) } else { None };
                let ssl = certs.as_ref().map(|(c, k)| (c.as_path(), k.as_path()));
                RenderedFile {
                    domain: s.domain.clone(),
                    path: vhosts::conf_path(&dir, &s.domain),
                    content: vhosts::render(&s.domain, &root_abs(ctx.paths, s), ssl, ctx.cfg.web.port, ctx.cfg.ssl_port()),
                }
            })
            .collect()
    }

    fn is_generated(&self, existing: &str, desired: &str) -> bool {
        // même contenu une fois tous les ports normalisés (port HTTP et port HTTPS de la configuration courante)
        vhosts::rewrite_ports(existing, 0, 0) == vhosts::rewrite_ports(desired, 0, 0)
    }

    fn validate(&self, ctx: &EngineCtx<'_>) -> Result<Validation, AppError> {
        let Some((program, dir, conf)) = httpd_parts(ctx) else { return Ok(Validation::Skipped) };
        let (ok, text) = run_httpd(&program, &dir, &conf, &["-t"])?;
        if ok {
            Ok(Validation::Valid)
        } else {
            let line = text.lines().find(|l| l.contains("Syntax error") || l.contains("AH")).or_else(|| text.lines().last()).unwrap_or("");
            Err(AppError::Config(format!("Apache (httpd -t) : {}", line.trim())))
        }
    }

    fn reload(&self, _ctx: &EngineCtx<'_>) -> Result<(), AppError> {
        // Mesuré (2026-11-20, Apache 2.4.71 lancé en mode console) : `httpd -k graceful` cherche le SERVICE Windows
        // « Apache2.4 » (AH00436) et échoue. Sans service, Apache ne se recharge pas à chaud : le superviseur le
        // redémarre (la configuration a déjà été validée par `httpd -t`).
        Err(AppError::NotStartable("Apache (mode console sous Windows) ne se recharge pas à chaud : redémarrage".into()))
    }

    fn remove_site(&self, ctx: &EngineCtx<'_>, domain: &str) -> Result<bool, AppError> {
        let path = vhosts::conf_path(&self.sites_dir(ctx), domain);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn restart_needed_after_new_site(&self) -> bool {
        true // si le rechargement à chaud échoue, Apache relit ses vhosts au redémarrage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::paths::AppPaths;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("astral-dev-eng-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t
    }

    fn cfg_in(root: &Path, standalone: bool) -> Config {
        let mut cfg = Config::default();
        if standalone {
            cfg.mode = "standalone".into();
        }
        let r = root.to_string_lossy().replace('\\', "/");
        cfg.paths.root = r.clone();
        cfg.paths.www = format!("{r}/www");
        cfg.paths.data = format!("{r}/data");
        cfg
    }

    fn fake_httpd(root: &Path) -> PathBuf {
        let httpd = root.join("bin/apache/httpd-2.4.62/bin/httpd.exe");
        std::fs::create_dir_all(httpd.parent().unwrap()).unwrap();
        std::fs::write(&httpd, "").unwrap();
        httpd
    }

    #[test]
    fn spec_is_the_web_service_and_reports_missing_binary() {
        let t = tmp("missing");
        let cfg = cfg_in(&t, false);
        let paths = AppPaths::from_config(&cfg);
        let s = ApacheEngine.service_spec(&EngineCtx { cfg: &cfg, paths: &paths });
        assert!(s.role == ServiceRole::Web && s.id == "apache" && s.name == "Apache" && s.port == Some(80) && !s.runtime);
        assert!(s.launch.is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Non-régression : même lancement qu'avant l'introduction du trait (mode adopt).
    #[test]
    fn adopt_mode_launch_matches_previous_behaviour() {
        let t = tmp("adopt");
        fake_httpd(&t);
        let cfg = cfg_in(&t, false);
        let paths = AppPaths::from_config(&cfg);
        let s = ApacheEngine.service_spec(&EngineCtx { cfg: &cfg, paths: &paths });
        let l = s.launch.unwrap();
        assert_eq!(l.args.len(), 2);
        assert_eq!(l.args[0], "-d");
        assert!(l.args[1].ends_with("httpd-2.4.62"));
        assert!(l.write_files.is_empty() && l.init.is_none());
        assert!(s.meta.ends_with("httpd-2.4.62"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Non-régression (« golden ») : en mode autonome, httpd.conf = exactement `provision::httpd_conf_text`.
    #[test]
    fn standalone_launch_writes_the_same_httpd_conf_as_before() {
        let t = tmp("standalone");
        let httpd = fake_httpd(&t);
        let cfg = cfg_in(&t, true);
        let paths = AppPaths::from_config(&cfg);
        let l = ApacheEngine.service_spec(&EngineCtx { cfg: &cfg, paths: &paths }).launch.unwrap();
        let dir = httpd.parent().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(l.args, vec!["-d".to_string(), slash(&dir), "-f".to_string(), slash(&paths.httpd_conf())]);
        assert_eq!(l.write_files.len(), 2);
        assert_eq!(l.write_files[0].0, paths.httpd_conf());
        assert_eq!(l.write_files[0].1, crate::provision::httpd_conf_text(&cfg, &paths, &dir, paths.active_php().as_deref()));
        assert_eq!(l.write_files[1].0, paths.data.join("run").join(".keep"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Non-régression (« golden ») : le rendu d'un site = `vhosts::render` avec les mêmes paramètres qu'avant.
    #[test]
    fn rendered_vhosts_are_byte_identical_to_the_previous_generator() {
        let t = tmp("render");
        let cfg = cfg_in(&t, true);
        let paths = AppPaths::from_config(&cfg);
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        std::fs::create_dir_all(paths.ssl_dir()).unwrap();
        let (crt, key) = crate::mkcert::cert_paths(&paths.ssl_dir(), "b.test");
        std::fs::write(&crt, "c").unwrap();
        std::fs::write(&key, "k").unwrap();
        let a = SiteConfig { name: "a".into(), domain: "a.test".into(), root: "www/a/public".into(), php: None, ssl: false, backend: None };
        let b = SiteConfig { name: "b".into(), domain: "b.test".into(), root: "www/b/public".into(), php: None, ssl: true, backend: None };
        let out = ApacheEngine.render(&ctx, &[a, b]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, vhosts::conf_path(&paths.sites_enabled(), "a.test"));
        assert_eq!(out[0].content, vhosts::render("a.test", &paths.root.join("www/a/public"), None, cfg.web.port, cfg.ssl_port()));
        assert_eq!(out[1].content, vhosts::render("b.test", &paths.root.join("www/b/public"), Some((&crt, &key)), cfg.web.port, cfg.ssl_port()));
        assert!(out[1].content.contains("SSLEngine on") && !out[0].content.contains("SSLEngine"));
        // idempotence : deux rendus = même texte
        let again = ApacheEngine.render(&ctx, &[SiteConfig { name: "a".into(), domain: "a.test".into(), root: "www/a/public".into(), php: None, ssl: false, backend: None }]);
        assert_eq!(again[0], out[0]);
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn ssl_block_needs_the_certificate_to_exist() {
        let t = tmp("nocert");
        let cfg = cfg_in(&t, true);
        let paths = AppPaths::from_config(&cfg);
        let s = SiteConfig { name: "x".into(), domain: "x.test".into(), root: "www/x/public".into(), php: None, ssl: true, backend: None };
        let out = ApacheEngine.render(&EngineCtx { cfg: &cfg, paths: &paths }, &[s]);
        assert!(!out[0].content.contains("SSLEngine"), "ssl voulu mais pas de certificat : pas de bloc HTTPS");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn generated_detection_ignores_ports_but_not_content() {
        let a = vhosts::render("a.test", Path::new("C:/x/public"), None, 80, 443);
        let moved = vhosts::render("a.test", Path::new("C:/x/public"), None, 8080, 8443);
        assert!(ApacheEngine.is_generated(&a, &moved));
        let custom = a.replace("AllowOverride All", "AllowOverride None\n    Options +Includes");
        assert!(!ApacheEngine.is_generated(&custom, &a));
    }

    #[test]
    fn capabilities_are_declared() {
        let c = ApacheEngine.capabilities();
        assert!(c.htaccess && !c.http3 && !c.auto_https && !c.reverse_proxy && !c.hot_reload);
        assert!(ApacheEngine.restart_needed_after_new_site());
    }

    fn http_get(port: u16, host: &str) -> String {
        use std::io::{Read, Write};
        let mut s = match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => return format!("ERR {e}"),
        };
        s.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
        let _ = s.write_all(format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes());
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    /// Réel : Apache 2.4 (téléchargé depuis le catalogue), sites ajoutés par `update_sites`, validation `httpd -t`,
    /// puis redémarrage supervisé (pas de rechargement à chaud en mode console). `cargo test real_apache -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_apache_validates_then_restarts_with_new_sites() {
        use crate::catalog::Catalog;
        use crate::engines::{update_sites, ApplyOpts};
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-p2".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.web.port = 18082;
        cfg.web.ssl_port = 18444;
        let paths = AppPaths::from_config(&cfg);
        for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
            std::fs::create_dir_all(d).unwrap();
        }
        if paths.latest_in("apache").is_none() {
            let cat = Catalog::embedded().unwrap();
            let c = cat.component("apache").unwrap();
            crate::components::install(&paths.root, &paths.data, c, &c.versions[0], &crate::components::Options { allow_local_http: false },
                &std::sync::atomic::AtomicBool::new(false), |_, _| {}, None).unwrap();
        }
        // état propre : une exécution précédente peut avoir laissé des sites
        let _ = std::fs::remove_dir_all(paths.sites_enabled());
        let _ = std::fs::remove_file(crate::sites::path(&paths));
        std::fs::create_dir_all(paths.sites_enabled()).unwrap();
        let mk = |name: &str, body: &str| {
            let d = paths.www.join(name).join("public");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("index.html"), body).unwrap();
        };
        mk("a", "SITE-A");
        mk("b", "SITE-B");
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        let site = |n: &str| SiteConfig { name: n.into(), domain: format!("{n}.test"), root: format!("www/{n}/public"), php: None, ssl: false, backend: None };
        update_sites(&ctx, &ApacheEngine, |s| s.upsert(site("a")), &ApplyOpts::default()).unwrap();
        let sup = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs()).with_own_root(Some(root.as_str()));
        sup.start("apache").unwrap();
        let pid_before = sup.statuses().into_iter().find(|s| s.id == "apache").unwrap().pid;
        assert!(http_get(cfg.web.port, "a.test").contains("SITE-A"));
        assert!(!http_get(cfg.web.port, "b.test").contains("SITE-B"), "b n'existe pas encore");

        // validation réelle : un site valide passe `httpd -t`
        let (_, rep) = update_sites(&ctx, &ApacheEngine, |s| s.upsert(site("b")), &ApplyOpts { validate: true, ..Default::default() }).unwrap();
        println!("rapport : {rep:?}");
        assert!(rep.validated, "httpd -t doit avoir tourné");

        // `httpd -k graceful` n'existe pas en mode console : reload() refuse, le superviseur redémarre (config validée)
        assert!(ApacheEngine.reload(&ctx).is_err());
        let t0 = std::time::Instant::now();
        sup.restart("apache").unwrap();
        println!("redémarrage supervisé (validé avant) : {} ms", t0.elapsed().as_millis());
        assert!(http_get(cfg.web.port, "b.test").contains("SITE-B"), "le nouveau site est servi après redémarrage");
        assert!(http_get(cfg.web.port, "a.test").contains("SITE-A"), "l'ancien site aussi");
        let _ = pid_before;

        // validation réelle d'une configuration invalide : refus avec le message d'Apache
        std::fs::write(paths.sites_enabled().join("auto.casse.test.conf"), "DirectiveInexistante oui\n").unwrap();
        let err = ApacheEngine.validate(&ctx).unwrap_err().to_string();
        println!("httpd -t invalide : {err}");
        assert!(err.contains("httpd -t"));
        // démarrage avec un fichier cassé : la cause exacte (pré-contrôle httpd -t) s'affiche, Apache n'est pas lancé
        sup.stop("apache").unwrap();
        let start_err = sup.start("apache").unwrap_err().to_string();
        println!("démarrage refusé : {start_err}");
        assert!(start_err.contains("AH00526") && start_err.contains("auto.casse.test.conf"), "{start_err}");
        std::fs::remove_file(paths.sites_enabled().join("auto.casse.test.conf")).unwrap();
        assert_eq!(ApacheEngine.validate(&ctx).unwrap(), Validation::Valid);
        sup.start("apache").unwrap();
        sup.stop("apache").unwrap();
    }
}
