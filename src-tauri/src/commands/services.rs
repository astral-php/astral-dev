//! Commandes Tauri : services (démarrer, arrêter, journaux).
use crate::error::AppError;
use crate::supervisor::{OpResult, ServiceStatus};
use std::sync::Arc;
use tauri::State;
use super::{Ctx, Sup, blocking};

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

/// Prend en compte la configuration des sites : rechargement **à chaud** du moteur web s'il tourne (sans couper les
/// sites en cours), redémarrage si le rechargement est impossible, démarrage s'il est arrêté.
/// Retour : `{ "mode": "reload" | "restart" | "start" }`.
#[tauri::command]
pub(crate) async fn reload_web(ctx: State<'_, Arc<Ctx>>, sup: Sup<'_>) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    let s = sup.inner().clone();
    blocking(move || {
        let engine = crate::engines::from_config(&c.cfg)?;
        let id = engine.id();
        let st = s.statuses().into_iter().find(|x| x.id == id).ok_or_else(|| AppError::UnknownService(id.into()))?;
        let mode = if st.state == "running" && st.managed {
            match engine.reload(&crate::engines::EngineCtx { cfg: &c.cfg, paths: &c.paths }) {
                Ok(()) => "reload",
                Err(_) => {
                    s.restart(id)?;
                    "restart"
                }
            }
        } else if st.state == "running" && st.orphan {
            s.restart(id)?;
            "restart"
        } else if st.state == "running" {
            return Err(AppError::NotManaged(st.name.into()));
        } else {
            s.start(id)?;
            "start"
        };
        Ok(serde_json::json!({ "mode": mode }))
    })
    .await
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
