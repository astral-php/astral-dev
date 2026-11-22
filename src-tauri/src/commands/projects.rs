//! Commandes Tauri : projets Astral (liste, création, console, exposition).
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::console;
use crate::hosts;
use crate::mkcert;
use crate::projects;
use crate::engines;
use crate::sites::{self, SiteConfig};
use crate::vhosts;
use super::{Ctx, blocking, spawn_job};

#[derive(serde::Serialize)]
pub(crate) struct ExposeResult {
    vhost: &'static str,
    hosts: &'static str,
    /// "mkcert" | "laragon" | "aucun"
    ssl: &'static str,
    /// Le moteur web doit prendre en compte la nouvelle configuration (rechargement à chaud ou redémarrage).
    restart_needed: bool,
}

#[derive(serde::Serialize)]
pub(crate) struct UnexposeResult {
    vhost: &'static str,
    hosts: &'static str,
    files: &'static str,
    restart_needed: bool,
}

/// Retire l'exposition d'un projet (vhost, certificat mkcert, ligne hosts du bloc astral-dev).
/// Les fichiers du projet ne sont supprimés que si `delete_files` est vrai (case cochée + nom retapé côté interface).
#[tauri::command]
pub(crate) async fn unexpose_project(ctx: State<'_, Arc<Ctx>>, name: String, delete_files: Option<bool>) -> Result<UnexposeResult, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("le retrait ne concerne que le mode standalone (les vhosts de Laragon ne sont pas touchés)".into()));
        }
        if !projects::valid_name(&name) {
            return Err(AppError::Config("nom de projet invalide".into()));
        }
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        let removed = engine.remove_site(&ectx, &domain)?;
        engines::update_sites(&ectx, engine.as_ref(), |s| { s.remove(&domain); }, &engines::ApplyOpts::default())?;
        let vhost = if removed { "supprimé" } else { "absent" };
        let (crt, key) = mkcert::cert_paths(&c.paths.ssl_dir(), &domain);
        for f in [crt, key] {
            let _ = std::fs::remove_file(f);
        }
        let hosts_path = hosts::hosts_path();
        let text = String::from_utf8(std::fs::read(&hosts_path)?).map_err(|_| AppError::Io("hosts non UTF-8 : modification refusée".into()))?;
        let hosts_state = match hosts::remove_domains(&text, &[domain.as_str()]) {
            None => "absent",
            Some(new_text) => {
                hosts::apply_elevated(text.as_bytes(), &new_text, &c.paths.data.join("tmp"), &[domain.as_str()], false)?;
                "retiré"
            }
        };
        let files = if delete_files.unwrap_or(false) {
            let dir = projects::project_dir(&c.paths.www, &name)?;
            if dir == c.paths.www || !dir.starts_with(&c.paths.www) {
                return Err(AppError::Config("suppression refusée : dossier hors de www".into()));
            }
            std::fs::remove_dir_all(&dir)?;
            "supprimés"
        } else {
            "conservés"
        };
        Ok(UnexposeResult { vhost, hosts: hosts_state, files, restart_needed: removed })
    })
    .await
}

#[tauri::command]
pub(crate) fn list_templates() -> Vec<(&'static str, &'static str)> {
    projects::TEMPLATES.to_vec()
}

#[tauri::command]
pub(crate) async fn list_projects(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<projects::ProjectInfo>, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let hosts_text = std::fs::read_to_string(hosts::hosts_path()).unwrap_or_default();
        let mut list = projects::list(&c.paths.www, &c.cfg.web.tld, &c.paths.sites_enabled(), &hosts_text)?;
        if c.paths.standalone {
            let pools = crate::php_pool::pools(&c.cfg, &c.paths);
            let default = crate::php_pool::default_series(&c.cfg, &pools).map(str::to_string);
            let sites = sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld).ok();
            let behind_caddy = c.cfg.web_mode() == crate::config::WebMode::CaddyApache;
            for p in &mut list {
                let site = sites.as_ref().and_then(|s| s.sites.iter().find(|x| x.name == p.name));
                let own = site.and_then(|s| s.php.clone());
                p.php_own = own.is_some();
                p.php = own.or_else(|| default.clone());
                if let Some(s) = site {
                    // « exposé » et « HTTPS » viennent de sites.toml : le fichier du moteur (vhost Apache ou fichier Caddy) en est dérivé
                    p.vhost = true;
                    let (crt, key) = mkcert::cert_paths(&c.paths.ssl_dir(), &s.domain);
                    p.https = p.https || (s.ssl && crt.is_file() && key.is_file());
                    if behind_caddy {
                        p.backend = Some(engines::caddy::backend_of(&c.cfg, s).to_string());
                    }
                }
            }
        }
        Ok(list)
    })
    .await
}

/// Séries de PHP installées et série par défaut (sélecteur de la page Projets).
#[tauri::command]
pub(crate) async fn list_php_versions(ctx: State<'_, Arc<Ctx>>) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let pools = crate::php_pool::pools(&c.cfg, &c.paths);
        let default = crate::php_pool::default_series(&c.cfg, &pools).map(str::to_string);
        let installed: Vec<String> = pools.iter().map(|p| p.series.clone()).collect();
        Ok(serde_json::json!({
            "default": default, "installed": installed, "workers": crate::php_pool::workers_of(&c.cfg), "standalone": c.paths.standalone,
            "web_mode": c.cfg.web.mode,
        }))
    })
    .await
}

/// Choisit le serveur qui exécute un site en topologie `caddy+apache` : `caddy` (FastCGI direct) ou `apache`
/// (`.htaccess`). Réécrit les fichiers des deux moteurs ; l'interface recharge ensuite le serveur web.
#[tauri::command]
pub(crate) async fn set_project_backend(ctx: State<'_, Arc<Ctx>>, name: String, backend: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone || c.cfg.web_mode() != crate::config::WebMode::CaddyApache {
            return Err(AppError::NotStartable("le choix du serveur par projet n'existe qu'en topologie « Caddy ➔ Apache » (Réglages)".into()));
        }
        if !projects::valid_name(&name) {
            return Err(AppError::Config("nom de projet invalide".into()));
        }
        if backend != "caddy" && backend != "apache" {
            return Err(AppError::Config("serveur inconnu (caddy ou apache)".into()));
        }
        let domain = format!("{name}.{}", c.cfg.web.tld);
        if sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld)?.get(&domain).is_none() {
            return Err(AppError::NotStartable(format!("exposez d'abord « {name} »")));
        }
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        let (_, report) = engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| {
                if let Some(site) = s.sites.iter_mut().find(|x| x.domain == domain) {
                    site.backend = Some(backend.clone());
                }
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![domain.clone()] },
        )?;
        // le vhost Apache d'un site passé sous Caddy n'a plus lieu d'être
        if backend == "caddy" {
            let f = crate::vhosts::conf_path(&c.paths.sites_enabled(), &domain);
            if std::fs::read_to_string(&f).map_or(false, |t| t.starts_with("define ROOT")) {
                let _ = std::fs::remove_file(f);
            }
        }
        Ok(serde_json::json!({ "restart_needed": !report.written.is_empty(), "backend": backend }))
    })
    .await
}

/// Choisit la série de PHP d'un projet exposé (`""` = série par défaut). Réécrit son vhost, valide `httpd -t`, puis
/// l'interface redémarre le moteur web (les workers de la série démarrent avec lui).
#[tauri::command]
pub(crate) async fn set_project_php(ctx: State<'_, Arc<Ctx>>, name: String, series: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("le choix de PHP par projet concerne le mode standalone".into()));
        }
        if !projects::valid_name(&name) {
            return Err(AppError::Config("nom de projet invalide".into()));
        }
        let pools = crate::php_pool::pools(&c.cfg, &c.paths);
        if !series.is_empty() && !pools.iter().any(|p| p.series == series) {
            return Err(AppError::Config(format!("PHP {series} n'est pas installé (page Composants)")));
        }
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        let sites_now = sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld)?;
        if sites_now.get(&domain).is_none() {
            return Err(AppError::NotStartable(format!("exposez d'abord « {name} » : la version de PHP se règle sur un site exposé")));
        }
        let (_, report) = engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| {
                if let Some(site) = s.sites.iter_mut().find(|x| x.domain == domain) {
                    site.php = if series.is_empty() { None } else { Some(series.clone()) };
                }
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![domain.clone()] },
        )?;
        Ok(serde_json::json!({ "restart_needed": !report.written.is_empty(), "series": series }))
    })
    .await
}

/// F-011/F-012 : crée un projet via `composer create-project` (refus si le dossier existe).
#[tauri::command]
pub(crate) fn create_project(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, name: String, template: String) -> Result<u64, AppError> {
    if !projects::valid_name(&name) {
        return Err(AppError::Config("nom invalide : minuscules, chiffres et tirets (2 à 41 caractères)".into()));
    }
    let package = projects::template_package(&template).ok_or_else(|| AppError::Config(format!("modèle inconnu : {template}")))?;
    let target = ctx.paths.www.join(&name);
    if target.exists() {
        return Err(AppError::Config(format!("PROJECT_EXISTS : {name} existe déjà")));
    }
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let phar = ctx.paths.composer_phar();
    if !phar.is_file() {
        return Err(AppError::BinaryNotFound(phar.display().to_string()));
    }
    let args = vec![
        phar.to_string_lossy().to_string(),
        "create-project".into(),
        package.into(),
        target.to_string_lossy().to_string(),
        "--no-interaction".into(),
        "--no-progress".into(),
        "--prefer-dist".into(),
    ];
    let cwd = ctx.paths.www.clone();
    Ok(spawn_job(app, ctx.inner(), php, args, cwd))
}

/// F-013 : `php bin/console <cmd>` dans un projet Astral (liste blanche).
#[tauri::command]
pub(crate) fn run_console(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, project: String, args: Vec<String>) -> Result<u64, AppError> {
    console::check_args(&args)?;
    let dir = projects::project_dir(&ctx.paths.www, &project)?;
    if !dir.join("bin").join("console").is_file() {
        return Err(AppError::NotStartable(format!("{project} n'est pas un projet Astral (bin/console absent)")));
    }
    let php = ctx.php_exe_for(&project).ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let mut full = vec!["bin/console".to_string()];
    full.extend(args);
    Ok(spawn_job(app, ctx.inner(), php, full, dir))
}

/// F-014/F-020 : vhost Apache + entrée hosts (UAC à la demande). N'écrase rien d'existant.
#[tauri::command]
pub(crate) async fn expose_project(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<ExposeResult, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &name)?;
        let domain = format!("{name}.{}", c.cfg.web.tld);
        if !hosts::valid_domain(&domain) {
            return Err(AppError::Config(format!("domaine invalide : {domain}")));
        }
        let certs = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert)
            .and_then(|m| mkcert::generate(&m, &c.paths.ssl_dir(), &domain).ok());
        let ssl = if certs.is_some() { "mkcert" } else if c.paths.ssl_dir().join("laragon.crt").is_file() { "laragon" } else { "aucun" };
        let public = dir.join("public");
        if !public.is_dir() {
            return Err(AppError::Io(format!("dossier public/ introuvable : {}", public.display())));
        }
        // La configuration du moteur est générée à partir de sites.toml (ADR-015) : jamais écrite à la main ici.
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        let file_exists = vhosts::conf_path(&engine.sites_dir(&ectx), &domain).exists();
        let site = SiteConfig { name: name.clone(), domain: domain.clone(), root: sites::root_for(&c.paths, &public), php: None, ssl: ssl != "aucun", backend: None };
        let (_, report) = engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| {
                // En mode adopt, un vhost déjà présent appartient à Laragon : on ne le revendique pas.
                if s.get(&domain).is_none() && !(file_exists && !c.paths.standalone) {
                    s.upsert(site);
                }
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![] },
        )?;
        let created = report.written.iter().any(|d| d == &domain);
        let vhost = if created { "créé" } else { "déjà présent" };
        let hosts_path = hosts::hosts_path();
        let bytes = std::fs::read(&hosts_path)?;
        let text = String::from_utf8(bytes).map_err(|_| AppError::Io("hosts non UTF-8 : modification refusée".into()))?;
        let hosts_state = match hosts::add_domains(&text, &[domain.as_str()]) {
            None => "déjà présent",
            Some(new_text) => {
                hosts::apply_elevated(text.as_bytes(), &new_text, &c.paths.data.join("tmp"), &[domain.as_str()], true)?;
                "ajouté"
            }
        };
        Ok(ExposeResult { vhost, hosts: hosts_state, ssl, restart_needed: created && engine.restart_needed_after_new_site() })
    })
    .await
}
