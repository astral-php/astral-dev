//! Mise à jour automatique (ADR-019, SPEC F-150). Aucune API du plugin n'est exposée à l'interface : deux commandes Rust
//! seulement (`check_update`, `install_update`) ; la signature de l'artefact est vérifiée par le plugin avec la clé
//! publique de `tauri.conf.json`, et la version annoncée doit être celle de la signature (anti-retour en arrière).
use crate::error::AppError;
use crate::commands::{blocking, Sup};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Mise à jour trouvée par la dernière vérification (consommée par `install_update`).
#[derive(Default)]
pub struct Pending(pub Mutex<Option<Update>>);

#[derive(Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub available: bool,
    pub version: Option<String>,
    pub notes: Option<String>,
}

fn friendly(e: impl std::fmt::Display) -> AppError {
    let m = e.to_string();
    let low = m.to_lowercase();
    let text = if low.contains("signature") {
        format!("signature de la mise à jour refusée : {m}")
    } else if low.contains("valid release") || low.contains("404") || low.contains("not found") {
        "aucune mise à jour publiée pour le moment (fichier latest.json introuvable)".to_string()
    } else if low.contains("error sending request") || low.contains("dns") || low.contains("connect") || low.contains("timed out") || low.contains("network") {
        format!("serveur de mises à jour injoignable (vérifiez la connexion Internet) — détail : {m}")
    } else {
        m
    };
    AppError::Io(text)
}

/// Adresse de test : `RUSTRAL_UPDATE_ENDPOINT` remplace l'adresse de `tauri.conf.json` (la signature reste vérifiée).
fn build_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, AppError> {
    let mut b = app.updater_builder();
    if let Ok(url) = std::env::var("RUSTRAL_UPDATE_ENDPOINT") {
        let u = url.parse().map_err(|e| AppError::Config(format!("RUSTRAL_UPDATE_ENDPOINT invalide : {e}")))?;
        b = b.endpoints(vec![u]).map_err(friendly)?;
    }
    b.timeout(std::time::Duration::from_secs(20)).build().map_err(friendly)
}

#[tauri::command]
pub(crate) async fn check_update(app: AppHandle, pending: State<'_, Pending>) -> Result<UpdateInfo, AppError> {
    let current = app.package_info().version.to_string();
    let updater = build_updater(&app)?;
    let found = updater.check().await.map_err(friendly)?;
    let info = UpdateInfo { current, available: found.is_some(), version: found.as_ref().map(|u| u.version.clone()), notes: found.as_ref().and_then(|u| u.body.clone()) };
    *pending.0.lock().unwrap() = found;
    Ok(info)
}

/// Télécharge (signature vérifiée), arrête les services proprement, puis lance l'installeur ; l'application se ferme
/// et l'installeur la relance. Aucun processus de service ne reste orphelin.
#[tauri::command]
pub(crate) async fn install_update(app: AppHandle, pending: State<'_, Pending>, sup: Sup<'_>) -> Result<(), AppError> {
    let update = pending.0.lock().unwrap().take().ok_or_else(|| AppError::NotStartable("aucune mise à jour vérifiée : cliquez d'abord sur « Vérifier les mises à jour »".into()))?;
    let bytes = update.download(|_, _| {}, || {}).await.map_err(friendly)?;
    let s = sup.inner().clone();
    blocking(move || {
        s.stop_all();
        Ok(())
    })
    .await?;
    let _ = &app;
    update.install(bytes).map_err(friendly)
}

/// Diagnostic : `rustral-studio.exe --selftest-update` vérifie puis installe seul une mise à jour (journal `logs/update.log`).
pub fn selftest(app: AppHandle, logs: std::path::PathBuf) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let log = |m: String| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("update.log")) {
                let _ = writeln!(f, "{m}");
            }
        };
        let _ = std::fs::create_dir_all(&logs);
        tokio_sleep(6).await;
        let pending = app.state::<Pending>();
        match check_update(app.clone(), pending.clone()).await {
            Ok(i) => log(format!("check : installée {} · disponible {:?}", i.current, i.version)),
            Err(e) => return log(format!("check : ERREUR {e}")),
        }
        match install_update(app.clone(), pending, app.state::<std::sync::Arc<crate::supervisor::Supervisor>>()).await {
            Ok(()) => log("install : lancée".into()),
            Err(e) => log(format!("install : ERREUR {e}")),
        }
    });
}

async fn tokio_sleep(secs: u64) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(std::time::Duration::from_secs(secs))).await;
}
