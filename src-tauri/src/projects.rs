//! Projets Astral sous www/ : liste, détection, modèles de création (SPEC F-010…F-012).
use crate::error::AppError;
use crate::hosts;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Q-1 (2026-11-14) : quatre modèles. Noms vérifiés dans les composer.json locaux.
pub const TEMPLATES: [(&str, &str); 4] = [
    ("starter", "astral-php/astral-starter"),
    ("shop", "astral-php/astral-shop"),
    ("blog", "astral-php/astral-blog"),
    ("forum", "astral-php/astral-forum"),
];

pub fn template_package(t: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|(k, _)| *k == t).map(|(_, p)| *p)
}

/// F-011 : `^[a-z][a-z0-9-]{1,40}$` (et pas de tiret final).
pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    (2..=41).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !n.ends_with('-')
}

/// Composant de chemin sûr pour désigner un projet existant (pas de séparateur, pas de « .. »).
fn safe_component(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('.')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !n.contains("..")
}

pub fn project_dir(www: &Path, name: &str) -> Result<PathBuf, AppError> {
    if !safe_component(name) {
        return Err(AppError::Io(format!("nom de projet invalide : {name}")));
    }
    let p = www.join(name);
    if !p.is_dir() {
        return Err(AppError::Io(format!("projet introuvable : {name}")));
    }
    Ok(p)
}

#[derive(Serialize, Debug, PartialEq)]
pub struct ProjectInfo {
    pub name: String,
    pub astral: bool,
    pub package: Option<String>,
    pub version: Option<String>,
    pub domain: String,
    pub vhost: bool,
    pub hosts: bool,
    /// Le vhost contient un bloc HTTPS (`SSLEngine on`).
    pub https: bool,
    pub cache_driver: Option<String>,
}

/// Valeur d'une clé d'un .env (sans interpréter les variables ; guillemets retirés).
pub fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        if l.starts_with('#') {
            return None;
        }
        let (k, v) = l.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.split(" #").next().unwrap_or(v).trim().trim_matches(['"', '\'']);
        Some(v.to_string())
    })
}

pub fn list(www: &Path, tld: &str, sites_dir: &Path, hosts_text: &str) -> Result<Vec<ProjectInfo>, AppError> {
    let mut out = vec![];
    for e in std::fs::read_dir(www)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        if !e.path().is_dir() || !safe_component(&name) {
            continue;
        }
        let dir = e.path();
        let composer: Option<serde_json::Value> =
            std::fs::read_to_string(dir.join("composer.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
        let get = |k: &str| composer.as_ref().and_then(|c| c.get(k)).and_then(|v| v.as_str()).map(String::from);
        let domain = format!("{name}.{tld}");
        let env = std::fs::read_to_string(dir.join(".env")).ok();
        out.push(ProjectInfo {
            astral: dir.join("bin").join("console").is_file(),
            package: get("name"),
            version: get("version"),
            vhost: sites_dir.join(format!("auto.{domain}.conf")).is_file(),
            hosts: hosts::has_domain(hosts_text, &domain),
            https: std::fs::read_to_string(sites_dir.join(format!("auto.{domain}.conf"))).map_or(false, |t| t.contains("SSLEngine on")),
            cache_driver: env.as_deref().and_then(|t| env_value(t, "CACHE_DRIVER")),
            domain,
            name,
        });
    }
    out.sort_by(|a, b| b.astral.cmp(&a.astral).then(a.name.cmp(&b.name)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_templates() {
        for ok in ["demo", "astral-demo", "a1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "a", "1abc", "Demo", "a b", "a_b", "../x", "a/b", "demo-", "a.b", &"x".repeat(42)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(template_package("shop"), Some("astral-php/astral-shop"));
        assert_eq!(template_package("evil"), None);
    }

    #[test]
    fn env_parsing() {
        let t = "# CACHE_DRIVER=file\nAPP_NAME=\"Astral Blog\"\nCACHE_DRIVER=redis # prod\n";
        assert_eq!(env_value(t, "CACHE_DRIVER").as_deref(), Some("redis"));
        assert_eq!(env_value(t, "APP_NAME").as_deref(), Some("Astral Blog"));
        assert_eq!(env_value(t, "NOPE"), None);
    }

    #[test]
    fn project_dir_rejects_traversal() {
        let t = std::env::temp_dir();
        assert!(project_dir(&t, "..").is_err());
        assert!(project_dir(&t, "a/b").is_err());
        assert!(project_dir(&t, "inexistant-xyz").is_err());
    }

    #[test]
    fn lists_and_detects_astral_projects() {
        let t = std::env::temp_dir().join(format!("astral-dev-www-{}", std::process::id()));
        let sites = t.join("sites");
        std::fs::create_dir_all(t.join("www/demo/bin")).unwrap();
        std::fs::create_dir_all(t.join("www/plain")).unwrap();
        std::fs::create_dir_all(t.join("www/.hidden")).unwrap();
        std::fs::create_dir_all(&sites).unwrap();
        std::fs::write(t.join("www/demo/bin/console"), "").unwrap();
        std::fs::write(t.join("www/demo/composer.json"), r#"{"name":"astral-php/astral-blog","version":"2.0.2"}"#).unwrap();
        std::fs::write(t.join("www/demo/.env"), "CACHE_DRIVER=redis\n").unwrap();
        std::fs::write(sites.join("auto.demo.local.conf"), "").unwrap();
        let l = list(&t.join("www"), "local", &sites, "127.0.0.1 demo.local\n").unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].name, "demo");
        assert!(l[0].astral && l[0].vhost && l[0].hosts);
        assert_eq!(l[0].cache_driver.as_deref(), Some("redis"));
        assert_eq!(l[0].package.as_deref(), Some("astral-php/astral-blog"));
        assert!(!l[1].astral && !l[1].vhost && !l[1].hosts);
        std::fs::remove_dir_all(&t).unwrap();
    }
}