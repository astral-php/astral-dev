//! Lecture de astral-dev.toml (schéma : Dossier-technique/schema/config.schema.md).
use crate::error::AppError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "astral-dev.toml";
/// Racine par défaut du mode autonome (ADR-011).
pub const HOME_DIR: &str = "C:/astral-dev";

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
    pub php: PhpCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PhpCfg {
    /// Version majeure.mineure utilisée par Apache et la CLI (mode standalone), ex. "8.3".
    pub version: String,
    /// Processus php-cgi par série de PHP (1 à 8) : php-cgi ne traite qu'une requête à la fois.
    pub workers: u32,
}

impl Default for PhpCfg {
    fn default() -> Self {
        Self { version: "8.3".into(), workers: 2 }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UiCfg {
    /// Fermer la fenêtre = réduire dans la zone de notification (F-062).
    pub close_to_tray: bool,
    pub start_minimized: bool,
    /// Démarrer les services au lancement d'astral-dev.
    pub autostart_services: bool,
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
    /// Port HTTPS ; 0 = automatique (443 si le port web est 80, sinon 8443 : évite le conflit avec Laragon).
    pub ssl_port: u16,
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
    /// Build de Redis à lancer parmi ceux installés : vide = le plus récent ; `5` ou `8` = cette version majeure.
    pub version: String,
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
            php: PhpCfg::default(),
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
        Self { server: "apache".into(), port: 80, tld: "local".into(), ssl_port: 0 }
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
            version: String::new(),
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

    /// Cherche astral-dev.toml : $ASTRAL_DEV_HOME, dossier courant et parents, puis C:/astral-dev.
    /// Absent → première installation : configuration autonome (ADR-011) créée sur C:/astral-dev.
    pub fn load() -> Result<Config, AppError> {
        match Config::find_file() {
            Some(p) => Config::load_from(&p),
            None => {
                let cfg = Config::standalone(HOME_DIR, crate::provision::find_laragon().is_some(), |p| crate::probe::port_open(p));
                let file = Path::new(HOME_DIR).join(FILE_NAME);
                if let Err(e) = std::fs::create_dir_all(HOME_DIR).and_then(|_| std::fs::write(&file, cfg.to_toml())) {
                    eprintln!("astral-dev : configuration non écrite ({e})");
                }
                Ok(cfg)
            }
        }
    }

    /// Configuration autonome : tout sous `root`, domaines .test. `busy` choisit des ports de repli
    /// quand le port standard est déjà pris (ex. Laragon actif).
    /// `laragon` : Laragon est installé sur la machine → ports de repli d'office, qu'il tourne ou non
    /// (sinon les ports figés au premier lancement entrent en conflit dès que Laragon démarre).
    pub fn standalone(root: &str, laragon: bool, busy: impl Fn(u16) -> bool) -> Config {
        let pick = |std: u16, alt: u16| if laragon || busy(std) { alt } else { std };
        let mut c = Config::default();
        c.mode = "standalone".into();
        c.paths = PathsCfg { root: root.into(), www: format!("{root}/www"), data: format!("{root}/data") };
        c.web = WebCfg { server: "apache".into(), port: pick(80, 8080), tld: "test".into(), ssl_port: 0 };
        c.mysql.port = pick(3306, 3307);
        c.redis.port = pick(6379, 6380);
        c.mailpit.http = pick(8025, 8026);
        c.mailpit.smtp = pick(1025, 1026);
        c.redis.password = "generated".into();
        c
    }

    /// Modifie des clés existantes (ou les ajoute à leur section) sans toucher au reste du fichier.
    /// `edits` : (section, clé, valeur déjà formatée TOML). Tout le reste (commentaires, autres clés) est conservé.
    pub fn patch_toml(text: &str, edits: &[(&str, &str, String)]) -> String {
        let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut lines: Vec<String> = text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        if lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }
        for (section, key, value) in edits {
            let header = format!("[{section}]");
            let start = match lines.iter().position(|l| l.trim() == header) {
                Some(i) => i,
                None => {
                    lines.push(String::new());
                    lines.push(header.clone());
                    lines.len() - 1
                }
            };
            let end = lines[start + 1..].iter().position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |p| start + 1 + p);
            let found = (start + 1..end).find(|&i| {
                let t = lines[i].trim_start();
                t.strip_prefix(*key).map_or(false, |r| r.trim_start().starts_with('='))
            });
            let new_line = format!("{key} = {value}");
            match found {
                Some(i) => lines[i] = new_line,
                None => lines.insert(start + 1, new_line),
            }
        }
        let mut out = lines.join(nl);
        out.push_str(nl);
        out
    }

    /// Port HTTPS effectif (voir `WebCfg::ssl_port`).
    pub fn ssl_port(&self) -> u16 {
        match self.web.ssl_port {
            0 if self.web.port == 80 => 443,
            0 => 8443,
            p => p,
        }
    }

    pub fn to_toml(&self) -> String {
        format!(
            "# astral-dev — configuration (mode autonome, ADR-011)\n# Aucun secret ici : le mot de passe Redis est généré dans data/secrets.\nmode = \"{mode}\"\n\n[paths]\nroot = \"{root}\"\nwww  = \"{www}\"\ndata = \"{data}\"\n\n[web]\nserver = \"{srv}\"\nport = {wp}\ntld = \"{tld}\"\n\n[php]\nversion = \"{php}\"\n\n[mysql]\nport = {my}\n\n[mailpit]\nsmtp = {ms}\nhttp = {mh}\n\n[redis]\nenabled = {re}\nbind = \"{rb}\"\nport = {rp}\nmaxmemory = \"{rm}\"\npolicy = \"{pol}\"\npersist = {per}\npassword = \"{pw}\"\n\n[ui]\nclose_to_tray = {ct}\nstart_minimized = {sm}\nautostart_services = {au}\n",
            mode = self.mode, root = self.paths.root, www = self.paths.www, data = self.paths.data,
            srv = self.web.server, wp = self.web.port, tld = self.web.tld, php = self.php.version,
            my = self.mysql.port, ms = self.mailpit.smtp, mh = self.mailpit.http,
            re = self.redis.enabled, rb = self.redis.bind, rp = self.redis.port, rm = self.redis.maxmemory,
            pol = self.redis.policy, per = self.redis.persist, pw = self.redis.password,
            ct = self.ui.close_to_tray, sm = self.ui.start_minimized, au = self.ui.autostart_services,
        )
    }

    pub fn is_standalone(&self) -> bool {
        self.mode == "standalone"
    }

    pub fn find_file() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("ASTRAL_DEV_HOME") {
            let p = Path::new(&home).join(FILE_NAME);
            if p.is_file() {
                return Some(p);
            }
        }
        if let Ok(start) = std::env::current_dir() {
            if let Some(p) = start.ancestors().map(|d| d.join(FILE_NAME)).find(|p| p.is_file()) {
                return Some(p);
            }
        }
        Some(Path::new(HOME_DIR).join(FILE_NAME)).filter(|p| p.is_file())
    }

    pub fn validate(&self) -> Result<(), AppError> {
        let bad = |m: String| Err(AppError::Config(m));
        if !["adopt", "standalone"].contains(&self.mode.as_str()) {
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
        if !(1..=8).contains(&self.php.workers) {
            return bad(format!("php.workers invalide : {} (1 à 8)", self.php.workers));
        }
        if self.php.version.is_empty() || !self.php.version.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return bad(format!("php.version invalide : {}", self.php.version));
        }
        if !self.redis.version.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return bad(format!("redis.version invalide : {}", self.redis.version));
        }
        if self.web.tld.is_empty() || !self.web.tld.chars().all(|c| c.is_ascii_lowercase()) {
            return bad(format!("web.tld invalide : {}", self.web.tld));
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
    fn standalone_defaults_and_roundtrip() {
        let c = Config::standalone("C:/astral-dev", false, |_| false);
        assert!(c.is_standalone() && c.web.port == 80 && c.web.tld == "test");
        let c2 = Config::parse(&c.to_toml()).unwrap();
        assert_eq!(c2.paths.www, "C:/astral-dev/www");
        assert_eq!(c2.redis.password, "generated");
        assert_eq!(c2.php.version, "8.3");
    }

    #[test]
    fn patch_toml_keeps_everything_else() {
        let t = "# commentaire\nmode = \"standalone\"\n\n[web]\nport = 80\ntld = \"test\"\n\n[php]\nversion = \"8.3\"\n\n[ssl]\nmkcert = \"D:/x/mkcert.exe\"\n";
        let out = Config::patch_toml(t, &[("web", "port", "8080".into()), ("php", "version", "\"8.4\"".into()), ("ui", "close_to_tray", "true".into())]);
        assert!(out.contains("port = 8080\n") && out.contains("version = \"8.4\"") && out.contains("[ui]\nclose_to_tray = true"));
        assert!(out.contains("# commentaire") && out.contains("mkcert = \"D:/x/mkcert.exe\"") && out.contains("tld = \"test\""));
        assert!(Config::parse(&out).is_ok());
        let crlf = Config::patch_toml(&t.replace('\n', "\r\n"), &[("web", "port", "81".into())]);
        assert!(crlf.contains("port = 81\r\n") && !crlf.contains("\n\n") || crlf.contains("\r\n\r\n"));
    }

    #[test]
    fn new_configurations_use_dot_test_and_the_example_file_agrees() {
        // Une configuration existante sans clé `tld` garde `local` (aucun domaine ne change) ; les nouvelles utilisent `.test`.
        assert_eq!(Config::parse("mode = \"adopt\"").unwrap().web.tld, "local");
        assert_eq!(Config::standalone("C:/astral-dev", false, |_| false).web.tld, "test");
        let example = include_str!("../../astral-dev.example.toml");
        assert_eq!(Config::parse(example).unwrap().web.tld, "test");
    }

    #[test]
    fn ssl_port_is_automatic_unless_set() {
        let mut c = Config::standalone("C:/astral-dev", false, |_| false);
        assert_eq!(c.ssl_port(), 443);
        c.web.port = 8080;
        assert_eq!(c.ssl_port(), 8443);
        c.web.ssl_port = 9443;
        assert_eq!(c.ssl_port(), 9443);
    }

    #[test]
    fn standalone_uses_alternate_ports_when_laragon_is_installed() {
        let c = Config::standalone("C:/astral-dev", true, |_| false);
        assert_eq!((c.web.port, c.mysql.port, c.redis.port), (8080, 3307, 6380));
    }

    #[test]
    fn standalone_falls_back_to_alternate_ports_when_busy() {
        let c = Config::standalone("C:/astral-dev", false, |p| p == 80 || p == 3306 || p == 6379);
        assert_eq!((c.web.port, c.mysql.port, c.redis.port, c.mailpit.http), (8080, 3307, 6380, 8025));
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