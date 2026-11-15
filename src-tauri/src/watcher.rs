//! Surveillance de www/ (F-016) : signale chaque nouveau dossier de projet.
use crate::error::AppError;
use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};

/// Nom du dossier créé directement sous `www` (ni caché, ni caractère exotique), sinon None.
pub fn new_project_name(www: &Path, path: &Path) -> Option<String> {
    if path.parent()? != www || !path.is_dir() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_string();
    let ok = !name.starts_with('.')
        && !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then_some(name)
}

/// Démarre la surveillance non récursive de `www`. Le watcher retourné doit rester vivant.
pub fn start<F>(www: PathBuf, on_new: F) -> Result<RecommendedWatcher, AppError>
where
    F: Fn(String) + Send + 'static,
{
    let base = www.clone();
    let mut w = recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, EventKind::Create(_)) {
                for p in &ev.paths {
                    if let Some(name) = new_project_name(&base, p) {
                        on_new(name);
                    }
                }
            }
        }
    })
    .map_err(|e| AppError::Io(e.to_string()))?;
    w.watch(&www, RecursiveMode::NonRecursive).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn filters_names() {
        let t = std::env::temp_dir().join(format!("astral-dev-wf-{}", std::process::id()));
        std::fs::create_dir_all(t.join("demo")).unwrap();
        std::fs::create_dir_all(t.join(".hidden")).unwrap();
        std::fs::create_dir_all(t.join("demo/sub")).unwrap();
        assert_eq!(new_project_name(&t, &t.join("demo")).as_deref(), Some("demo"));
        assert_eq!(new_project_name(&t, &t.join(".hidden")), None);
        assert_eq!(new_project_name(&t, &t.join("demo/sub")), None); // pas directement sous www
        assert_eq!(new_project_name(&t, &t.join("absent")), None);
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn real_watcher_reports_a_new_folder() {
        let t = std::env::temp_dir().join(format!("astral-dev-ww-{}", std::process::id()));
        std::fs::create_dir_all(&t).unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = start(t.clone(), move |n| {
            let _ = tx.send(n);
        })
        .unwrap();
        std::fs::create_dir_all(t.join("nouveau-projet")).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "nouveau-projet");
        drop(_w);
        std::fs::remove_dir_all(&t).unwrap();
    }
}