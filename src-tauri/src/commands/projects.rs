//! Commandes Tauri : projets Astral (liste, création, console, exposition).
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::console;
use crate::hosts;
use crate::mkcert;
use crate::projects;
use crate::vhosts;
use super::{Ctx, blocking, spawn_job};

#[derive(serde::Serialize)]
pub(crate) struct ExposeResult {
    vhost: &'static str,
    hosts: &'static str,
    /// "mkcert" | "laragon" | "aucun"
    ssl: &'static str,
    apache_restart_needed: bool,
}

#[derive(serde::Serialize)]
pub(crate) struct UnexposeResult {
    vhost: &'static str,
    hosts: &'static str,
    files: &'static str,
    apache_restart_needed: bool,
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
        let conf = vhosts::conf_path(&c.paths.sites_enabled(), &domain);
        let vhost = if conf.is_file() {
            std::fs::remove_file(&conf)?;
            "supprimé"
        } else {
            "absent"
        };
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
        Ok(UnexposeResult { vhost, hosts: hosts_state, files, apache_restart_needed: vhost == "supprimé" })
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
        projects::list(&c.paths.www, &c.cfg.web.tld, &c.paths.sites_enabled(), &hosts_text)
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
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
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
        let vhost = match vhosts::ensure(&c.paths.sites_enabled(), &domain, &dir.join("public"), &c.paths.ssl_dir(), certs, c.cfg.web.port, c.cfg.ssl_port())? {
            vhosts::VhostOutcome::Created => "créé",
            vhosts::VhostOutcome::AlreadyExists => "déjà présent",
        };
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
        Ok(ExposeResult { vhost, hosts: hosts_state, ssl, apache_restart_needed: vhost == "créé" })
    })
    .await
}
