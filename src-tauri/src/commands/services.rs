//! Commandes Tauri : services (démarrer, arrêter, journaux).
use crate::error::AppError;
use crate::supervisor::{OpResult, ServiceStatus};
use super::{Sup, blocking};

#[tauri::command]
pub(crate) async fn list_services(sup: Sup<'_>) -> Result<Vec<ServiceStatus>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.statuses())).await
}

#[tauri::command]
pub(crate) async fn start_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.start(&id)).await
}

#[tauri::command]
pub(crate) async fn stop_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.stop(&id)).await
}

#[tauri::command]
pub(crate) async fn restart_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.restart(&id)).await
}

#[tauri::command]
pub(crate) async fn start_all(sup: Sup<'_>) -> Result<Vec<OpResult>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.start_all())).await
}

#[tauri::command]
pub(crate) async fn stop_all(sup: Sup<'_>) -> Result<Vec<OpResult>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.stop_everything())).await
}

#[tauri::command]
pub(crate) async fn service_logs(sup: Sup<'_>, id: String, lines: Option<usize>) -> Result<Vec<String>, AppError> {
    let s = sup.inner().clone();
    blocking(move || s.logs(&id, lines.unwrap_or(500).min(5000))).await
}
