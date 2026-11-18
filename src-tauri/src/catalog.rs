//! Catalogue de composants (v0.2, ADR-012) : modèle, lecture, validation.
#![allow(dead_code)] // câblé aux commandes Tauri au lot 7.4 (ADR-012)
//! Le catalogue ne peut que RESTREINDRE la liste blanche de domaines intégrée au code.
use crate::error::AppError;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Domaines autorisés pour un téléchargement (HTTPS uniquement), en dur dans le code.
pub const BUILTIN_HOSTS: &[&str] = &[
    "downloads.php.net",
    "windows.php.net",
    "www.apachelounge.com",
    "cdn.mysql.com",
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "getcomposer.org",
    "curl.se",
    "raw.githubusercontent.com",
];

pub const SCHEMA: u32 = 1;
/// Catalogue embarqué dans l'exécutable.
pub const EMBEDDED: &str = include_str!("../catalog.json");

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Catalog {
    pub schema: u32,
    #[serde(default)]
    pub generated: String,
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Component {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub required: bool,
    pub allowed_hosts: Vec<String>,
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Version {
    pub version: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub sha256: Sha256Spec,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub strip_prefix: Option<String>,
    pub extract_to: String,
    #[serde(default)]
    pub check: Vec<String>,
    #[serde(default)]
    pub single_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Source {
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub asset: Option<String>,
}

/// Comment obtenir la somme : épinglée, ou depuis un fichier officiel.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Sha256Spec {
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Chemin relatif sûr : ni absolu, ni lecteur, ni `..`.
pub fn is_safe_rel(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return false;
    }
    !p.replace('\\', "/").split('/').any(|c| c == "..")
}

pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.contains('@') || host.is_empty() {
        return None; // jamais d'identifiants dans l'URL
    }
    Some(host.split(':').next()?.to_ascii_lowercase())
}

pub fn host_allowed(url: &str, allowed: &[String]) -> bool {
    match host_of(url) {
        Some(h) => BUILTIN_HOSTS.contains(&h.as_str()) && allowed.iter().any(|a| a.eq_ignore_ascii_case(&h)),
        None => false,
    }
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Catalog, AppError> {
        let c: Catalog = serde_json::from_str(text).map_err(|e| AppError::Config(format!("catalogue : {e}")))?;
        c.validate()?;
        Ok(c)
    }

    pub fn embedded() -> Result<Catalog, AppError> {
        Catalog::parse(EMBEDDED)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let bad = |m: String| Err(AppError::Config(format!("catalogue : {m}")));
        if self.schema != SCHEMA {
            return bad(format!("schéma {} non pris en charge (attendu {SCHEMA})", self.schema));
        }
        for c in &self.components {
            for h in &c.allowed_hosts {
                if !BUILTIN_HOSTS.iter().any(|b| b.eq_ignore_ascii_case(h)) {
                    return bad(format!("{} : hôte {h} hors liste blanche", c.id));
                }
            }
            for v in &c.versions {
                let id = format!("{} {}", c.id, v.version);
                if !is_safe_rel(&v.extract_to) {
                    return bad(format!("{id} : extract_to non sûr"));
                }
                if v.check.iter().any(|p| !is_safe_rel(p)) {
                    return bad(format!("{id} : chemin `check` non sûr"));
                }
                if let Some(u) = &v.url {
                    if !host_allowed(u, &c.allowed_hosts) {
                        return bad(format!("{id} : URL hors liste blanche ({u})"));
                    }
                }
                if let Some(u) = v.sha256.url.as_ref().or_else(|| v.source.as_ref().and_then(|s| s.url.as_ref())) {
                    if !host_allowed(u, &c.allowed_hosts) {
                        return bad(format!("{id} : URL de somme hors liste blanche ({u})"));
                    }
                }
                if let Some(p) = &v.sha256.pinned {
                    if !is_hex64(p) {
                        return bad(format!("{id} : somme épinglée invalide"));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }
}

impl Version {
    /// Sans somme vérifiable (ni épinglée ni source officielle), une version n'est jamais installable.
    pub fn verifiable(&self) -> bool {
        self.sha256.pinned.is_some() || self.sha256.from.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_valid() {
        let c = Catalog::embedded().unwrap();
        assert!(c.component("composer").is_some());
    }

    #[test]
    fn rejects_unknown_schema_and_foreign_hosts() {
        assert!(Catalog::parse(r#"{"schema":2,"components":[]}"#).is_err());
        let evil = r#"{"schema":1,"components":[{"id":"x","allowed_hosts":["evil.example"],"versions":[]}]}"#;
        assert!(Catalog::parse(evil).is_err());
        let url = r#"{"schema":1,"components":[{"id":"x","allowed_hosts":["curl.se"],"versions":[
            {"version":"1","url":"https://evil.example/a.zip","sha256":{"pinned":null},"extract_to":"bin/x"}]}]}"#;
        assert!(Catalog::parse(url).is_err());
    }

    #[test]
    fn rejects_unsafe_paths_and_bad_pins() {
        let mk = |extract: &str, pin: &str| {
            format!(
                r#"{{"schema":1,"components":[{{"id":"x","allowed_hosts":["curl.se"],"versions":[
                {{"version":"1","url":"https://curl.se/a.zip","sha256":{{"pinned":"{pin}"}},"extract_to":"{extract}"}}]}}]}}"#
            )
        };
        let ok = "a".repeat(64);
        assert!(Catalog::parse(&mk("bin/x", &ok)).is_ok());
        assert!(Catalog::parse(&mk("../x", &ok)).is_err());
        assert!(Catalog::parse(&mk("C:/x", &ok)).is_err());
        assert!(Catalog::parse(&mk("bin/x", "zz")).is_err());
    }

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://Curl.SE/ca.pem").as_deref(), Some("curl.se"));
        assert_eq!(host_of("http://curl.se/x"), None);
        assert_eq!(host_of("https://user@curl.se/x"), None);
        assert!(!host_allowed("https://github.com.evil.example/x", &["github.com".into()]));
    }
}
