//! Lecture de astral-dev.toml (schéma : Dossier-technique/schema/config.schema.md).
use crate::error::AppError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "astral-dev.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: String,
    pub paths: PathsCfg,
    pub web: WebCfg,
    pub mysql: MysqlCfg,
    pub mailpit: MailpitCfg,
    pub redis: RedisCfg,
    pub ui: UiCfg,
    pub ssl: SslCfg,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UiCfg {
    /// Fermer la fenêtre = réduire dans la zone de notification (F-062).
    pub close_to_tray: bool,
    pub start_minimized: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SslCfg {
    /// Chemin de mkcert.exe (vide = détection automatique, F-015).
    pub mkcert: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PathsCfg {
    pub root: String,
    pub www: String,
    pub data: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WebCfg {
    pub server: String,
    pub port: u16,
    pub tld: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MysqlCfg {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MailpitCfg {
    pub smtp: u16,
    pub http: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RedisCfg {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub maxmemory: String,
    pub policy: String,
    pub persist: bool,
    /// "none" | "generated" (mot de passe aléatoire stocké dans data/secrets, F-050).
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "adopt".into(),
            paths: PathsCfg::default(),
            web: WebCfg::default(),
            mysql: MysqlCfg::default(),
            mailpit: MailpitCfg::default(),
            redis: RedisCfg::default(),
            ui: UiCfg::default(),
            ssl: SslCfg::default(),
        }
    }
}
impl Default for PathsCfg {
    fn default() -> Self {
        Self {
            root: "D:/laragon".into(),
            www: "D:/laragon/www".into(),
            data: "D:/laragon/astral-dev-data".into(),
        }
    }
}
impl Default for WebCfg {
    fn default() -> Self {
        Self { server: "apache".into(), port: 80, tld: "local".into() }
    }
}
impl Default for MysqlCfg {
    fn default() -> Self {
        Self { port: 3306 }
    }
}
impl Default for MailpitCfg {
    fn default() -> Self {
        Self { smtp: 1025, http: 8025 }
    }
}
impl Default for RedisCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            bind: "127.0.0.1".into(),
            port: 6379,
            maxmemory: "256mb".into(),
            policy: "allkeys-lru".into(),
            persist: false,
            password: "none".into(),
        }
    }
}

const POLICIES: [&str; 8] = [
    "noeviction", "allkeys-lru", "allkeys-lfu", "allkeys-random",
    "volatile-lru", "volatile-lfu", "volatile-random", "volatile-ttl",
];

impl Config {
    pub fn parse(text: &str) -> Result<Config, AppError> {
        let cfg: Config = toml::from_str(text).map_err(|e| AppError::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load_from(path: &Path) -> Result<Config, AppError> {
        Config::parse(&std::fs::read_to_string(path)?)
    }

    /// Cherche astral-dev.toml : $ASTRAL_DEV_HOME, puis le dossier courant et ses parents.
    /// Absent → valeurs par défaut (mode adopt, Laragon sous D:/laragon).
    pub fn load() -> Result<Config, AppError> {
        match Config::find_file() {
            Some(p) => Config::load_from(&p),
            None => Ok(Config::default()),
        }
    }

    pub fn find_file() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("ASTRAL_DEV_HOME") {
            let p = Path::new(&home).join(FILE_NAME);
            if p.is_file() {
                return Some(p);
            }
        }
        let start = std::env::current_dir().ok()?;
        start.ancestors().map(|d| d.join(FILE_NAME)).find(|p| p.is_file())
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let bad = |m: String| Err(AppError::Config(m));
        if self.mode != "adopt" && self.mode != "portable" {
            return bad(format!("mode inconnu : {}", self.mode));
        }
        if self.web.server != "apache" && self.web.server != "nginx" {
            return bad(format!("web.server inconnu : {}", self.web.server));
        }
        for (name, port) in [
            ("web.port", self.web.port),
            ("mysql.port", self.mysql.port),
            ("mailpit.smtp", self.mailpit.smtp),
            ("mailpit.http", self.mailpit.http),
            ("redis.port", self.redis.port),
        ] {
            if port == 0 {
                return bad(format!("{name} doit être > 0"));
            }
        }
        // NF-002 : aucun service n'écoute hors boucle locale.
        if !["127.0.0.1", "::1", "localhost"].contains(&self.redis.bind.as_str()) {
            return bad("redis.bind doit rester sur la boucle locale".into());
        }
        // Les valeurs sont écrites dans redis.conf : liste blanche contre l'injection.
        if self.redis.maxmemory.is_empty()
            || !self.redis.maxmemory.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return bad(format!("redis.maxmemory invalide : {}", self.redis.maxmemory));
        }
        if !POLICIES.contains(&self.redis.policy.as_str()) {
            return bad(format!("redis.policy inconnue : {}", self.redis.policy));
        }
        if self.redis.password != "none" && self.redis.password != "generated" {
            return bad("redis.password : \"none\" ou \"generated\" attendu (jamais de secret dans ce fichier)".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let c = Config::parse("[redis]\nport = 6380\n").unwrap();
        assert_eq!(c.redis.port, 6380);
        assert_eq!(c.web.port, 80);
        assert_eq!(c.mode, "adopt");
    }

    #[test]
    fn rejects_non_loopback_bind() {
        assert!(Config::parse("[redis]\nbind = \"0.0.0.0\"\n").is_err());
    }

    #[test]
    fn ui_and_ssl_sections() {
        let c = Config::parse("[ui]\nclose_to_tray = true\n[ssl]\nmkcert = \"C:/tools/mkcert.exe\"\n").unwrap();
        assert!(c.ui.close_to_tray && !c.ui.start_minimized);
        assert_eq!(c.ssl.mkcert, "C:/tools/mkcert.exe");
    }

    #[test]
    fn password_modes() {
        assert!(Config::parse("[redis]\npassword = \"generated\"\n").is_ok());
        assert!(Config::parse("[redis]\npassword = \"hunter2\"\n").is_err());
    }

    #[test]
    fn rejects_injection_in_redis_values() {
        assert!(Config::parse("[redis]\nmaxmemory = \"1mb\\nflushall\"\n").is_err());
        assert!(Config::parse("[redis]\npolicy = \"x\"\n").is_err());
    }

    #[test]
    fn rejects_zero_port_and_unknown_keys_types() {
        assert!(Config::parse("[mysql]\nport = 0\n").is_err());
        assert!(Config::parse("mode = 3\n").is_err());
    }
}