//! Définition (data-driven) des services gérés — ADR-007. Le superviseur les exécute.
use crate::config::{Config, RedisCfg};
use crate::paths::{slash, AppPaths};
use std::path::{Path, PathBuf};

/// Ordre de démarrage ; l'arrêt se fait dans l'ordre inverse (F-007). `web_id` = identifiant du moteur web actif ;
/// les workers PHP démarrent avant lui (il les contacte dès la première requête).
pub fn start_order(web_id: &'static str, workers: &[&'static str]) -> Vec<&'static str> {
    let mut v = vec!["mysql", "redis"];
    v.extend_from_slice(workers);
    v.push(web_id);
    v.push("mailpit");
    v
}

/// Contrôle de pré-lancement : `Err(message)` empêche le démarrage et s'affiche tel quel.
pub type Preflight = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// Rôle d'un service dans la pile web (ADR-015) : le moteur web actif occupe sa place dans l'ordre de démarrage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ServiceRole {
    /// Serveur web qui écoute sur les ports publics (Apache aujourd'hui).
    Web,
    /// Serveur en façade qui relaie vers un autre moteur (Caddy devant Apache, phase 4).
    #[allow(dead_code)]
    FrontProxy,
    /// Serveur derrière un proxy (Apache sur son port interne, phase 4).
    #[allow(dead_code)]
    Backend,
    /// Worker php-cgi d'une série de PHP (ADR-017) : plusieurs par série, masqués de la grille des services.
    PhpWorker,
    None,
}

#[derive(Default)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Fichiers générés avant le lancement (ex. redis.conf).
    pub write_files: Vec<(PathBuf, String)>,
    /// Étape unique avant le premier lancement (ex. initialisation du datadir MySQL).
    pub init: Option<InitStep>,
    /// Variables d'environnement ajoutées à celles de l'application (ex. `PHPRC` pour php-cgi).
    pub env: Vec<(String, String)>,
}

/// Commande bloquante exécutée si `marker` n'existe pas encore.
pub struct InitStep {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub marker: PathBuf,
}

pub enum Stop {
    Kill,
    Redis { port: u16, save: bool, password: Option<String> },
    MysqlAdmin { exe: PathBuf, port: u16 },
}

pub struct ServiceSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub port: Option<u16>,
    pub meta: String,
    pub enabled: bool,
    /// Délai max de démarrage (MySQL : initialisation InnoDB ~10 s+).
    pub start_timeout_secs: u64,
    /// Runtime sans processus propre (PHP tourne via Apache/CLI).
    pub runtime: bool,
    /// Ports supplémentaires utilisés par le service (ex. SMTP de Mailpit) : contrôlés comme le port principal.
    pub extra_ports: Vec<(&'static str, u16)>,
    pub role: ServiceRole,
    /// Série de PHP desservie (workers uniquement) : sert à ne démarrer que les séries utilisées.
    pub pool: Option<String>,
    /// Contrôle exécuté AVANT le lancement (ex. `httpd -t`) : un échec donne la cause exacte au lieu d'un simple code de sortie.
    pub preflight: Option<Preflight>,
    pub launch: Result<Launch, String>,
    pub stop: Stop,
}

pub(crate) fn exe(dir: Option<PathBuf>, rel: &str, what: &str) -> Result<(PathBuf, PathBuf), String> {
    let dir = dir.ok_or_else(|| format!("{what} : aucun dossier trouvé sous bin/"))?;
    let program = dir.join(rel);
    if program.is_file() {
        Ok((dir, program))
    } else {
        Err(format!("{} introuvable", program.display()))
    }
}

#[cfg(test)]
pub fn redis_conf(c: &RedisCfg, data_dir: &Path, password: Option<&str>) -> String {
    redis_conf_for(c, data_dir, password, false)
}

/// Les builds MSYS2/Cygwin de Redis (redis-windows) ne comprennent pas les chemins Windows (`C:/…`) : le dossier de
/// données est alors relatif (`./`) et le processus est lancé dans ce dossier avec un fichier de configuration relatif.
pub fn redis_conf_for(c: &RedisCfg, data_dir: &Path, password: Option<&str>, relative_dir: bool) -> String {
    let save = if c.persist { "save 900 1\nsave 300 10\nsave 60 10000" } else { "save \"\"" };
    let auth = password.map(|p| format!("requirepass {p}\n")).unwrap_or_default();
    let dir = if relative_dir { "./".to_string() } else { slash(data_dir) };
    format!(
        "bind {}\nport {}\ndir {}\nmaxmemory {}\nmaxmemory-policy {}\nappendonly no\n{}{}\n",
        c.bind, c.port, dir, c.maxmemory, c.policy, auth, save
    )
}

/// Build Redis fourni avec un runtime POSIX (MSYS2 ou Cygwin).
pub fn is_posix_runtime_build(dir: &Path) -> bool {
    dir.join("msys-2.0.dll").is_file() || dir.join("cygwin1.dll").is_file()
}

/// Mot de passe Redis effectif (None si password = "none").
pub fn redis_secret(cfg: &Config, paths: &AppPaths) -> Option<String> {
    if cfg.redis.password == "generated" {
        crate::secrets::redis_password(&paths.secrets()).ok()
    } else {
        None
    }
}

pub fn build(cfg: &Config, paths: &AppPaths) -> Vec<ServiceSpec> {
    // Moteur web (Apache aujourd'hui) : voir engines/
    let web = match crate::engines::from_config(cfg) {
        Ok(engine) => engine.service_spec(&crate::engines::EngineCtx { cfg, paths }),
        Err(_) => crate::engines::unsupported_spec(cfg),
    };

    // PHP (runtime)
    let php_dir = paths.active_php();
    let php_meta = php_dir
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().replace("php-", ""))
        .unwrap_or_else(|| "introuvable".into());
    let php = match &php_dir {
        Some(d) if d.join("php.exe").is_file() => Ok(Launch {
            program: d.join("php.exe"), args: vec![], cwd: d.clone(), write_files: vec![], init: None, env: vec![],
        }),
        _ => Err("php.exe introuvable".to_string()),
    };

    // MySQL
    let mysql_dir = paths.latest_in("mysql");
    let mysql = exe(mysql_dir.clone(), "bin/mysqld.exe", "MySQL").map(|(dir, program)| {
        if paths.standalone {
            let ini = paths.mysql_ini();
            let text = crate::provision::my_ini_text(cfg, paths, &dir);
            let defaults = format!("--defaults-file={}", slash(&ini));
            Launch {
                args: vec![defaults.clone(), "--console".into()],
                cwd: dir.clone(),
                program: program.clone(),
                write_files: vec![(ini, text)],
                init: Some(InitStep {
                    program,
                    args: vec![defaults, "--initialize-insecure".into(), "--console".into()],
                    cwd: dir,
                    marker: paths.mysql_data().join("mysql"),
                }),
                env: vec![],
            }
        } else {
            Launch { args: vec!["--console".into()], cwd: dir, program, write_files: vec![], init: None, env: vec![] }
        }
    });
    let mysql_stop = match &mysql_dir {
        Some(d) => Stop::MysqlAdmin { exe: d.join("bin/mysqladmin.exe"), port: cfg.mysql.port },
        None => Stop::Kill,
    };

    // Redis
    let redis_pw = redis_secret(cfg, paths);
    let redis_dir = paths.redis_data();
    let conf_path = redis_dir.join("redis.conf");
    let redis = exe(paths.redis_dir(&cfg.redis.version), "redis-server.exe", "Redis").map(|(dir, program)| {
        let posix = is_posix_runtime_build(&dir);
        Launch {
            // build POSIX : fichier de configuration relatif, processus lancé dans le dossier de données
            args: vec![if posix { "redis.conf".to_string() } else { slash(&conf_path) }],
            cwd: if posix { redis_dir.clone() } else { dir },
            program,
            write_files: vec![(conf_path.clone(), redis_conf_for(&cfg.redis, &redis_dir, redis_pw.as_deref(), posix))],
            init: None, env: vec![],
        }
    });

    // Mailpit
    let mailpit = exe(paths.latest_in("mailpit"), "mailpit.exe", "Mailpit").map(|(dir, program)| Launch {
        args: vec![
            "--smtp".into(), format!("127.0.0.1:{}", cfg.mailpit.smtp),
            "--listen".into(), format!("127.0.0.1:{}", cfg.mailpit.http),
        ],
        cwd: dir,
        program,
        write_files: vec![],
        init: None, env: vec![],
    });

    let mut specs = vec![
        web,
        ServiceSpec {
            id: "php", name: "PHP", port: None, meta: php_meta,
            enabled: true, start_timeout_secs: 0, runtime: true, extra_ports: vec![], role: ServiceRole::None, pool: None, preflight: None, launch: php, stop: Stop::Kill,
        },
        ServiceSpec {
            id: "mysql", name: "MySQL", port: Some(cfg.mysql.port),
            meta: format!("port {}", cfg.mysql.port),
            enabled: true, start_timeout_secs: 60, runtime: false, extra_ports: vec![], role: ServiceRole::None, pool: None, preflight: None, launch: mysql, stop: mysql_stop,
        },
        ServiceSpec {
            id: "redis", name: "Redis", port: Some(cfg.redis.port),
            meta: format!("{}:{}", cfg.redis.bind, cfg.redis.port),
            enabled: cfg.redis.enabled, start_timeout_secs: 10, runtime: false, extra_ports: vec![], role: ServiceRole::None, pool: None, preflight: None, launch: redis,
            stop: Stop::Redis { port: cfg.redis.port, save: cfg.redis.persist, password: redis_pw },
        },
        ServiceSpec {
            id: "mailpit", name: "Mailpit", port: Some(cfg.mailpit.http),
            meta: format!("SMTP {} · UI {}", cfg.mailpit.smtp, cfg.mailpit.http),
            enabled: true, start_timeout_secs: 10, runtime: false, extra_ports: vec![("SMTP", cfg.mailpit.smtp)], role: ServiceRole::None, pool: None, preflight: None, launch: mailpit, stop: Stop::Kill,
        },
    ];
    // Workers php-cgi (mode autonome) : masqués dans la grille, démarrés avant le moteur web.
    specs.extend(crate::php_pool::specs(cfg, paths));
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_conf_is_loopback_without_persistence_by_default() {
        let c = redis_conf(&RedisCfg::default(), Path::new("D:\\data\\redis"), None);
        assert!(c.contains("bind 127.0.0.1\n"));
        assert!(c.contains("port 6379\n"));
        assert!(c.contains("dir D:/data/redis\n"));
        assert!(c.contains("save \"\""));
        assert!(!c.contains("requirepass"));
    }

    #[test]
    fn redis_conf_persistence_option() {
        let mut r = RedisCfg::default();
        r.persist = true;
        assert!(redis_conf(&r, Path::new("x"), None).contains("save 900 1"));
    }

    #[test]
    fn redis_conf_requirepass() {
        let c = redis_conf(&RedisCfg::default(), Path::new("x"), Some("abc123"));
        assert!(c.contains("requirepass abc123\n"));
    }

    #[test]
    fn build_lists_five_services_in_display_order() {
        let cfg = Config::default();
        let ids: Vec<_> = build(&cfg, &AppPaths::from_config(&cfg)).iter().map(|s| s.id).collect();
        assert_eq!(ids, ["apache", "php", "mysql", "redis", "mailpit"], "adopt : pas de workers php-cgi");
        assert_eq!(start_order("apache", &["php-8.3-1"]), ["mysql", "redis", "php-8.3-1", "apache", "mailpit"]);
    }
}