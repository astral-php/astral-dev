//! Installation sûre d'un composant (v0.2, ADR-012) : extraction sans zip-slip, contrôle de structure,
//! renommage atomique, `data/installed.json`. Une installation précédente n'est jamais touchée en cas d'échec.
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Taille décompressée maximale (garde-fou contre les archives piégées).
pub const MAX_UNPACKED: u64 = 3 * 1024 * 1024 * 1024;

fn io(msg: impl Into<String>) -> AppError {
    AppError::Io(msg.into())
}

/// `base` + `name` d'archive, ou `None` si le chemin sort de `base` (absolu, `..`, lecteur).
pub fn safe_join(base: &Path, name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') || name.contains(':') {
        return None;
    }
    let mut out = base.to_path_buf();
    for part in Path::new(&name).components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Extrait `zip_path` dans `dest` en retirant `strip_prefix` (ex. `Apache24/`).
pub fn extract_zip(zip_path: &Path, dest: &Path, strip_prefix: Option<&str>) -> Result<u64, AppError> {
    let file = fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| io(format!("archive illisible : {e}")))?;
    fs::create_dir_all(dest)?;
    let prefix = strip_prefix.unwrap_or("").replace('\\', "/");
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| io(format!("archive : {e}")))?;
        let raw = entry.name().map_err(|e| io(format!("nom d'entrée invalide : {e}")))?.replace('\\', "/");
        if entry.is_symlink() {
            return Err(io(format!("lien symbolique refusé dans l'archive : {raw}")));
        }
        let rel = match raw.strip_prefix(prefix.as_str()) {
            Some(r) => r.to_string(),
            None if prefix.is_empty() => raw.clone(),
            None => continue, // hors du dossier racine voulu
        };
        if rel.is_empty() {
            continue;
        }
        let target = safe_join(dest, &rel).ok_or_else(|| io(format!("chemin dangereux refusé : {raw}")))?;
        if entry.is_dir() || raw.ends_with('/') {
            fs::create_dir_all(&target)?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED {
            return Err(io("archive trop volumineuse une fois décompressée"));
        }
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut out = fs::File::create(&target)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buf).map_err(|e| io(format!("décompression : {e}")))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
    }
    Ok(total)
}

/// Installe un composant : extraction dans `<target>.tmp`, contrôle, puis renommage atomique.
pub fn install_zip(
    zip_path: &Path,
    target: &Path,
    strip_prefix: Option<&str>,
    check: &[String],
) -> Result<(), AppError> {
    if target.exists() {
        return Err(io(format!("déjà installé : {}", target.display())));
    }
    let tmp = PathBuf::from(format!("{}.tmp", target.display()));
    let _ = fs::remove_dir_all(&tmp);
    let res = (|| {
        extract_zip(zip_path, &tmp, strip_prefix)?;
        for c in check {
            let p = safe_join(&tmp, c).ok_or_else(|| io(format!("chemin de contrôle non sûr : {c}")))?;
            if !p.exists() {
                return Err(io(format!("structure inattendue : {c} absent après extraction")));
            }
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&tmp, target)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    res
}

/// Installe un composant d'un seul fichier (phar, pem) : copie atomique vers `target_dir/name`.
pub fn install_single(src: &Path, target_dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    if name.contains(['/', '\\']) || name.contains("..") {
        return Err(io("nom de fichier non sûr"));
    }
    fs::create_dir_all(target_dir)?;
    let dest = target_dir.join(name);
    let tmp = target_dir.join(format!("{name}.tmp"));
    fs::copy(src, &tmp)?;
    fs::rename(&tmp, &dest).or_else(|_| {
        let _ = fs::remove_file(&dest);
        fs::rename(&tmp, &dest)
    })?;
    Ok(dest)
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    pub schema: u32,
    pub components: Vec<InstalledComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledComponent {
    pub id: String,
    pub version: String,
    pub dir: String,
    pub sha256: String,
    pub source: String,
    pub installed_at: String,
    /// `download` | `laragon-import`
    pub origin: String,
}

impl Installed {
    pub fn load(path: &Path) -> Installed {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Installed {
            schema: 1,
            components: vec![],
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self).map_err(|e| io(e.to_string()))?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn upsert(&mut self, c: InstalledComponent) {
        self.components.retain(|x| !(x.id == c.id && x.version == c.version));
        self.components.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("astral-dev-inst-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let f = fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opt = zip::write::SimpleFileOptions::default();
        for (n, c) in entries {
            z.start_file(*n, opt).unwrap();
            z.write_all(c).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn safe_join_refuses_escapes() {
        let b = Path::new("C:/astral-dev/bin/x");
        assert!(safe_join(b, "bin/a.exe").is_some());
        assert!(safe_join(b, "../evil").is_none());
        assert!(safe_join(b, "a/../../evil").is_none());
        assert!(safe_join(b, "/abs").is_none());
        assert!(safe_join(b, "C:/win").is_none());
        assert!(safe_join(b, "..\\evil").is_none());
    }

    #[test]
    fn installs_with_prefix_and_check() {
        let d = dir("ok");
        let z = d.join("a.zip");
        make_zip(&z, &[("Apache24/bin/httpd.exe", b"x"), ("Apache24/conf/httpd.conf", b"y"), ("readme.txt", b"z")]);
        let target = d.join("bin").join("apache");
        install_zip(&z, &target, Some("Apache24/"), &["bin/httpd.exe".into()]).unwrap();
        assert!(target.join("bin/httpd.exe").is_file());
        assert!(!target.join("readme.txt").exists(), "hors préfixe ignoré");
        assert!(!PathBuf::from(format!("{}.tmp", target.display())).exists());
        assert!(install_zip(&z, &target, Some("Apache24/"), &[]).is_err(), "déjà installé");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn zip_slip_is_refused_and_leaves_no_trace() {
        let d = dir("slip");
        let z = d.join("evil.zip");
        make_zip(&z, &[("ok.txt", b"a"), ("../../evil.txt", b"b")]);
        let target = d.join("bin").join("x");
        assert!(install_zip(&z, &target, None, &[]).is_err());
        assert!(!target.exists());
        assert!(!PathBuf::from(format!("{}.tmp", target.display())).exists());
        assert!(!d.parent().unwrap().join("evil.txt").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn missing_check_file_cancels_and_keeps_previous() {
        let d = dir("check");
        let z = d.join("a.zip");
        make_zip(&z, &[("other.txt", b"a")]);
        let target = d.join("bin").join("x");
        let err = install_zip(&z, &target, None, &["redis-server.exe".into()]).unwrap_err();
        assert!(err.to_string().contains("redis-server.exe"));
        assert!(!target.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn single_file_and_installed_json_roundtrip() {
        let d = dir("single");
        let src = d.join("c.phar");
        fs::write(&src, b"phar").unwrap();
        let dest = install_single(&src, &d.join("bin/composer"), "composer.phar").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"phar");
        assert!(install_single(&src, &d, "../x").is_err());

        let mut inst = Installed::load(&d.join("data/installed.json"));
        inst.upsert(InstalledComponent {
            id: "composer".into(),
            version: "2.9".into(),
            dir: "bin/composer".into(),
            sha256: "a".repeat(64),
            source: "https://getcomposer.org/".into(),
            installed_at: "2026-11-18T10:00:00Z".into(),
            origin: "download".into(),
        });
        inst.save(&d.join("data/installed.json")).unwrap();
        assert_eq!(Installed::load(&d.join("data/installed.json")), inst);
        fs::remove_dir_all(&d).unwrap();
    }
}
