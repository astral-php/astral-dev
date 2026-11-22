//! Commandes Tauri : services (démarrer, arrêter, journaux).
use crate::error::AppError;
use crate::supervisor::{OpResult, ServiceStatus};
use std::sync::Arc;
use tauri::State;
use super::{Ctx, Sup, blocking};

#[tauri::command]
pub(crate) async fn list_services(sup: Sup<'_>) -> Result<Vec<ServiceStatus>, AppError> {
    let s = sup.inner().clone();
    blocking(move || {
        s.heal_workers();
        Ok(s.statuses())
    })
    .await
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
        // un site peut avoir changé de série de PHP : ses workers doivent tourner avant le moteur web
        s.start_workers();
        let ectx = crate::engines::EngineCtx { cfg: &c.cfg, paths: &c.paths };
        // Moteurs dans l'ordre de démarrage (Apache avant Caddy) ; le résultat retenu est le plus fort : restart > start > reload.
        let mut result = "reload";
        for engine in crate::engines::list(&c.cfg)? {
            let id = engine.id();
            let st = s.statuses().into_iter().find(|x| x.id == id).ok_or_else(|| AppError::UnknownService(id.into()))?;
            let mode = if st.state == "running" && st.managed {
                match engine.reload(&ectx) {
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
            result = match (result, mode) {
                ("restart", _) | (_, "restart") => "restart",
                ("start", _) | (_, "start") => "start",
                _ => "reload",
            };
        }
        Ok(serde_json::json!({ "mode": result }))
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
