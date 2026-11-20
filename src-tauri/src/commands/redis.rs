//! Commandes Tauri : explorateur et console Redis.
use crate::error::AppError;
use tauri::{State};
use crate::redis_client;
use crate::redis_client::{KeyDetail, RedisInfo, ScanPage};
use super::{RedisTarget, blocking};

#[tauri::command]
pub(crate) async fn redis_info(rt: State<'_, RedisTarget>) -> Result<RedisInfo, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::info(&p)).await
}

#[tauri::command]
pub(crate) async fn redis_scan(
    rt: State<'_, RedisTarget>,
    pattern: String,
    cursor: String,
    count: Option<usize>,
) -> Result<ScanPage, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::scan(&p, &pattern, &cursor, count.unwrap_or(100))).await
}

#[tauri::command]
pub(crate) async fn redis_get(rt: State<'_, RedisTarget>, key: String) -> Result<KeyDetail, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::get(&p, &key)).await
}

#[tauri::command]
pub(crate) async fn redis_del(rt: State<'_, RedisTarget>, keys: Vec<String>) -> Result<i64, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::del(&p, &keys)).await
}

#[tauri::command]
pub(crate) async fn redis_flush_prefix(rt: State<'_, RedisTarget>, prefix: String) -> Result<u64, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::flush_prefix(&p, &prefix)).await
}

/// F-049 : console Redis. Lecture seule hors mode avancé ; KEYS et commandes bloquantes toujours refusées.
#[tauri::command]
pub(crate) async fn redis_exec(rt: State<'_, RedisTarget>, line: String, advanced: bool) -> Result<String, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::exec(&p, &line, advanced)).await
}
