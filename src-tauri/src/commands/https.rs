//! Commandes Tauri : HTTPS local (mkcert).
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::mkcert;
use crate::projects;
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
        let conf = vhosts::conf_path(&c.paths.sites_enabled(), &domain);
        let current = std::fs::read_to_string(&conf).map_err(|_| AppError::NotStartable("exposez d'abord le projet (vhost absent)".into()))?;
        if !current.starts_with("define ROOT") {
            return Err(AppError::NotStartable("vhost personnalisé : modification refusée".into()));
        }
        let (crt, key) = mkcert::generate(&m, &c.paths.ssl_dir(), &domain)?;
        let public = dir.join("public");
        std::fs::write(&conf, vhosts::render(&domain, &public, Some((&crt, &key)), c.cfg.web.port, c.cfg.ssl_port()))?;
        let ssl = c.cfg.ssl_port();
        // Le httpd.conf généré au démarrage ne contient `Listen <port HTTPS>` que si un certificat existait alors.
        let listens = std::fs::read_to_string(c.paths.httpd_conf()).map_or(false, |t| t.contains(&format!("Listen 127.0.0.1:{ssl}")));
        let url = if ssl == 443 { format!("https://{domain}/") } else { format!("https://{domain}:{ssl}/") };
        Ok(serde_json::json!({ "url": url, "app_restart_needed": !listens, "ca_trusted": mkcert::ca_trusted() }))
    })
    .await
}
