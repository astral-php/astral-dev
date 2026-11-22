//! Commandes Tauri : HTTPS local (mkcert).
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::mkcert;
use crate::projects;
use crate::engines;
use crate::sites;
use crate::vhosts;
use super::{Ctx, blocking};

/// HTTPS local (mkcert) : installé ? autorité de confiance ? port HTTPS.
#[tauri::command]
pub(crate) async fn mkcert_status(ctx: State<'_, Arc<Ctx>>) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let path = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert);
        let trusted = path.is_some() && mkcert::ca_trusted();
        Ok(serde_json::json!({
            "installed": path.is_some(),
            "path": path.map(|p| p.display().to_string()),
            "ca_trusted": trusted,
            "ssl_port": c.cfg.ssl_port(),
        }))
    })
    .await
}

/// `mkcert -install` sur demande explicite (Windows demande confirmation à l'utilisateur).
#[tauri::command]
pub(crate) async fn mkcert_install_ca(ctx: State<'_, Arc<Ctx>>) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        mkcert::install_ca(&m)
    })
    .await
}

/// `mkcert -uninstall` sur demande explicite : retire l'approbation de l'autorité locale.
#[tauri::command]
pub(crate) async fn mkcert_uninstall_ca(ctx: State<'_, Arc<Ctx>>) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        mkcert::uninstall_ca(&m)
    })
    .await
}

/// Désactive HTTPS pour un projet (le bloc HTTPS disparaît de son vhost ; le certificat est conservé pour une
/// réactivation rapide). Réécriture sur action explicite (force), validée par `httpd -t` avant d'être conservée.
#[tauri::command]
pub(crate) async fn disable_https(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("mode standalone seulement (les vhosts de Laragon ne sont pas modifiés)".into()));
        }
        projects::project_dir(&c.paths.www, &name)?;
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        // « exposé » = présent dans sites.toml (le fichier du moteur, vhost Apache ou fichier Caddy, en est dérivé)
        if sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld)?.get(&domain).is_none() {
            let custom = std::fs::read_to_string(vhosts::conf_path(&c.paths.sites_enabled(), &domain)).is_ok();
            return Err(AppError::NotStartable(if custom { "vhost personnalisé : modification refusée" } else { "projet non exposé" }.into()));
        }
        engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| {
                if let Some(x) = s.sites.iter_mut().find(|x| x.domain.eq_ignore_ascii_case(&domain)) {
                    x.ssl = false;
                }
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![domain.clone()] },
        )?;
        Ok(serde_json::json!({ "restart_needed": true }))
    })
    .await
}

/// Active HTTPS pour un projet déjà exposé : certificat mkcert + bloc HTTPS dans son vhost.
#[tauri::command]
pub(crate) async fn enable_https(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("HTTPS guidé : mode standalone seulement (les vhosts de Laragon ne sont pas modifiés)".into()));
        }
        let dir = projects::project_dir(&c.paths.www, &name)?;
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        let _ = &dir;
        if sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld)?.get(&domain).is_none() {
            let custom = std::fs::read_to_string(vhosts::conf_path(&c.paths.sites_enabled(), &domain)).is_ok();
            return Err(AppError::NotStartable(if custom { "vhost personnalisé : modification refusée" } else { "exposez d'abord le projet" }.into()));
        }
        mkcert::generate(&m, &c.paths.ssl_dir(), &domain)?;
        // Le bloc HTTPS est produit par le moteur à partir de sites.toml ; fichier réécrit sur action explicite (force).
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| {
                if let Some(x) = s.sites.iter_mut().find(|x| x.domain.eq_ignore_ascii_case(&domain)) {
                    x.ssl = true;
                }
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![domain.clone()] },
        )?;
        let ssl = c.cfg.ssl_port();
        // Apache : le httpd.conf généré au démarrage ne contient `Listen <port HTTPS>` que si un certificat existait alors.
        // Caddy ouvre son port HTTPS au rechargement : pas de redémarrage de l'application.
        let listens = c.cfg.web_mode() != crate::config::WebMode::Apache
            || std::fs::read_to_string(c.paths.httpd_conf()).map_or(false, |t| t.contains(&format!("Listen 127.0.0.1:{ssl}")));
        let url = if ssl == 443 { format!("https://{domain}/") } else { format!("https://{domain}:{ssl}/") };
        Ok(serde_json::json!({ "url": url, "app_restart_needed": !listens, "ca_trusted": mkcert::ca_trusted() }))
    })
    .await
}
