//! Vhost Apache au format « auto » de Laragon : etc/apache2/sites-enabled/auto.<domaine>.conf (F-014).
use crate::error::AppError;
use std::path::{Path, PathBuf};

pub fn conf_path(sites_dir: &Path, domain: &str) -> PathBuf {
    sites_dir.join(format!("auto.{domain}.conf"))
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// `ssl` = (certificat, clé) si disponibles : sinon seul le bloc :80 est généré.
pub fn render(domain: &str, public_dir: &Path, ssl: Option<(&Path, &Path)>, port: u16) -> String {
    let mut s = format!(
        "define ROOT \"{root}\"\ndefine SITE \"{domain}\"\n\n<VirtualHost *:{port}>\n    DocumentRoot \"${{ROOT}}\"\n    ServerName ${{SITE}}\n    ServerAlias *.${{SITE}}\n    <Directory \"${{ROOT}}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n</VirtualHost>\n",
        root = slash(public_dir),
        port = port
    );
    if let Some((crt, key)) = ssl {
        s.push_str(&format!(
            "\n<VirtualHost *:443>\n    DocumentRoot \"${{ROOT}}\"\n    ServerName ${{SITE}}\n    ServerAlias *.${{SITE}}\n    <Directory \"${{ROOT}}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n\n    SSLEngine on\n    SSLCertificateFile      {}\n    SSLCertificateKeyFile   {}\n</VirtualHost>\n",
            slash(crt),
            slash(key)
        ));
    }
    s
}

pub enum VhostOutcome {
    Created,
    AlreadyExists,
}

/// N'écrase jamais un vhost existant (celui de Laragon par exemple).
/// `certs` : certificat mkcert (crt, clé) à utiliser ; sinon celui de Laragon s'il existe.
pub fn ensure(
    sites_dir: &Path,
    domain: &str,
    public_dir: &Path,
    ssl_dir: &Path,
    certs: Option<(PathBuf, PathBuf)>,
    port: u16,
) -> Result<VhostOutcome, AppError> {
    let path = conf_path(sites_dir, domain);
    if path.exists() {
        return Ok(VhostOutcome::AlreadyExists);
    }
    if !public_dir.is_dir() {
        return Err(AppError::Io(format!("dossier public/ introuvable : {}", public_dir.display())));
    }
    let (crt, key) = certs.unwrap_or_else(|| (ssl_dir.join("laragon.crt"), ssl_dir.join("laragon.key")));
    let ssl = if crt.is_file() && key.is_file() { Some((crt.as_path(), key.as_path())) } else { None };
    std::fs::create_dir_all(sites_dir)?;
    std::fs::write(&path, render(domain, public_dir, ssl, port))?;
    Ok(VhostOutcome::Created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_laragon_compatible_conf() {
        let s = render("demo.local", Path::new("D:\\laragon\\www\\demo\\public"), None, 80);
        assert!(s.contains("define ROOT \"D:/laragon/www/demo/public\""));
        assert!(s.contains("define SITE \"demo.local\""));
        assert!(s.contains("<VirtualHost *:80>") && !s.contains(":443"));
        let s = render("demo.local", Path::new("x"), Some((Path::new("a.crt"), Path::new("a.key"))), 80);
        assert!(s.contains("SSLCertificateFile      a.crt"));
    }

    #[test]
    fn never_overwrites_an_existing_vhost() {
        let t = std::env::temp_dir().join(format!("astral-dev-vh-{}", std::process::id()));
        let public = t.join("www/demo/public");
        std::fs::create_dir_all(&public).unwrap();
        let sites = t.join("sites");
        assert!(matches!(ensure(&sites, "demo.local", &public, &t.join("ssl"), None, 80).unwrap(), VhostOutcome::Created));
        std::fs::write(conf_path(&sites, "demo.local"), "MINE").unwrap();
        assert!(matches!(ensure(&sites, "demo.local", &public, &t.join("ssl"), None, 80).unwrap(), VhostOutcome::AlreadyExists));
        assert_eq!(std::fs::read_to_string(conf_path(&sites, "demo.local")).unwrap(), "MINE");
        assert!(ensure(&sites, "x.local", &t.join("absent"), &t, None, 80).is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }
}