//! Commandes Tauri, regroupées par domaine. L'état partagé (`Ctx`) et les utilitaires communs sont ici.
pub(crate) mod cache;
pub(crate) mod components;
pub(crate) mod https;
pub(crate) mod logs;
pub(crate) mod projects;
pub(crate) mod redis;
pub(crate) mod services;
pub(crate) mod settings;

use crate::error::AppError;
use std::sync::Arc;
use tauri::{Emitter, State};
use crate::console;
use crate::config::Config;
use crate::paths::AppPaths;
use crate::supervisor::{Supervisor};
use crate::redis_client::{Target};

pub(crate) struct RedisTarget(pub(crate) Target);

/// Avertissements détectés au démarrage (ex. `sites.toml` illisible), affichés en bandeau par l'interface.
pub(crate) struct StartupWarnings(pub(crate) Vec<String>);

/// Contexte applicatif partagé (config + chemins + compteur de tâches).
pub(crate) struct Ctx {
    pub(crate) cfg: Config,
    pub(crate) paths: AppPaths,
    pub(crate) jobs: std::sync::atomic::AtomicU64,
    /// v0.2 : une seule installation de composant à la fois ; annulation demandée par l'UI.
    pub(crate) installing: std::sync::atomic::AtomicBool,
    pub(crate) install_cancel: std::sync::atomic::AtomicBool,
}

/// Lance un processus en tâche de fond ; émet `console-line` / `console-done`. Retourne l'id de tâche.
pub(crate) fn spawn_job(
    app: tauri::AppHandle,
    ctx: &Arc<Ctx>,
    program: std::path::PathBuf,
    args: Vec<String>,
    cwd: std::path::PathBuf,
) -> u64 {
    let id = ctx.jobs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let php_dir = ctx.paths.active_php();
    std::thread::spawn(move || {
        let res = console::run(&program, &args, &cwd, php_dir.as_deref(), |err, line| {
            let _ = app.emit("console-line", serde_json::json!({ "job": id, "stderr": err, "line": line }));
        });
        let payload = match res {
            Ok(code) => serde_json::json!({ "job": id, "code": code }),
            Err(e) => serde_json::json!({ "job": id, "code": -1, "error": e.to_string() }),
        };
        let _ = app.emit("console-done", payload);
    });
    id
}

pub(crate) type Sup<'a> = State<'a, Arc<Supervisor>>;

pub(crate) async fn blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Io(e.to_string()))?
}
