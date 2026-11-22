//! Commandes Tauri : réglages, redémarrage, sortie.
use crate::error::AppError;
use std::sync::Arc;
use std::time::Duration;
use tauri::{State};
use crate::config;
use crate::probe;
use super::{Ctx, Sup, blocking};

/// Lance une nouvelle instance après la fin de ce processus puis quitte (voir `restart_app`).
pub(crate) fn relaunch_and_exit(app: &tauri::AppHandle, logs: std::path::PathBuf) -> Result<(), AppError> {
    let exe = std::env::current_exe()?;
    let exe_s = exe.display().to_string();
    let log = logs.join("restart.log");
    let log_s = log.display().to_string();
    if exe_s.contains('\'') || log_s.contains('\'') {
        return Err(AppError::Config("chemin avec apostrophe : relance automatique impossible".into()));
    }
    let _ = std::fs::create_dir_all(logs);
    let pid = std::process::id();
    // La nouvelle instance ne démarre qu'une fois CE processus terminé (instance unique) ; journal pour diagnostic.
    let script = format!(
        "Add-Content -LiteralPath '{log_s}' (\"[\" + (Get-Date -Format s) + \"] attente de la fin du processus {pid}\"); \
         Wait-Process -Id {pid} -Timeout 60 -ErrorAction SilentlyContinue; \
         Add-Content -LiteralPath '{log_s}' (\"[\" + (Get-Date -Format s) + \"] relance\"); \
         Start-Process -FilePath '{exe_s}'"
    );
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP (DETACHED_PROCESS empêche PowerShell de démarrer)
    }
    c.spawn()?;
    // Sortie forcée si l'arrêt propre traîne : sans cela l'instance unique bloquerait la relance.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(4));
        std::process::exit(0);
    });
    app.exit(0);
    Ok(())
}

/// Relance l'application : arrête d'abord les services (sinon ils restent orphelins), puis lance une nouvelle
/// instance détachée après une courte pause (l'instance unique empêche un démarrage simultané) et quitte.
#[tauri::command]
pub(crate) async fn restart_app(app: tauri::AppHandle, sup: Sup<'_>, _ctx: State<'_, Arc<Ctx>>) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || {
        s.stop_all();
        Ok(())
    })
    .await?;
    relaunch_and_exit(&app, _ctx.paths.logs())
}

/// Réglages courants (lus dans la configuration chargée au démarrage) + versions de PHP installées.
#[tauri::command]
pub(crate) fn settings_get(ctx: State<'_, Arc<Ctx>>) -> serde_json::Value {
    let mut php: Vec<String> = ctx
        .paths
        .php_dirs()
        .iter()
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter_map(|n| n.strip_prefix("php-").map(|r| r.split('.').take(2).collect::<Vec<_>>().join(".")))
        .collect();
    php.sort();
    php.dedup();
    let c = &ctx.cfg;
    serde_json::json!({
        "standalone": ctx.paths.standalone,
        "file": config::Config::find_file().map(|p| p.display().to_string()),
        "php_version": c.php.version, "php_installed": php,
        "web_port": c.web.port, "tld": c.web.tld,
        "mysql_port": c.mysql.port, "redis_port": c.redis.port,
        "php_workers": c.php.workers, "redis_version": c.redis.version,
        "web_mode": c.web.mode, "apache_port": c.web.apache_port, "caddy_admin_port": c.web.caddy_admin_port,
        "caddy_installed": ctx.paths.latest_in("caddy").map_or(false, |d| d.join("caddy.exe").is_file()),
        "mailpit_http": c.mailpit.http, "mailpit_smtp": c.mailpit.smtp,
        "close_to_tray": c.ui.close_to_tray, "start_minimized": c.ui.start_minimized, "autostart_services": c.ui.autostart_services,
        "www": c.paths.www,
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct SettingsInput {
    php_version: String,
    web_port: u16,
    mysql_port: u16,
    redis_port: u16,
    mailpit_http: u16,
    mailpit_smtp: u16,
    #[serde(default)]
    php_workers: Option<u32>,
    #[serde(default)]
    redis_version: Option<String>,
    /// `apache` | `caddy` | `caddy+apache` ; absent = inchangé.
    #[serde(default)]
    web_mode: Option<String>,
    #[serde(default)]
    apache_port: Option<u16>,
    #[serde(default)]
    caddy_admin_port: Option<u16>,
    close_to_tray: bool,
    start_minimized: bool,
    #[serde(default)]
    autostart_services: bool,
}

/// Enregistre les réglages dans astral-dev.toml (modification ciblée, sauvegarde `astral-dev.toml.bak` une fois).
/// Un redémarrage d'astral-dev est nécessaire.
#[tauri::command]
pub(crate) fn settings_save(input: SettingsInput) -> Result<(), AppError> {
    let file = config::Config::find_file().ok_or_else(|| AppError::Io("astral-dev.toml introuvable".into()))?;
    let ports = [input.web_port, input.mysql_port, input.redis_port, input.mailpit_http, input.mailpit_smtp];
    if ports.iter().any(|p| *p == 0) || (0..ports.len()).any(|i| (i + 1..ports.len()).any(|j| ports[i] == ports[j])) {
        return Err(AppError::Config("ports invalides ou en double".into()));
    }
    let text = std::fs::read_to_string(&file)?;
    let q = |s: &str| format!("\"{s}\"");
    let workers = input.php_workers.unwrap_or(2);
    if !(1..=8).contains(&workers) {
        return Err(AppError::Config("le nombre de workers PHP doit être compris entre 1 et 8".into()));
    }
    let redis_version = input.redis_version.clone().unwrap_or_default();
    if !["", "5", "8"].contains(&redis_version.as_str()) {
        return Err(AppError::Config("version de Redis inconnue (automatique, 5 ou 8)".into()));
    }
    let mut web_edits: Vec<(&str, &str, String)> = vec![];
    if let Some(m) = &input.web_mode {
        web_edits.push(("web", "mode", q(m)));
    }
    if let Some(p) = input.apache_port {
        web_edits.push(("web", "apache_port", p.to_string()));
    }
    if let Some(p) = input.caddy_admin_port {
        web_edits.push(("web", "caddy_admin_port", p.to_string()));
    }
    let mut edits: Vec<(&str, &str, String)> = vec![
        ("php", "workers", workers.to_string()),
        ("redis", "version", q(&redis_version)),
        ("php", "version", q(&input.php_version)),
        ("web", "port", input.web_port.to_string()),
        ("mysql", "port", input.mysql_port.to_string()),
        ("redis", "port", input.redis_port.to_string()),
        ("mailpit", "http", input.mailpit_http.to_string()),
        ("mailpit", "smtp", input.mailpit_smtp.to_string()),
        ("ui", "close_to_tray", input.close_to_tray.to_string()),
        ("ui", "start_minimized", input.start_minimized.to_string()),
        ("ui", "autostart_services", input.autostart_services.to_string()),
    ];
    edits.extend(web_edits);
    let new = config::Config::patch_toml(&text, &edits);
    config::Config::parse(&new)?; // refuse tout ce qui rendrait le fichier invalide
    let bak = file.with_extension("toml.bak");
    if !bak.exists() {
        std::fs::write(&bak, &text)?;
    }
    std::fs::write(&file, new)?;
    Ok(())
}

/// Avertissements du démarrage (ex. `sites.toml` invalide) : l'interface les affiche en haut du tableau de bord.
#[tauri::command]
pub(crate) fn startup_warnings(w: State<'_, super::StartupWarnings>) -> Vec<String> {
    w.0.clone()
}

/// Choisit un port libre pour un service dont un port est tenu par un programme étranger (ex. le Mailpit de Laragon)
/// et l'enregistre dans astral-dev.toml (sauvegarde unique). Un redémarrage d'astral-dev est nécessaire.
#[tauri::command]
pub(crate) async fn fix_port_conflict(ctx: State<'_, Arc<Ctx>>, id: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let file = config::Config::find_file().ok_or_else(|| AppError::Io("astral-dev.toml introuvable".into()))?;
        let cfg = &c.cfg;
        // (section, clé, port courant)
        let entries: Vec<(&str, &str, u16)> = match id.as_str() {
            "apache" if cfg.web_mode() == config::WebMode::CaddyApache => vec![("web", "apache_port", cfg.web.apache_port)],
            "apache" | "caddy" => vec![("web", "port", cfg.web.port)],
            "mysql" => vec![("mysql", "port", cfg.mysql.port)],
            "redis" => vec![("redis", "port", cfg.redis.port)],
            "mailpit" => vec![("mailpit", "http", cfg.mailpit.http), ("mailpit", "smtp", cfg.mailpit.smtp)],
            other => return Err(AppError::UnknownService(other.into())),
        };
        let root = cfg.paths.root.clone();
        let mut taken: Vec<u16> = vec![cfg.web.port, cfg.ssl_port(), cfg.mysql.port, cfg.redis.port, cfg.mailpit.http, cfg.mailpit.smtp];
        let mut edits: Vec<(&str, &str, String)> = vec![];
        let mut changes = vec![];
        for (section, key, port) in entries {
            if probe::foreign_owner(port, &root).is_some() {
                let new = probe::pick_free_port(port, probe::port_open, &taken)
                    .ok_or_else(|| AppError::PortInUse(port))?;
                taken.push(new);
                edits.push((section, key, new.to_string()));
                changes.push(serde_json::json!({ "key": format!("{section}.{key}"), "from": port, "to": new }));
            }
        }
        if edits.is_empty() {
            return Err(AppError::NotStartable("aucun port de ce service n'est occupé par un autre programme".into()));
        }
        let text = std::fs::read_to_string(&file)?;
        let new = config::Config::patch_toml(&text, &edits);
        config::Config::parse(&new)?;
        let bak = file.with_extension("toml.bak");
        if !bak.exists() {
            std::fs::write(&bak, &text)?;
        }
        std::fs::write(&file, new)?;
        Ok(serde_json::json!({ "changes": changes }))
    })
    .await
}

/// Ouvre astral-dev.toml dans le Bloc-notes (réglages jusqu'à la page Réglages).
#[tauri::command]
pub(crate) fn open_config() -> Result<(), AppError> {
    let file = config::Config::find_file().ok_or_else(|| AppError::Io("astral-dev.toml introuvable".into()))?;
    std::process::Command::new("notepad").arg(file).spawn()?;
    Ok(())
}

/// Quitte vraiment l'application (la croix de la fenêtre ne fait que la fermer / la masquer).
#[tauri::command]
pub(crate) fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}
