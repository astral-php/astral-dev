//! Moteurs web interchangeables (ADR-015). Un moteur fournit : (1) le `ServiceSpec` que le superviseur exécute,
//! (2) le rendu **pur** de la configuration des sites à partir de `SiteConfig`, (3) la validation et le rechargement,
//! (4) ses capacités. Tout le reste (hosts, certificats, ports, PHP, projets) reste indépendant du moteur.
use crate::config::Config;
use crate::error::AppError;
use crate::paths::AppPaths;
use crate::services::{Launch, ServiceRole, ServiceSpec, Stop};
use crate::sites::{SiteConfig, Sites};
use std::path::PathBuf;

pub mod apache;

/// Ce que le moteur sait faire ; l'interface pourra griser ou signaler une option incompatible.
#[allow(dead_code)] // lu par l'interface à partir de la phase 4 (Réglages)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Capabilities {
    pub htaccess: bool,
    pub http3: bool,
    pub auto_https: bool,
    pub reverse_proxy: bool,
    /// Rechargement de la configuration sans couper les sites. Apache sous Windows en mode console : non
    /// (`httpd -k` ne pilote que le service Windows) ; Caddy : oui (phase 4).
    pub hot_reload: bool,
}

pub struct EngineCtx<'a> {
    pub cfg: &'a Config,
    pub paths: &'a AppPaths,
}

/// Fichier de configuration d'un site tel que le moteur le produit.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFile {
    pub domain: String,
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validation {
    /// `httpd -t` (ou équivalent) a réussi.
    Valid,
    /// Pas de validation possible ici (binaire absent, mode adopt) : on continue.
    Skipped,
}

pub trait WebEngine: Send + Sync {
    /// Identifiant stable du service (celui du `ServiceSpec`).
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    #[allow(dead_code)]
    fn capabilities(&self) -> Capabilities;
    /// Dossier où le moteur lit la configuration des sites.
    fn sites_dir(&self, ctx: &EngineCtx<'_>) -> PathBuf;
    /// Service à superviser (rôle `Web` ou `FrontProxy`).
    fn service_spec(&self, ctx: &EngineCtx<'_>) -> ServiceSpec;
    /// Rendu pur et idempotent de la configuration de chaque site (aucun accès disque en écriture).
    fn render(&self, ctx: &EngineCtx<'_>, sites: &[SiteConfig]) -> Vec<RenderedFile>;
    /// `existing` est-il une version de `desired` produite par astral-dev (aux ports près) ? Sinon il est « personnalisé ».
    fn is_generated(&self, existing: &str, desired: &str) -> bool;
    /// Valide la configuration sur disque avant tout rechargement (`httpd -t`).
    fn validate(&self, ctx: &EngineCtx<'_>) -> Result<Validation, AppError>;
    /// Recharge à chaud la configuration d'un moteur en cours d'exécution ; `Err` = le moteur doit être redémarré.
    fn reload(&self, ctx: &EngineCtx<'_>) -> Result<(), AppError>;
    /// Supprime le fichier du site (uniquement s'il est « généré » ou si `force`) ; `true` si un fichier a été supprimé.
    fn remove_site(&self, ctx: &EngineCtx<'_>, domain: &str) -> Result<bool, AppError>;
    /// Un nouveau site exige-t-il un redémarrage si le rechargement à chaud échoue ?
    #[allow(dead_code)]
    fn restart_needed_after_new_site(&self) -> bool;
}

/// Moteur choisi par `[web] server` dans astral-dev.toml.
pub fn from_config(cfg: &Config) -> Result<Box<dyn WebEngine>, AppError> {
    match cfg.web.server.as_str() {
        "apache" => Ok(Box::new(apache::ApacheEngine)),
        "nginx" => Err(AppError::NotStartable("Nginx : pas encore pris en charge (abandonné au profit de Caddy)".into())),
        other => Err(AppError::Config(format!("moteur web non pris en charge : {other}"))),
    }
}

/// Service affiché (en erreur) quand le moteur configuré n'est pas disponible : la liste des services reste complète
/// et l'interface explique pourquoi il ne démarre pas (identifiant `apache`, comme avant l'introduction du trait).
pub fn unsupported_spec(cfg: &Config) -> ServiceSpec {
    let (name, message) = match cfg.web.server.as_str() {
        "nginx" => ("Nginx", "Nginx : pas encore pris en charge (prévu après la première version)".to_string()),
        other => ("Web", format!("moteur web non pris en charge : {other}")),
    };
    ServiceSpec {
        id: "apache",
        name,
        port: Some(cfg.web.port),
        meta: format!("port {}", cfg.web.port),
        enabled: true,
        start_timeout_secs: 15,
        runtime: false,
        extra_ports: vec![],
        role: ServiceRole::Web,
        pool: None,
        preflight: None,
        launch: Err::<Launch, String>(message),
        stop: Stop::Kill,
    }
}

#[derive(Debug, Default, Clone)]
pub struct ApplyOpts {
    /// Valider la configuration (`httpd -t`) après écriture ; en cas d'échec, tout est remis comme avant.
    pub validate: bool,
    /// Recharger à chaud après validation.
    pub reload: bool,
    /// Domaines dont le fichier doit être (ré)écrit quoi qu'il contienne (action explicite de l'utilisateur).
    pub force: Vec<String>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ApplyReport {
    pub written: Vec<String>,
    /// Fichiers personnalisés par l'utilisateur : non régénérés.
    pub kept_custom: Vec<String>,
    pub validated: bool,
    pub reloaded: bool,
    /// Rechargement impossible : le moteur doit être redémarré pour prendre en compte la configuration.
    pub restart_needed: bool,
    pub reload_error: Option<String>,
}

/// `apply_config()` (ADR-015) : rend la configuration de tous les sites, l'écrit, la **valide**, puis recharge.
/// En mode autonome les fichiers générés sont réalignés (ports) ; en mode adopt (vhosts de Laragon) on ne fait que créer
/// les fichiers manquants. En cas d'échec de validation, l'état précédent est restauré et l'erreur est renvoyée.
pub fn apply_config(ctx: &EngineCtx<'_>, engine: &dyn WebEngine, sites: &Sites, opts: &ApplyOpts) -> Result<ApplyReport, AppError> {
    let mut report = ApplyReport::default();
    // (chemin, contenu précédent ou None si le fichier n'existait pas)
    let mut undo: Vec<(PathBuf, Option<String>)> = vec![];
    let standalone = ctx.paths.standalone;
    for f in engine.render(ctx, &sites.sites) {
        let existing = std::fs::read_to_string(&f.path).ok();
        let forced = opts.force.iter().any(|d| d.eq_ignore_ascii_case(&f.domain));
        let write = match &existing {
            None => true,
            Some(cur) if *cur == f.content => false,
            Some(_) if !standalone => false, // jamais d'écrasement dans les vhosts de Laragon
            Some(_) if forced => true,
            Some(cur) => {
                if engine.is_generated(cur, &f.content) {
                    true
                } else {
                    report.kept_custom.push(f.domain.clone());
                    false
                }
            }
        };
        if !write {
            continue;
        }
        if let Some(dir) = f.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        undo.push((f.path.clone(), existing));
        std::fs::write(&f.path, &f.content)?;
        report.written.push(f.domain.clone());
    }
    let rollback = |undo: &[(PathBuf, Option<String>)]| {
        for (p, prev) in undo {
            match prev {
                Some(t) => {
                    let _ = std::fs::write(p, t);
                }
                None => {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    };
    if opts.validate && !undo.is_empty() {
        match engine.validate(ctx) {
            Ok(Validation::Valid) => report.validated = true,
            Ok(Validation::Skipped) => {}
            Err(e) => {
                rollback(&undo);
                return Err(e);
            }
        }
    }
    if opts.reload && !report.written.is_empty() {
        match engine.reload(ctx) {
            Ok(()) => report.reloaded = true,
            Err(e) => {
                report.restart_needed = true;
                report.reload_error = Some(e.to_string());
            }
        }
    }
    Ok(report)
}

/// Charge les sites (avec migration éventuelle), applique `mutate`, écrit les fichiers du moteur, les **valide**, et ne
/// sauvegarde `sites.toml` qu'ensuite : un échec laisse tout comme avant (fichiers restaurés, `sites.toml` intact).
pub fn update_sites(
    ctx: &EngineCtx<'_>,
    engine: &dyn WebEngine,
    mutate: impl FnOnce(&mut Sites),
    opts: &ApplyOpts,
) -> Result<(Sites, ApplyReport), AppError> {
    let mut sites = Sites::load_or_migrate(ctx.paths, &ctx.cfg.web.tld)?;
    mutate(&mut sites);
    sites.validate()?;
    let report = apply_config(ctx, engine, &sites, opts)?;
    sites.save(&crate::sites::path(ctx.paths))?;
    Ok((sites, report))
}

/// Démarrage de l'application (mode autonome) : aligne les fichiers générés sur la configuration courante (ports),
/// sans validation ni rechargement (le serveur n'est pas encore lancé).
pub fn sync_all(cfg: &Config, paths: &AppPaths) -> Option<String> {
    if !paths.standalone {
        return None;
    }
    let Ok(engine) = from_config(cfg) else { return None };
    let ctx = EngineCtx { cfg, paths };
    match Sites::load_or_migrate(paths, &cfg.web.tld) {
        Ok(sites) => apply_config(&ctx, engine.as_ref(), &sites, &ApplyOpts::default())
            .err()
            .map(|e| format!("Synchronisation des sites impossible : {e}")),
        Err(e) => Some(format!(
            "{e}. Corrigez data/sites.toml (une sauvegarde sites.toml.bak existe peut-être) ou supprimez-le : il sera recréé à partir des vhosts existants. Aucun site n'est modifié tant que le fichier est invalide."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_engine_from_config() {
        let mut cfg = Config::default();
        assert_eq!(from_config(&cfg).unwrap().id(), "apache");
        cfg.web.server = "nginx".into();
        assert!(matches!(from_config(&cfg), Err(AppError::NotStartable(_))));
        cfg.web.server = "lighttpd".into();
        assert!(matches!(from_config(&cfg), Err(AppError::Config(_))));
    }

    #[test]
    fn unsupported_engine_keeps_the_service_list_complete() {
        let mut cfg = Config::default();
        cfg.web.server = "nginx".into();
        let s = unsupported_spec(&cfg);
        assert!(s.role == ServiceRole::Web && s.name == "Nginx" && s.id == "apache");
        match s.launch {
            Err(m) => assert!(m.contains("pas encore pris en charge")),
            Ok(_) => panic!("le moteur non pris en charge ne doit pas avoir de lancement"),
        }
    }

    // ---------- apply_config / update_sites ----------

    use crate::engines::apache::ApacheEngine;
    use std::path::Path;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("astral-dev-apply-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t
    }

    fn cfg_in(root: &Path, standalone: bool, port: u16) -> Config {
        let mut cfg = Config::default();
        if standalone {
            cfg.mode = "standalone".into();
        }
        let r = root.to_string_lossy().replace('\\', "/");
        cfg.paths.root = r.clone();
        cfg.paths.www = format!("{r}/www");
        cfg.paths.data = format!("{r}/data");
        cfg.web.port = port;
        cfg
    }

    fn site(name: &str) -> SiteConfig {
        SiteConfig { name: name.into(), domain: format!("{name}.test"), root: format!("www/{name}/public"), php: None, ssl: false, backend: None }
    }

    fn sites_of(names: &[&str]) -> Sites {
        Sites { schema: 1, sites: names.iter().map(|n| site(n)).collect() }
    }

    fn vhost(paths: &AppPaths, name: &str) -> PathBuf {
        crate::vhosts::conf_path(&paths.sites_enabled(), &format!("{name}.test"))
    }

    #[test]
    fn apply_creates_missing_skips_identical_and_realigns_generated_files_on_a_port_change() {
        let t = tmp("realign");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        let sites = sites_of(&["a", "b"]);
        let r = apply_config(&EngineCtx { cfg: &cfg, paths: &paths }, &ApacheEngine, &sites, &ApplyOpts::default()).unwrap();
        assert_eq!(r.written, ["a.test", "b.test"]);
        // 2e passage identique : rien à écrire
        let r = apply_config(&EngineCtx { cfg: &cfg, paths: &paths }, &ApacheEngine, &sites, &ApplyOpts::default()).unwrap();
        assert!(r.written.is_empty() && r.kept_custom.is_empty());
        // changement de port : les fichiers générés sont réalignés
        let cfg2 = cfg_in(&t, true, 8080);
        let paths2 = AppPaths::from_config(&cfg2);
        let r = apply_config(&EngineCtx { cfg: &cfg2, paths: &paths2 }, &ApacheEngine, &sites, &ApplyOpts::default()).unwrap();
        assert_eq!(r.written.len(), 2);
        assert!(std::fs::read_to_string(vhost(&paths2, "a")).unwrap().contains("<VirtualHost *:8080>"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn customised_files_are_kept_unless_forced() {
        let t = tmp("custom");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        let sites = sites_of(&["a"]);
        apply_config(&ctx, &ApacheEngine, &sites, &ApplyOpts::default()).unwrap();
        let f = vhost(&paths, "a");
        let custom = std::fs::read_to_string(&f).unwrap().replace("AllowOverride All", "AllowOverride None");
        std::fs::write(&f, &custom).unwrap();
        let r = apply_config(&ctx, &ApacheEngine, &sites, &ApplyOpts::default()).unwrap();
        assert_eq!(r.kept_custom, ["a.test"]);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), custom, "la personnalisation est préservée");
        let r = apply_config(&ctx, &ApacheEngine, &sites, &ApplyOpts { force: vec!["a.test".into()], ..Default::default() }).unwrap();
        assert_eq!(r.written, ["a.test"]);
        assert!(std::fs::read_to_string(&f).unwrap().contains("AllowOverride All"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn adopt_mode_only_creates_missing_files_and_never_overwrites_laragon_vhosts() {
        let t = tmp("adoptapply");
        let cfg = cfg_in(&t, false, 80);
        let paths = AppPaths::from_config(&cfg);
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        std::fs::create_dir_all(paths.sites_enabled()).unwrap();
        std::fs::write(vhost(&paths, "a"), "VHOST DE LARAGON").unwrap();
        let r = apply_config(&ctx, &ApacheEngine, &sites_of(&["a", "b"]), &ApplyOpts { force: vec!["a.test".into()], ..Default::default() }).unwrap();
        assert_eq!(r.written, ["b.test"]);
        assert_eq!(std::fs::read_to_string(vhost(&paths, "a")).unwrap(), "VHOST DE LARAGON", "même forcé");
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Moteur dont la validation échoue : sert à prouver que l'état précédent est restauré.
    struct Refusing;
    impl WebEngine for Refusing {
        fn id(&self) -> &'static str { "apache" }
        fn display_name(&self) -> &'static str { "Refusing" }
        fn capabilities(&self) -> Capabilities { ApacheEngine.capabilities() }
        fn sites_dir(&self, ctx: &EngineCtx<'_>) -> PathBuf { ApacheEngine.sites_dir(ctx) }
        fn service_spec(&self, ctx: &EngineCtx<'_>) -> ServiceSpec { ApacheEngine.service_spec(ctx) }
        fn render(&self, ctx: &EngineCtx<'_>, sites: &[SiteConfig]) -> Vec<RenderedFile> { ApacheEngine.render(ctx, sites) }
        fn is_generated(&self, e: &str, d: &str) -> bool { ApacheEngine.is_generated(e, d) }
        fn validate(&self, _ctx: &EngineCtx<'_>) -> Result<Validation, AppError> { Err(AppError::Config("httpd -t : Syntax error on line 3".into())) }
        fn reload(&self, _ctx: &EngineCtx<'_>) -> Result<(), AppError> { Ok(()) }
        fn remove_site(&self, ctx: &EngineCtx<'_>, d: &str) -> Result<bool, AppError> { ApacheEngine.remove_site(ctx, d) }
        fn restart_needed_after_new_site(&self) -> bool { true }
    }

    #[test]
    fn failed_validation_restores_everything_and_keeps_sites_toml_unchanged() {
        let t = tmp("rollback");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        // état initial valide : un site « a »
        update_sites(&ctx, &ApacheEngine, |s| s.upsert(site("a")), &ApplyOpts::default()).unwrap();
        let before_toml = std::fs::read_to_string(crate::sites::path(&paths)).unwrap();
        let before_a = std::fs::read_to_string(vhost(&paths, "a")).unwrap();
        // ajout de « b » + modification de « a » avec un moteur qui refuse
        let err = update_sites(
            &ctx,
            &Refusing,
            |s| {
                s.upsert(site("b"));
                s.sites[0].root = "www/ailleurs/public".into();
            },
            &ApplyOpts { validate: true, force: vec!["a.test".into()], ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Syntax error"), "{err}");
        assert!(!vhost(&paths, "b").exists(), "le nouveau fichier est supprimé");
        assert_eq!(std::fs::read_to_string(vhost(&paths, "a")).unwrap(), before_a, "le fichier modifié est restauré");
        assert_eq!(std::fs::read_to_string(crate::sites::path(&paths)).unwrap(), before_toml, "sites.toml intact");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn sync_all_realigns_generated_vhosts_at_startup() {
        let t = tmp("sync");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        update_sites(&EngineCtx { cfg: &cfg, paths: &paths }, &ApacheEngine, |s| s.upsert(site("a")), &ApplyOpts::default()).unwrap();
        assert!(crate::sites::path(&paths).is_file());
        // démarrage avec un autre port : sync_all réaligne le vhost sans validation ni rechargement
        let cfg2 = cfg_in(&t, true, 8081);
        let paths2 = AppPaths::from_config(&cfg2);
        sync_all(&cfg2, &paths2);
        assert!(std::fs::read_to_string(vhost(&paths2, "a")).unwrap().contains("<VirtualHost *:8081>"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn https_can_be_enabled_then_disabled_per_site_without_touching_the_others() {
        let t = tmp("toggle");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        let ctx = EngineCtx { cfg: &cfg, paths: &paths };
        std::fs::create_dir_all(paths.ssl_dir()).unwrap();
        for d in ["a.test", "b.test"] {
            let (crt, key) = crate::mkcert::cert_paths(&paths.ssl_dir(), d);
            std::fs::write(crt, "c").unwrap();
            std::fs::write(key, "k").unwrap();
        }
        update_sites(&ctx, &ApacheEngine, |s| { s.upsert(SiteConfig { ssl: true, ..site("a") }); s.upsert(SiteConfig { ssl: true, ..site("b") }); }, &ApplyOpts::default()).unwrap();
        assert!(std::fs::read_to_string(vhost(&paths, "a")).unwrap().contains("SSLEngine on"));
        let b_before = std::fs::read_to_string(vhost(&paths, "b")).unwrap();
        // désactivation pour « a » seulement (action explicite = force)
        update_sites(&ctx, &ApacheEngine, |s| s.sites[0].ssl = false, &ApplyOpts { force: vec!["a.test".into()], ..Default::default() }).unwrap();
        assert!(!std::fs::read_to_string(vhost(&paths, "a")).unwrap().contains("SSLEngine"), "a : HTTP seulement");
        assert_eq!(std::fs::read_to_string(vhost(&paths, "b")).unwrap(), b_before, "b : inchangé");
        assert!(crate::sites::Sites::load(&crate::sites::path(&paths)).unwrap().sites.iter().any(|s| s.domain == "b.test" && s.ssl));
        // réactivation : le certificat conservé permet de revenir en HTTPS immédiatement
        update_sites(&ctx, &ApacheEngine, |s| s.sites[0].ssl = true, &ApplyOpts { force: vec!["a.test".into()], ..Default::default() }).unwrap();
        assert!(std::fs::read_to_string(vhost(&paths, "a")).unwrap().contains("SSLEngine on"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn invalid_sites_toml_is_reported_at_startup_and_nothing_is_modified() {
        let t = tmp("warn");
        let cfg = cfg_in(&t, true, 80);
        let paths = AppPaths::from_config(&cfg);
        update_sites(&EngineCtx { cfg: &cfg, paths: &paths }, &ApacheEngine, |s| s.upsert(site("a")), &ApplyOpts::default()).unwrap();
        let vhost_a = std::fs::read_to_string(vhost(&paths, "a")).unwrap();
        let f = crate::sites::path(&paths);
        let mut txt = std::fs::read_to_string(&f).unwrap();
        txt.push_str("
ceci n'est pas du toml
");
        std::fs::write(&f, &txt).unwrap();
        let w = sync_all(&cfg, &paths).expect("avertissement attendu");
        assert!(w.contains("sites.toml invalide") && w.contains("sites.toml.bak"), "{w}");
        assert_eq!(std::fs::read_to_string(vhost(&paths, "a")).unwrap(), vhost_a, "aucun vhost modifié");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), txt, "le fichier invalide n'est pas réécrit");
        std::fs::remove_dir_all(&t).unwrap();
    }
}
