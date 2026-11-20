//! Source de vérité des sites (ADR-015) : `data/sites.toml`. Les fichiers des moteurs web (vhosts Apache, plus tard
//! `Caddyfile`) en sont dérivés. Écriture atomique, sauvegarde unique `sites.toml.bak`, migration depuis les vhosts
//! existants (format « auto » : `define ROOT` / `define SITE`) sans rien supprimer.
use crate::error::AppError;
use crate::hosts::valid_domain;
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteConfig {
    /// Dossier du projet sous `www/`.
    pub name: String,
    /// `<name>.<tld>`.
    pub domain: String,
    /// Racine web : relative à la racine de données, ou absolue si `www/` est ailleurs.
    pub root: String,
    /// Version de PHP du site (série, ex. `8.3`) ; absent = version par défaut. Utilisée à partir de la phase 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// HTTPS voulu : le bloc n'est généré que si le certificat existe.
    #[serde(default)]
    pub ssl: bool,
    /// `caddy` | `apache` (topologie `caddy+apache`, phase 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sites {
    #[serde(default = "schema_one")]
    pub schema: u32,
    #[serde(default, rename = "site")]
    pub sites: Vec<SiteConfig>,
}

fn schema_one() -> u32 {
    SCHEMA
}

impl Default for Sites {
    fn default() -> Self {
        Self { schema: SCHEMA, sites: vec![] }
    }
}

pub fn path(paths: &AppPaths) -> PathBuf {
    paths.data.join("sites.toml")
}

/// Racine web d'un projet telle qu'elle est stockée : relative à `paths.root` si possible, sinon absolue.
pub fn root_for(paths: &AppPaths, public_dir: &Path) -> String {
    let abs = public_dir.to_string_lossy().replace('\\', "/");
    let root = paths.root.to_string_lossy().replace('\\', "/");
    match abs.strip_prefix(&format!("{}/", root.trim_end_matches('/'))) {
        Some(rel) => rel.to_string(),
        None => abs,
    }
}

/// Chemin absolu de la racine web d'un site.
pub fn root_abs(paths: &AppPaths, site: &SiteConfig) -> PathBuf {
    let p = Path::new(&site.root);
    if p.is_absolute() || site.root.contains(':') {
        p.to_path_buf()
    } else {
        paths.root.join(&site.root)
    }
}

/// Extrait `(domaine, racine, https)` d'un vhost au format « auto ». `None` si le fichier n'a pas ce format.
pub fn parse_vhost(text: &str) -> Option<(String, String, bool)> {
    let get = |key: &str| {
        text.lines().find_map(|l| {
            let rest = l.trim().strip_prefix("define ")?.strip_prefix(key)?.trim();
            Some(rest.trim_matches('"').to_string())
        })
    };
    if !text.trim_start().starts_with("define ROOT") {
        return None;
    }
    let (root, site) = (get("ROOT")?, get("SITE")?);
    if root.is_empty() || site.is_empty() {
        return None;
    }
    Some((site, root, text.contains("SSLEngine on")))
}

impl Sites {
    /// Charge `sites.toml` ; absent = liste vide. Un fichier invalide est une erreur (on ne devine pas).
    pub fn load(file: &Path) -> Result<Sites, AppError> {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Sites::default()),
            Err(e) => return Err(e.into()),
        };
        let s: Sites = toml::from_str(&text).map_err(|e| AppError::Config(format!("sites.toml invalide : {e}")))?;
        s.validate()?;
        Ok(s)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let bad = |m: String| Err(AppError::Config(format!("sites.toml : {m}")));
        if self.schema != SCHEMA {
            return bad(format!("schéma {} non pris en charge (attendu {SCHEMA})", self.schema));
        }
        for (i, s) in self.sites.iter().enumerate() {
            if !valid_domain(&s.domain) {
                return bad(format!("domaine invalide : {}", s.domain));
            }
            if s.name.is_empty() || s.root.split(['/', '\\']).any(|c| c == "..") {
                return bad(format!("site {} : nom ou racine invalide", s.domain));
            }
            // La racine est écrite entre guillemets dans la configuration du serveur : pas de guillemet ni de
            // caractère de contrôle (injection de directives).
            if s.root.chars().any(|c| c == '"' || c.is_control()) || s.name.chars().any(|c| c.is_control()) {
                return bad(format!("site {} : caractère interdit dans le nom ou la racine", s.domain));
            }
            if self.sites[..i].iter().any(|o| o.domain.eq_ignore_ascii_case(&s.domain)) {
                return bad(format!("domaine en double : {}", s.domain));
            }
        }
        Ok(())
    }

    /// Écriture atomique (fichier temporaire puis renommage) ; sauvegarde unique `sites.toml.bak` d'un fichier existant.
    pub fn save(&self, file: &Path) -> Result<(), AppError> {
        self.validate()?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if file.is_file() {
            let bak = file.with_extension("toml.bak");
            if !bak.exists() {
                std::fs::copy(file, &bak)?;
            }
        }
        let text = format!(
            "# Généré par astral-dev : source de vérité des sites (les fichiers des serveurs web en sont dérivés).\n{}",
            toml::to_string_pretty(self).map_err(|e| AppError::Config(e.to_string()))?
        );
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file)?;
        Ok(())
    }

    pub fn get(&self, domain: &str) -> Option<&SiteConfig> {
        self.sites.iter().find(|s| s.domain.eq_ignore_ascii_case(domain))
    }

    /// Ajoute le site ou le remplace (même domaine).
    pub fn upsert(&mut self, site: SiteConfig) {
        match self.sites.iter_mut().find(|s| s.domain.eq_ignore_ascii_case(&site.domain)) {
            Some(s) => *s = site,
            None => self.sites.push(site),
        }
    }

    /// Retire le site ; `true` s'il existait.
    pub fn remove(&mut self, domain: &str) -> bool {
        let n = self.sites.len();
        self.sites.retain(|s| !s.domain.eq_ignore_ascii_case(domain));
        self.sites.len() != n
    }

    /// Migration : un `[[site]]` par vhost « auto » du dossier (rien n'est supprimé ni modifié).
    /// Les vhosts personnalisés (sans `define ROOT`) sont ignorés : ils restent la propriété de l'utilisateur.
    pub fn migrate_from_vhosts(sites_dir: &Path, paths: &AppPaths, tld: &str) -> Sites {
        let mut out = Sites::default();
        let Ok(rd) = std::fs::read_dir(sites_dir) else { return out };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if !(name.starts_with("auto.") && name.ends_with(".conf")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let Some((domain, root, ssl)) = parse_vhost(&text) else { continue };
            if !valid_domain(&domain) {
                continue;
            }
            let project = domain.strip_suffix(&format!(".{tld}")).unwrap_or(domain.split('.').next().unwrap_or(&domain)).to_string();
            let root_rel = root_for(paths, Path::new(&root));
            out.upsert(SiteConfig { name: project, domain, root: root_rel, php: None, ssl, backend: None });
        }
        out
    }

    /// Charge `sites.toml` ; s'il n'existe pas encore, le crée par migration depuis les vhosts (mode autonome seulement :
    /// en mode adopt, les vhosts appartiennent à Laragon et ne sont pas importés).
    pub fn load_or_migrate(paths: &AppPaths, tld: &str) -> Result<Sites, AppError> {
        let file = path(paths);
        if file.is_file() {
            return Sites::load(&file);
        }
        if !paths.standalone {
            return Ok(Sites::default());
        }
        let s = Sites::migrate_from_vhosts(&paths.sites_enabled(), paths, tld);
        if !s.sites.is_empty() {
            s.save(&file)?;
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn tmp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("astral-dev-sites-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        t
    }

    fn paths_in(t: &Path, standalone: bool) -> AppPaths {
        let mut cfg = Config::default();
        cfg.mode = if standalone { "standalone".into() } else { "adopt".into() };
        let r = t.to_string_lossy().replace('\\', "/");
        cfg.paths.root = r.clone();
        cfg.paths.www = format!("{r}/www");
        cfg.paths.data = format!("{r}/data");
        AppPaths::from_config(&cfg)
    }

    fn site(name: &str) -> SiteConfig {
        SiteConfig { name: name.into(), domain: format!("{name}.test"), root: format!("www/{name}/public"), php: None, ssl: false, backend: None }
    }

    #[test]
    fn roundtrip_atomic_save_and_single_backup() {
        let t = tmp("rt");
        let f = t.join("data/sites.toml");
        let mut s = Sites::default();
        s.upsert(site("a"));
        s.upsert(SiteConfig { ssl: true, php: Some("8.4".into()), backend: Some("caddy".into()), ..site("b") });
        s.save(&f).unwrap();
        assert!(!f.with_extension("toml.tmp").exists());
        assert_eq!(Sites::load(&f).unwrap(), s);
        assert!(!f.with_extension("toml.bak").exists(), "pas de sauvegarde à la création");
        s.remove("a.test");
        s.save(&f).unwrap();
        let bak = std::fs::read_to_string(f.with_extension("toml.bak")).unwrap();
        s.upsert(site("c"));
        s.save(&f).unwrap();
        assert_eq!(std::fs::read_to_string(f.with_extension("toml.bak")).unwrap(), bak, "sauvegarde unique");
        assert!(std::fs::read_to_string(&f).unwrap().contains("[[site]]"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn missing_file_is_empty_but_invalid_file_is_an_error() {
        let t = tmp("inv");
        assert_eq!(Sites::load(&t.join("absent.toml")).unwrap(), Sites::default());
        std::fs::write(t.join("bad.toml"), "schema = 1\n[[site]]\nname = \n").unwrap();
        assert!(Sites::load(&t.join("bad.toml")).is_err());
        std::fs::write(t.join("v2.toml"), "schema = 2\n").unwrap();
        assert!(Sites::load(&t.join("v2.toml")).is_err());
        std::fs::write(t.join("dup.toml"), "schema = 1\n[[site]]\nname=\"a\"\ndomain=\"a.test\"\nroot=\"x\"\n[[site]]\nname=\"b\"\ndomain=\"A.test\"\nroot=\"y\"\n").unwrap();
        assert!(Sites::load(&t.join("dup.toml")).is_err(), "domaine en double");
        // guillemet puis saut de ligne dans la racine : injection de directive Apache refusée
        std::fs::write(t.join("inj.toml"), "schema = 1\n[[site]]\nname=\"a\"\ndomain=\"a.test\"\nroot='x\"\\nRequire all granted'\n").unwrap();
        assert!(Sites::load(&t.join("inj.toml")).is_err(), "guillemet dans la racine");
        std::fs::write(t.join("esc.toml"), "schema = 1\n[[site]]\nname=\"a\"\ndomain=\"a.test\"\nroot=\"../evil\"\n").unwrap();
        assert!(Sites::load(&t.join("esc.toml")).is_err(), "racine hors du dossier");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn vhost_parsing_recognises_the_auto_format_only() {
        let http = crate::vhosts::render("demo.test", Path::new("C:/x/www/demo/public"), None, 80, 443);
        assert_eq!(parse_vhost(&http), Some(("demo.test".into(), "C:/x/www/demo/public".into(), false)));
        let https = crate::vhosts::render("demo.test", Path::new("C:/x/www/demo/public"), Some((Path::new("a.crt"), Path::new("a.key"))), 80, 443);
        assert_eq!(parse_vhost(&https).map(|v| v.2), Some(true));
        assert_eq!(parse_vhost("<VirtualHost *:80>\nServerName custom.test\n</VirtualHost>\n"), None, "vhost personnalisé ignoré");
    }

    #[test]
    fn migration_imports_auto_vhosts_and_leaves_files_untouched() {
        let t = tmp("mig");
        let paths = paths_in(&t, true);
        let sites_dir = paths.sites_enabled();
        std::fs::create_dir_all(&sites_dir).unwrap();
        let pub_a = paths.www.join("alpha").join("public");
        let pub_b = paths.www.join("beta").join("public");
        let a = crate::vhosts::render("alpha.test", &pub_a, None, 8080, 8443);
        let b = crate::vhosts::render("beta.test", &pub_b, Some((Path::new("b.crt"), Path::new("b.key"))), 8080, 8443);
        std::fs::write(sites_dir.join("auto.alpha.test.conf"), &a).unwrap();
        std::fs::write(sites_dir.join("auto.beta.test.conf"), &b).unwrap();
        std::fs::write(sites_dir.join("auto.perso.test.conf"), "<VirtualHost *:80>\nServerName perso.test\n</VirtualHost>\n").unwrap();
        std::fs::write(sites_dir.join("autre.conf"), a.clone()).unwrap();
        let s = Sites::load_or_migrate(&paths, "test").unwrap();
        assert_eq!(s.sites.len(), 2);
        let alpha = s.get("alpha.test").unwrap();
        assert_eq!((alpha.name.as_str(), alpha.root.as_str(), alpha.ssl), ("alpha", "www/alpha/public", false));
        assert!(s.get("beta.test").unwrap().ssl);
        assert!(path(&paths).is_file(), "sites.toml créé");
        // aucun vhost modifié
        assert_eq!(std::fs::read_to_string(sites_dir.join("auto.alpha.test.conf")).unwrap(), a);
        assert_eq!(std::fs::read_to_string(sites_dir.join("auto.beta.test.conf")).unwrap(), b);
        // 2e chargement : lit le fichier, ne ré-importe pas
        std::fs::write(sites_dir.join("auto.gamma.test.conf"), crate::vhosts::render("gamma.test", &paths.www.join("gamma/public"), None, 8080, 8443)).unwrap();
        assert_eq!(Sites::load_or_migrate(&paths, "test").unwrap().sites.len(), 2);
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn adopt_mode_never_imports_laragon_vhosts() {
        let t = tmp("adopt");
        let paths = paths_in(&t, false);
        let dir = paths.sites_enabled();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auto.laragon.local.conf"), crate::vhosts::render("laragon.local", Path::new("D:/laragon/www/x/public"), None, 80, 443)).unwrap();
        let s = Sites::load_or_migrate(&paths, "local").unwrap();
        assert!(s.sites.is_empty() && !path(&paths).exists());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn root_is_relative_to_the_data_root_when_possible() {
        let t = tmp("root");
        let paths = paths_in(&t, true);
        let r = root_for(&paths, &paths.www.join("x").join("public"));
        assert_eq!(r, "www/x/public");
        let s = SiteConfig { root: r, ..site("x") };
        assert_eq!(root_abs(&paths, &s), paths.root.join("www/x/public"));
        assert_eq!(root_for(&paths, Path::new("E:/ailleurs/y/public")), "E:/ailleurs/y/public");
        let ext = SiteConfig { root: "E:/ailleurs/y/public".into(), ..site("y") };
        assert_eq!(root_abs(&paths, &ext), PathBuf::from("E:/ailleurs/y/public"));
        std::fs::remove_dir_all(&t).unwrap();
    }
}
