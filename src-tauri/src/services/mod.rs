//! Définition (data-driven) des services gérés — ADR-007. Le superviseur les exécute.
use crate::config::{Config, RedisCfg};
use crate::paths::{slash, AppPaths};
use std::path::{Path, PathBuf};

/// Ordre de démarrage ; l'arrêt se fait dans l'ordre inverse (F-007).
pub const START_ORDER: [&str; 4] = ["mysql", "redis", "apache", "mailpit"];

pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Fichiers générés avant le lancement (ex. redis.conf).
    pub write_files: Vec<(PathBuf, String)>,
    /// Étape unique avant le premier lancement (ex. initialisation du datadir MySQL).
    pub init: Option<InitStep>,
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
    pub launch: Result<Launch, String>,
    pub stop: Stop,
}

fn exe(dir: Option<PathBuf>, rel: &str, what: &str) -> Result<(PathBuf, PathBuf), String> {
    let dir = dir.ok_or_else(|| format!("{what} : aucun dossier trouvé sous bin/"))?;
    let program = dir.join(rel);
    if program.is_file() {
        Ok((dir, program))
    } else {
        Err(format!("{} introuvable", program.display()))
    }
}

pub fn redis_conf(c: &RedisCfg, data_dir: &Path, password: Option<&str>) -> String {
    let save = if c.persist { "save 900 1\nsave 300 10\nsave 60 10000" } else { "save \"\"" };
    let auth = password.map(|p| format!("requirepass {p}\n")).unwrap_or_default();
    format!(
        "bind {}\nport {}\ndir {}\nmaxmemory {}\nmaxmemory-policy {}\nappendonly no\n{}{}\n",
        c.bind, c.port, slash(data_dir), c.maxmemory, c.policy, auth, save
    )
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
    // Apache
    let apache = if cfg.web.server == "apache" {
        exe(paths.latest_in("apache"), "bin/httpd.exe", "Apache").map(|(dir, program)| {
            if paths.standalone {
                let conf = paths.httpd_conf();
                let text = crate::provision::httpd_conf_text(cfg, paths, &dir, paths.active_php().as_deref());
                Launch {
                    args: vec!["-d".into(), slash(&dir), "-f".into(), slash(&conf)],
                    cwd: dir,
                    program,
                    write_files: vec![(conf, text), (paths.data.join("run").join(".keep"), String::new())],
                    init: None,
                }
            } else {
                Launch { args: vec!["-d".into(), slash(&dir)], cwd: dir, program, write_files: vec![], init: None }
            }
        })
    } else {
        Err("Nginx : pas encore pris en charge (prévu après la première version)".into())
    };
    let web_name = if cfg.web.server == "nginx" { "Nginx" } else { "Apache" };
    let web_dir_name = paths.latest_in(&cfg.web.server).and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    // PHP (runtime)
    let php_dir = paths.active_php();
    let php_meta = php_dir
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().replace("php-", ""))
        .unwrap_or_else(|| "introuvable".into());
    let php = match &php_dir {
        Some(d) if d.join("php.exe").is_file() => Ok(Launch {
            program: d.join("php.exe"), args: vec![], cwd: d.clone(), write_files: vec![], init: None,
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
            }
        } else {
            Launch { args: vec!["--console".into()], cwd: dir, program, write_files: vec![], init: None }
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
    let redis = exe(paths.latest_in("redis"), "redis-server.exe", "Redis").map(|(dir, program)| Launch {
        args: vec![slash(&conf_path)],
        cwd: dir,
        program,
        write_files: vec![(conf_path.clone(), redis_conf(&cfg.redis, &redis_dir, redis_pw.as_deref()))],
        init: None,
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
        init: None,
    });

    vec![
        ServiceSpec {
            id: "apache", name: web_name, port: Some(cfg.web.port),
            meta: format!("port {} · {}", cfg.web.port, web_dir_name.unwrap_or_default()),
            enabled: true, start_timeout_secs: 15, runtime: false, launch: apache, stop: Stop::Kill,
        },
        ServiceSpec {
            id: "php", name: "PHP", port: None, meta: php_meta,
            enabled: true, start_timeout_secs: 0, runtime: true, launch: php, stop: Stop::Kill,
        },
        ServiceSpec {
            id: "mysql", name: "MySQL", port: Some(cfg.mysql.port),
            meta: format!("port {}", cfg.mysql.port),
            enabled: true, start_timeout_secs: 60, runtime: false, launch: mysql, stop: mysql_stop,
        },
        ServiceSpec {
            id: "redis", name: "Redis", port: Some(cfg.redis.port),
            meta: format!("{}:{}", cfg.redis.bind, cfg.redis.port),
            enabled: cfg.redis.enabled, start_timeout_secs: 10, runtime: false, launch: redis,
            stop: Stop::Redis { port: cfg.redis.port, save: cfg.redis.persist, password: redis_pw },
        },
        ServiceSpec {
            id: "mailpit", name: "Mailpit", port: Some(cfg.mailpit.http),
            meta: format!("SMTP {} · UI {}", cfg.mailpit.smtp, cfg.mailpit.http),
            enabled: true, start_timeout_secs: 10, runtime: false, launch: mailpit, stop: Stop::Kill,
        },
    ]
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
        assert_eq!(ids, ["apache", "php", "mysql", "redis", "mailpit"]);
    }
}