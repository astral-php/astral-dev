//! Commandes Tauri : composants du catalogue et premier lancement.
use crate::error::AppError;
use std::sync::Arc;
use tauri::{Emitter, State};
use crate::catalog;
use crate::components;
use crate::install;
use crate::provision;
use super::{Ctx, Sup, blocking};

/// ADR-011 : l'application a-t-elle tous ses binaires ? (sinon l'interface propose l'import depuis Laragon)
#[tauri::command]
pub(crate) fn setup_status(ctx: State<'_, Arc<Ctx>>) -> provision::SetupStatus {
    provision::status(&ctx.cfg, &ctx.paths)
}

/// Copie les binaires depuis Laragon (tâche de fond). Événements : `import-line`, `import-done`.
#[tauri::command]
pub(crate) fn import_laragon(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>) -> Result<(), AppError> {
    if !ctx.paths.standalone {
        return Err(AppError::NotStartable("l'import ne concerne que le mode standalone".into()));
    }
    let src = provision::find_laragon().ok_or_else(|| AppError::BinaryNotFound("Laragon introuvable (D:/laragon, C:/laragon)".into()))?;
    let c = ctx.inner().clone();
    std::thread::spawn(move || {
        let h = app.clone();
        let res = provision::import_from_laragon(&src, &c.paths, &provision::PHP_VERSIONS, &|l| {
            let _ = h.emit("import-line", l);
        });
        let payload = match res {
            Ok(done) => serde_json::json!({ "ok": true, "done": done }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        let _ = app.emit("import-done", payload);
    });
    Ok(())
}

/// v0.2 (ADR-012) : composants du catalogue et état d'installation.
#[tauri::command]
pub(crate) fn components_list(ctx: State<'_, Arc<Ctx>>) -> Result<serde_json::Value, AppError> {
    let cat = catalog::Catalog::embedded()?;
    let inst = install::Installed::load(&ctx.paths.data.join("installed.json"));
    let list: Vec<_> = cat
        .components
        .iter()
        .map(|c| {
            let versions: Vec<_> = c
                .versions
                .iter()
                .map(|v| {
                    serde_json::json!({
                        "version": v.version,
                        "variant": v.variant,
                        "installed": components::is_installed(&ctx.paths, &inst, c, v),
                        "size": v.size,
                    })
                })
                .collect();
            serde_json::json!({ "id": c.id, "name": c.name, "required": c.required, "versions": versions })
        })
        .collect();
    Ok(serde_json::json!({ "components": list, "installed": inst.components }))
}

/// Supprime un composant installé (dossier de la version ; données MySQL et projets jamais touchés).
#[tauri::command]
pub(crate) async fn remove_component(ctx: State<'_, Arc<Ctx>>, sup: Sup<'_>, id: String, version: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    let s = sup.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("la suppression de composants concerne le mode standalone".into()));
        }
        if c.installing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::NotStartable("une installation est en cours".into()));
        }
        let cat = catalog::Catalog::embedded()?;
        let comp = cat.component(&id).ok_or_else(|| AppError::Config(format!("composant inconnu : {id}")))?;
        let ver = comp.versions.iter().find(|v| v.version == version).ok_or_else(|| AppError::Config(format!("version inconnue : {id} {version}")))?;
        // Un composant utilisé par un service en cours ne se supprime pas.
        let service = match id.as_str() {
            "apache" | "php" | "ext-redis" => Some("apache"),
            "mysql" => Some("mysql"),
            "redis" => Some("redis"),
            "mailpit" => Some("mailpit"),
            _ => None,
        };
        if let Some(sid) = service {
            if s.statuses().iter().any(|st| st.id == sid && st.state == "running") {
                return Err(AppError::NotStartable(format!("arrêtez d'abord le service « {sid} » (Tout arrêter), puis supprimez le composant")));
            }
        }
        let file = c.paths.data.join("installed.json");
        let mut inst = install::Installed::load(&file);
        let removed = components::remove(&c.paths, &mut inst, comp, ver)?;
        inst.save(&file)?;
        Ok(removed)
    })
    .await
}

/// Télécharge, vérifie (SHA-256) et installe un composant. Événements : `component-progress`, `component-done`.
#[tauri::command]
pub(crate) fn install_component(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, id: String, version: String) -> Result<(), AppError> {
    use std::sync::atomic::Ordering;
    if !ctx.paths.standalone {
        return Err(AppError::NotStartable("l'installation de composants concerne le mode standalone".into()));
    }
    let cat = catalog::Catalog::embedded()?;
    let comp = cat.component(&id).cloned().ok_or_else(|| AppError::Config(format!("composant inconnu : {id}")))?;
    let ver = comp.versions.iter().find(|v| v.version == version).cloned().ok_or_else(|| AppError::Config(format!("version inconnue : {id} {version}")))?;
    if ctx.installing.swap(true, Ordering::SeqCst) {
        return Err(AppError::NotStartable("une installation est déjà en cours".into()));
    }
    ctx.install_cancel.store(false, Ordering::SeqCst);
    let c = ctx.inner().clone();
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let (pid, pver) = (comp.id.clone(), ver.version.clone());
        let res = components::install(
            &c.paths.root,
            &c.paths.data,
            &comp,
            &ver,
            &components::Options { allow_local_http: false },
            &c.install_cancel,
            |got, total| {
                if last.elapsed() >= std::time::Duration::from_millis(250) {
                    last = std::time::Instant::now();
                    let _ = app.emit("component-progress", serde_json::json!({ "id": pid, "version": pver, "received": got, "total": total }));
                }
            },
            None,
        );
        let payload = match res {
            Ok(out) => {
                if comp.id == "php" || comp.id == "ext-redis" {
                    for dir in c.paths.php_dirs() {
                        let _ = provision::write_php_ini(&dir, &c.paths.ssl_dir().join("cacert.pem"));
                    }
                }
                let note = match out {
                    components::Outcome::Installed(r) => format!("installé : {} {}", r.id, r.version),
                    components::Outcome::AlreadyInstalled(p) => format!("déjà installé : {}", p.display()),
                };
                serde_json::json!({ "id": comp.id, "version": ver.version, "ok": true, "note": note })
            }
            Err(e) => serde_json::json!({ "id": comp.id, "version": ver.version, "ok": false, "error": e.to_string() }),
        };
        c.installing.store(false, Ordering::SeqCst);
        let _ = app.emit("component-done", payload);
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn cancel_install(ctx: State<'_, Arc<Ctx>>) {
    ctx.install_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}
