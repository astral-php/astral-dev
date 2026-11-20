//! Commandes Tauri : cache Redis par projet.
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use crate::cache_setup;
use crate::probe;
use crate::projects;
use crate::services;
use super::{Ctx, blocking, spawn_job};

#[tauri::command]
pub(crate) async fn cache_status(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<cache_setup::CacheStatus, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        Ok(cache_setup::status(&dir, c.paths.php_exe().as_deref(), c.cfg.redis.port))
    })
    .await
}

/// F-047 : active Redis pour un projet (écrit le .env après contrôles). `cache:clear` est lancé ensuite par l'UI.
#[tauri::command]
pub(crate) async fn cache_enable(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<(), AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        let s = cache_setup::status(&dir, c.paths.php_exe().as_deref(), c.cfg.redis.port);
        let pw = services::redis_secret(&c.cfg, &c.paths).unwrap_or_default();
        cache_setup::enable(&dir, &s, &pw)
    })
    .await
}

#[tauri::command]
pub(crate) async fn cache_disable(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || cache_setup::disable(&projects::project_dir(&c.paths.www, &project)?)).await
}

/// `composer require astral-php/astral-cache` (confirmation côté UI). L'enregistrement du provider reste manuel.
#[tauri::command]
pub(crate) fn add_cache_package(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, project: String) -> Result<u64, AppError> {
    let dir = projects::project_dir(&ctx.paths.www, &project)?;
    if !dir.join("composer.json").is_file() {
        return Err(AppError::NotStartable("composer.json introuvable".into()));
    }
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let args = vec![
        ctx.paths.composer_phar().to_string_lossy().to_string(),
        "require".into(),
        "astral-php/astral-cache".into(),
        "--no-interaction".into(),
        "--no-progress".into(),
    ];
    Ok(spawn_job(app, ctx.inner(), php, args, dir))
}

/// Teste le cache Redis du projet avec les paramètres de son `.env` : PHP (ext-redis) écrit puis relit une clé.
#[tauri::command]
pub(crate) async fn cache_test(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        let php = c.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
        let env = std::fs::read_to_string(dir.join(".env")).map_err(|_| AppError::NotStartable(".env introuvable".into()))?;
        if projects::env_value(&env, "CACHE_DRIVER").as_deref() != Some("redis") {
            return Err(AppError::NotStartable("activez d'abord le cache Redis pour ce projet".into()));
        }
        let get = |k: &str, d: &str| projects::env_value(&env, k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        let port: u16 = get("REDIS_PORT", "6379").parse().unwrap_or(6379);
        if !probe::port_open(port) {
            return Err(AppError::NotStartable(format!("Redis ne répond pas sur 127.0.0.1:{port} — démarrez-le (Tout démarrer) puis relancez le test")));
        }
        let code = "try{$r=new Redis();$r->connect(getenv('RH'),(int)getenv('RP'),3);if(getenv('RPW')!=='')$r->auth(getenv('RPW'));$r->select((int)getenv('RDB'));$k='astral:cache:astral-dev-test';$r->setex($k,60,'ok');echo ($r->get($k)==='ok')?'OK':'KO';}catch(Throwable $e){echo 'ERR:'.$e->getMessage();}";
        let mut cmd = std::process::Command::new(&php);
        cmd.arg("-r").arg(code)
            .env("RH", get("REDIS_HOST", "127.0.0.1"))
            .env("RP", get("REDIS_PORT", "6379"))
            .env("RPW", projects::env_value(&env, "REDIS_PASSWORD").unwrap_or_default())
            .env("RDB", get("REDIS_DB", "0"))
            .stdin(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(msg) = text.strip_prefix("ERR:") {
            return Err(AppError::StartFailed(format!("le cache Redis ne répond pas correctement : {} (vérifiez REDIS_HOST / REDIS_PORT / REDIS_PASSWORD dans le .env)", msg.trim())));
        }
        if text == "OK" {
            Ok("Cache Redis opérationnel : PHP a écrit puis relu la clé astral:cache:astral-dev-test (expire dans 60 s). Visible dans la page Redis.".into())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").to_string();
            Err(AppError::StartFailed(format!("test du cache en échec ({}){}", if text.is_empty() { "pas de réponse" } else { &text }, if err.is_empty() { String::new() } else { format!(" : {err}") })))
        }
    })
    .await
}
