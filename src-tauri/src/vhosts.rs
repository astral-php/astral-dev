//! Vhost Apache au format « auto » de Laragon : etc/apache2/sites-enabled/auto.<domaine>.conf (F-014).
#[cfg(test)]
use crate::error::AppError;
use std::path::{Path, PathBuf};

pub fn conf_path(sites_dir: &Path, domain: &str) -> PathBuf {
    sites_dir.join(format!("auto.{domain}.conf"))
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// `ssl` = (certificat, clé) si disponibles : sinon seul le bloc :80 est généré.
pub fn render(domain: &str, public_dir: &Path, ssl: Option<(&Path, &Path)>, port: u16, ssl_port: u16) -> String {
    let mut s = format!(
        "define ROOT \"{root}\"\ndefine SITE \"{domain}\"\n\n<VirtualHost *:{port}>\n    DocumentRoot \"${{ROOT}}\"\n    ServerName ${{SITE}}\n    ServerAlias *.${{SITE}}\n    <Directory \"${{ROOT}}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n</VirtualHost>\n",
        root = slash(public_dir),
        port = port
    );
    if let Some((crt, key)) = ssl {
        s.push_str(&format!(
            "\n<VirtualHost *:{ssl_port}>\n    DocumentRoot \"${{ROOT}}\"\n    ServerName ${{SITE}}\n    ServerAlias *.${{SITE}}\n    <Directory \"${{ROOT}}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n\n    SSLEngine on\n    SSLCertificateFile      {}\n    SSLCertificateKeyFile   {}\n</VirtualHost>\n",
            slash(crt),
            slash(key),
            ssl_port = ssl_port
        ));
    }
    s
}

#[cfg(test)]
pub enum VhostOutcome {
    Created,
    AlreadyExists,
}

/// N'écrase jamais un vhost existant (celui de Laragon par exemple).
/// `certs` : certificat mkcert (crt, clé) à utiliser ; sinon celui de Laragon s'il existe.
#[cfg(test)]
pub fn ensure(
    sites_dir: &Path,
    domain: &str,
    public_dir: &Path,
    ssl_dir: &Path,
    certs: Option<(PathBuf, PathBuf)>,
    port: u16,
    ssl_port: u16,
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
    std::fs::write(&path, render(domain, public_dir, ssl, port, ssl_port))?;
    Ok(VhostOutcome::Created)
}

pub(crate) fn rewrite_ports(text: &str, port: u16, ssl_port: u16) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_header = |l: &str| l.trim().strip_prefix("<VirtualHost *:").and_then(|r| r.strip_suffix('>')).map_or(false, |n| n.chars().all(|c| c.is_ascii_digit()));
    // Un bloc est SSL s'il contient `SSLEngine on` avant sa fermeture.
    let mut ssl_headers = std::collections::HashSet::new();
    let mut current: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if is_header(l) {
            current = Some(i);
        } else if l.trim() == "</VirtualHost>" {
            current = None;
        } else if l.trim().eq_ignore_ascii_case("SSLEngine on") {
            if let Some(h) = current {
                ssl_headers.insert(h);
            }
        }
    }
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if is_header(l) {
                let eol = &l[l.trim_end().len()..];
                let n = if ssl_headers.contains(&i) { ssl_port } else { port };
                format!("<VirtualHost *:{n}>{eol}")
            } else {
                (*l).to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_http_and_ssl_ports_by_block() {
        let ssl = render("a.test", Path::new("x"), Some((Path::new("a.crt"), Path::new("a.key"))), 80, 443);
        let out = rewrite_ports(&ssl, 8080, 8443);
        assert!(out.contains("<VirtualHost *:8080>") && out.contains("<VirtualHost *:8443>"));
        assert!(!out.contains("*:80>") && !out.contains("*:443>"));
        // un 2e passage (ports déjà alignés) ne change rien ; les fins de ligne Windows sont conservées
        assert_eq!(rewrite_ports(&out, 8080, 8443), out);
        let crlf = ssl.replace('\n', "\r\n");
        assert!(rewrite_ports(&crlf, 81, 444).contains("<VirtualHost *:81>\r\n"));
    }

    #[test]
    fn renders_laragon_compatible_conf() {
        let s = render("demo.local", Path::new("D:\\laragon\\www\\demo\\public"), None, 80, 443);
        assert!(s.contains("define ROOT \"D:/laragon/www/demo/public\""));
        assert!(s.contains("define SITE \"demo.local\""));
        assert!(s.contains("<VirtualHost *:80>") && !s.contains(":443"));
        let s = render("demo.local", Path::new("x"), Some((Path::new("a.crt"), Path::new("a.key"))), 80, 443);
        assert!(s.contains("SSLCertificateFile      a.crt"));
    }

    #[test]
    fn never_overwrites_an_existing_vhost() {
        let t = std::env::temp_dir().join(format!("astral-dev-vh-{}", std::process::id()));
        let public = t.join("www/demo/public");
        std::fs::create_dir_all(&public).unwrap();
        let sites = t.join("sites");
        assert!(matches!(ensure(&sites, "demo.local", &public, &t.join("ssl"), None, 80, 443).unwrap(), VhostOutcome::Created));
        std::fs::write(conf_path(&sites, "demo.local"), "MINE").unwrap();
        assert!(matches!(ensure(&sites, "demo.local", &public, &t.join("ssl"), None, 80, 443).unwrap(), VhostOutcome::AlreadyExists));
        assert_eq!(std::fs::read_to_string(conf_path(&sites, "demo.local")).unwrap(), "MINE");
        assert!(ensure(&sites, "x.local", &t.join("absent"), &t, None, 80, 443).is_err());
        std::fs::remove_dir_all(&t).unwrap();
    }
}