//! Commandes Tauri : HTTPS local (mkcert).
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::mkcert;
use crate::projects;
use crate::engines;
use crate::sites::{self, SiteConfig};
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
        let conf = vhosts::conf_path(&engine.sites_dir(&ectx), &domain);
        let current = std::fs::read_to_string(&conf).map_err(|_| AppError::NotStartable("projet non exposé (vhost absent)".into()))?;
        let from_file = sites::parse_vhost(&current).ok_or_else(|| AppError::NotStartable("vhost personnalisé : modification refusée".into()))?;
        engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| match s.sites.iter_mut().find(|x| x.domain.eq_ignore_ascii_case(&domain)) {
                Some(x) => x.ssl = false,
                None => s.upsert(SiteConfig {
                    name: name.clone(),
                    domain: domain.clone(),
                    root: sites::root_for(&c.paths, std::path::Path::new(&from_file.1)),
                    php: None,
                    ssl: false,
                    backend: None,
                }),
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
        let conf = vhosts::conf_path(&c.paths.sites_enabled(), &domain);
        let current = std::fs::read_to_string(&conf).map_err(|_| AppError::NotStartable("exposez d'abord le projet (vhost absent)".into()))?;
        if !current.starts_with("define ROOT") {
            return Err(AppError::NotStartable("vhost personnalisé : modification refusée".into()));
        }
        mkcert::generate(&m, &c.paths.ssl_dir(), &domain)?;
        let public = dir.join("public");
        // Le bloc HTTPS est produit par le moteur à partir de sites.toml ; fichier réécrit sur action explicite (force).
        let engine = engines::from_config(&c.cfg)?;
        let ectx = engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        let from_file = sites::parse_vhost(&current);
        engines::update_sites(
            &ectx,
            engine.as_ref(),
            |s| match s.sites.iter_mut().find(|x| x.domain.eq_ignore_ascii_case(&domain)) {
                Some(x) => x.ssl = true,
                None => s.upsert(SiteConfig {
                    name: name.clone(),
                    domain: domain.clone(),
                    root: from_file.map_or_else(|| sites::root_for(&c.paths, &public), |(_, r, _)| sites::root_for(&c.paths, std::path::Path::new(&r))),
                    php: None,
                    ssl: true,
                    backend: None,
                }),
            },
            &engines::ApplyOpts { validate: true, reload: false, force: vec![domain.clone()] },
        )?;
        let ssl = c.cfg.ssl_port();
        // Le httpd.conf généré au démarrage ne contient `Listen <port HTTPS>` que si un certificat existait alors.
        let listens = std::fs::read_to_string(c.paths.httpd_conf()).map_or(false, |t| t.contains(&format!("Listen 127.0.0.1:{ssl}")));
        let url = if ssl == 443 { format!("https://{domain}/") } else { format!("https://{domain}:{ssl}/") };
        Ok(serde_json::json!({ "url": url, "app_restart_needed": !listens, "ca_trusted": mkcert::ca_trusted() }))
    })
    .await
}
