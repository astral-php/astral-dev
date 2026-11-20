//! Commandes Tauri : journaux de data/logs.
use crate::error::AppError;
use std::sync::Arc;
use tauri::{State};
use super::{Ctx};

/// Fichiers de logs d'astral-dev (`data/logs/*.log`) : nom + taille.
#[tauri::command]
pub(crate) fn list_logs(ctx: State<'_, Arc<Ctx>>) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = std::fs::read_dir(ctx.paths.logs())
        .map(|r| {
            r.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    let ok = n.ends_with(".log") && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
                    ok.then(|| (n, e.metadata().map(|m| m.len()).unwrap_or(0)))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Dernières lignes d'un log (`name` validé : jamais de chemin).
#[tauri::command]
pub(crate) fn tail_log(ctx: State<'_, Arc<Ctx>>, name: String, lines: Option<usize>) -> Result<Vec<String>, AppError> {
    use std::io::{Read, Seek, SeekFrom};
    if !name.ends_with(".log") || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err(AppError::Config("nom de journal invalide".into()));
    }
    let mut f = std::fs::File::open(ctx.paths.logs().join(&name)).map_err(|_| AppError::Io(format!("{name} introuvable")))?;
    let len = f.metadata()?.len();
    let want = 512 * 1024;
    f.seek(SeekFrom::Start(len.saturating_sub(want)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let n = lines.unwrap_or(400).min(5000);
    Ok(all[all.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect())
}
