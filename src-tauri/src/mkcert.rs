//! mkcert (F-015) : détection et génération d'un certificat par domaine. Aucun téléchargement automatique.
use crate::error::AppError;
use crate::hosts::valid_domain;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Ordre : chemin configuré, PATH, <root>/bin/mkcert/mkcert.exe, <root>/bin/mkcert.exe.
pub fn find(root: &Path, configured: &str) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        return p.is_file().then_some(p);
    }
    let in_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join("mkcert.exe")).find(|p| p.is_file())
    });
    in_path.or_else(|| {
        [root.join("bin").join("mkcert").join("mkcert.exe"), root.join("bin").join("mkcert.exe")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

pub fn cert_paths(ssl_dir: &Path, domain: &str) -> (PathBuf, PathBuf) {
    (ssl_dir.join(format!("{domain}.pem")), ssl_dir.join(format!("{domain}-key.pem")))
}

pub fn args(domain: &str, crt: &Path, key: &Path) -> Vec<String> {
    vec![
        "-cert-file".into(),
        crt.to_string_lossy().to_string(),
        "-key-file".into(),
        key.to_string_lossy().to_string(),
        domain.to_string(),
        format!("*.{domain}"),
    ]
}

/// Génère (ou réutilise) le certificat de `domain`. N'installe pas l'autorité racine (`mkcert -install` reste manuel).
pub fn generate(mkcert: &Path, ssl_dir: &Path, domain: &str) -> Result<(PathBuf, PathBuf), AppError> {
    if !valid_domain(domain) {
        return Err(AppError::Config(format!("domaine invalide : {domain}")));
    }
    std::fs::create_dir_all(ssl_dir)?;
    let (crt, key) = cert_paths(ssl_dir, domain);
    if crt.is_file() && key.is_file() {
        return Ok((crt, key));
    }
    let mut cmd = Command::new(mkcert);
    cmd.args(args(domain, &crt, &key)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
    if ok && crt.is_file() && key.is_file() {
        Ok((crt, key))
    } else {
        Err(AppError::StartFailed("mkcert n'a pas produit le certificat".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_mkcert_arguments_with_wildcard() {
        let (c, k) = cert_paths(Path::new("D:/ssl"), "demo.local");
        assert!(c.to_string_lossy().ends_with("demo.local.pem") && k.to_string_lossy().ends_with("demo.local-key.pem"));
        let a = args("demo.local", &c, &k);
        assert_eq!(&a[0], "-cert-file");
        assert_eq!(a[4], "demo.local");
        assert_eq!(a[5], "*.demo.local");
    }

    #[test]
    fn finds_configured_binary_only_if_it_exists() {
        assert!(find(Path::new("Z:/none"), "Z:/absent/mkcert.exe").is_none());
        let t = std::env::temp_dir().join(format!("astral-dev-mk-{}", std::process::id()));
        std::fs::create_dir_all(t.join("bin/mkcert")).unwrap();
        std::fs::write(t.join("bin/mkcert/mkcert.exe"), "").unwrap();
        assert_eq!(find(&t, "").unwrap(), t.join("bin/mkcert/mkcert.exe"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn rejects_invalid_domain_before_running_anything() {
        assert!(generate(Path::new("Z:/nope.exe"), Path::new("."), "bad domain").is_err());
    }
}