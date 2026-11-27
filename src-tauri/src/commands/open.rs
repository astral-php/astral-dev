//! Commandes Tauri : ouvertures externes (ADR-020). Aucune ne reçoit d'adresse ni de chemin libre : l'adresse est
//! construite ici à partir de la configuration, le dossier est `www` ou un projet validé.
use crate::error::AppError;
use crate::{projects, sites};
use std::process::{Command, Stdio};
use std::sync::Arc;
use tauri::{Manager, State};
use super::{blocking, Ctx, Sup};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `http(s)://domaine[:port]/` : le schéma est `https` seulement si demandé, le port n'est écrit que s'il n'est pas le défaut.
pub fn site_url(domain: &str, https: bool, port: u16, ssl_port: u16) -> String {
    let (scheme, p, default) = if https { ("https", ssl_port, 443) } else { ("http", port, 80) };
    if p == default { format!("{scheme}://{domain}/") } else { format!("{scheme}://{domain}:{p}/") }
}

fn spawn_hidden(program: &str, args: &[&str]) -> Result<(), AppError> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.spawn().map(|_| ()).map_err(|e| AppError::Io(format!("{program} : {e}")))
}

/// Ouvre une adresse construite par l'application dans le navigateur par défaut (sans shell).
fn open_url(url: &str) -> Result<(), AppError> {
    debug_assert!(url.starts_with("http://") || url.starts_with("https://"));
    spawn_hidden("rundll32.exe", &["url.dll,FileProtocolHandler", url])
}

fn open_dir(path: &std::path::Path) -> Result<(), AppError> {
    if !path.is_dir() {
        return Err(AppError::Io(format!("dossier introuvable : {}", path.display())));
    }
    let p = path.to_string_lossy().replace('/', "\\");
    // explorer.exe n'est pas lancé avec CREATE_NO_WINDOW : il ouvre sa propre fenêtre
    Command::new("explorer.exe").arg(p).spawn().map(|_| ()).map_err(|e| AppError::Io(format!("explorer.exe : {e}")))
}

/// Ouvre l'interface de Mailpit (navigateur par défaut, ou fenêtre intégrée si `[ui] mailpit_window`).
#[tauri::command]
pub(crate) async fn open_mailpit(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, sup: Sup<'_>) -> Result<(), AppError> {
    let c = ctx.inner().clone();
    let s = sup.inner().clone();
    blocking(move || {
        let running = s.statuses().into_iter().any(|x| x.id == "mailpit" && x.state == "running");
        if !running {
            return Err(AppError::NotStartable("Mailpit n'est pas démarré : démarrez-le d'abord (tableau de bord)".into()));
        }
        let url = format!("http://127.0.0.1:{}/", c.cfg.mailpit.http);
        if !c.cfg.ui.mailpit_window {
            return open_url(&url);
        }
        if let Some(w) = app.get_webview_window("mailpit") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
            return Ok(());
        }
        let parsed: tauri::Url = url.parse().map_err(|_| AppError::Config("adresse Mailpit invalide".into()))?;
        // Aucune capability pour cette fenêtre : le contenu externe n'a pas accès aux commandes de l'application.
        tauri::WebviewWindowBuilder::new(&app, "mailpit", tauri::WebviewUrl::External(parsed))
            .title("Mailpit — Rustral Studio")
            .inner_size(1100.0, 780.0)
            .build()
            .map(|_| ())
            .map_err(|e| AppError::Io(format!("fenêtre Mailpit : {e}")))
    })
    .await
}

/// Ouvre le site d'un projet exposé dans le navigateur par défaut.
#[tauri::command]
pub(crate) async fn open_site(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !projects::valid_name(&name) {
            return Err(AppError::Config("nom de projet invalide".into()));
        }
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let https = if c.paths.standalone {
            let s = sites::Sites::load_or_migrate(&c.paths, &c.cfg.web.tld)?;
            let site = s.get(&domain).ok_or_else(|| AppError::NotStartable(format!("« {name} » n'est pas activé : activez-le d'abord")))?;
            let (crt, key) = crate::mkcert::cert_paths(&c.paths.ssl_dir(), &domain);
            site.ssl && crt.is_file() && key.is_file()
        } else {
            let f = crate::vhosts::conf_path(&c.paths.sites_enabled(), &domain);
            let t = std::fs::read_to_string(&f).map_err(|_| AppError::NotStartable(format!("« {name} » n'est pas activé : activez-le d'abord")))?;
            t.contains("SSLEngine on")
        };
        let url = site_url(&domain, https, c.cfg.web.port, c.cfg.ssl_port());
        open_url(&url)?;
        Ok(url)
    })
    .await
}

/// Ouvre un dossier dans l'Explorateur : `"www"` ou le nom d'un projet.
#[tauri::command]
pub(crate) async fn open_folder(ctx: State<'_, Arc<Ctx>>, target: String) -> Result<(), AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if target == "www" {
            return open_dir(&c.paths.www);
        }
        let dir = projects::project_dir(&c.paths.www, &target)?;
        open_dir(&dir)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_omit_the_default_port_and_choose_the_scheme() {
        assert_eq!(site_url("a.test", false, 80, 443), "http://a.test/");
        assert_eq!(site_url("a.test", true, 80, 443), "https://a.test/");
        assert_eq!(site_url("a.test", false, 8080, 8443), "http://a.test:8080/");
        assert_eq!(site_url("a.test", true, 8080, 8443), "https://a.test:8443/");
    }

    #[test]
    fn opening_a_missing_folder_is_refused() {
        assert!(open_dir(std::path::Path::new("C:/astral-dev-n-existe-pas")).is_err());
    }
}
