//! Mode autonome (ADR-011) : import des binaires depuis Laragon et génération des configurations.
//! astral-dev ne lit plus rien dans Laragon une fois l'import terminé.
use crate::config::Config;
use crate::error::AppError;
use crate::paths::{latest_dir, slash, AppPaths};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Versions PHP importées par défaut (décision propriétaire, 2026-11-15).
pub const PHP_VERSIONS: [&str; 3] = ["8.3", "8.4", "8.5"];

/// Modules Apache chargés (ceux du httpd.conf de Laragon, strict nécessaire pour des sites PHP).
const MODULES: [&str; 22] = [
    "access_compat", "auth_basic", "authn_core", "authn_file", "authz_core", "authz_groupfile",
    "authz_host", "authz_user", "socache_shmcb", "filter", "reqtimeout", "autoindex", "mime",
    "log_config", "alias", "dir", "rewrite", "env", "headers", "setenvif", "version", "ssl",
];

#[derive(Serialize, Clone)]
pub struct SetupStatus {
    pub standalone: bool,
    pub ready: bool,
    pub missing: Vec<String>,
    /// Racine Laragon détectée (source possible de l'import).
    pub laragon: Option<String>,
}

fn has(p: &Path) -> bool {
    p.exists()
}

/// Contrôle des binaires nécessaires. En mode adopt : toujours `ready` (Laragon fait foi).
pub fn status(cfg: &Config, paths: &AppPaths) -> SetupStatus {
    let mut missing = vec![];
    if paths.standalone {
        let need = |name: &str, rel: &str, missing: &mut Vec<String>| {
            if !paths.latest_in(name).map(|d| d.join(rel)).map_or(false, |p| has(&p)) {
                missing.push(name.to_string());
            }
        };
        need("apache", "bin/httpd.exe", &mut missing);
        need("mysql", "bin/mysqld.exe", &mut missing);
        need("redis", "redis-server.exe", &mut missing);
        need("mailpit", "mailpit.exe", &mut missing);
        if paths.php_exe().is_none() {
            missing.push(format!("php {}", cfg.php.version));
        }
        if !has(&paths.composer_phar()) {
            missing.push("composer".into());
        }
    }
    SetupStatus {
        standalone: paths.standalone,
        ready: missing.is_empty(),
        missing,
        laragon: find_laragon().map(|p| p.display().to_string()),
    }
}

/// Cherche Laragon : D:/laragon puis C:/laragon (présence de bin/apache).
pub fn find_laragon() -> Option<PathBuf> {
    ["D:/laragon", "C:/laragon"].iter().map(PathBuf::from).find(|p| p.join("bin").join("apache").is_dir())
}

// ---------- copie ----------

fn copy_tree(src: &Path, dst: &Path, skip: &[&str], n: &mut u64, progress: &dyn Fn(String)) -> Result<(), AppError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let sp = entry.path();
        let dp = dst.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if skip.iter().any(|s| name.to_string_lossy().eq_ignore_ascii_case(s)) {
                continue;
            }
            copy_tree(&sp, &dp, skip, n, progress)?;
        } else if ft.is_file() {
            let len = entry.metadata()?.len();
            // reprise possible : fichier déjà copié (même taille) ignoré.
            if fs::metadata(&dp).map_or(false, |m| m.len() == len) {
                continue;
            }
            fs::copy(&sp, &dp)?;
            *n += 1;
            if *n % 1000 == 0 {
                progress(format!("… {n} fichiers copiés"));
            }
        }
    }
    Ok(())
}

fn copy_component(
    src_parent: &Path,
    dst_parent: &Path,
    label: &str,
    skip: &[&str],
    progress: &dyn Fn(String),
) -> Result<String, AppError> {
    let dir = latest_dir(src_parent).ok_or_else(|| AppError::BinaryNotFound(format!("{label} introuvable dans {}", src_parent.display())))?;
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    progress(format!("Copie de {label} ({name})…"));
    let mut n = 0;
    copy_tree(&dir, &dst_parent.join(&name), skip, &mut n, progress)?;
    Ok(name)
}

/// Dernier dossier php-<version>.* sous src/bin/php.
fn php_for(src_php: &Path, version: &str) -> Option<PathBuf> {
    let prefix = format!("php-{version}.");
    let mut v: Vec<PathBuf> = fs::read_dir(src_php)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().map_or(false, |n| n.to_string_lossy().starts_with(&prefix)))
        .collect();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    v.pop()
}

/// Copie les binaires de Laragon vers la racine astral-dev. Reprenable. Retourne la liste copiée.
pub fn import_from_laragon(src: &Path, paths: &AppPaths, php_versions: &[&str], progress: &dyn Fn(String)) -> Result<Vec<String>, AppError> {
    let sb = src.join("bin");
    if !sb.join("apache").is_dir() {
        return Err(AppError::BinaryNotFound(format!("{} ne ressemble pas à Laragon (bin/apache absent)", src.display())));
    }
    let db = paths.bin();
    let mut done = vec![];
    done.push(format!("apache {}", copy_component(&sb.join("apache"), &db.join("apache"), "Apache", &["manual"], progress)?));
    done.push(format!("mysql {}", copy_component(&sb.join("mysql"), &db.join("mysql"), "MySQL", &["docs", "include"], progress)?));
    done.push(format!("redis {}", copy_component(&sb.join("redis"), &db.join("redis"), "Redis", &[], progress)?));
    done.push(format!("mailpit {}", copy_component(&sb.join("mailpit"), &db.join("mailpit"), "Mailpit", &[], progress)?));
    for v in php_versions {
        match php_for(&sb.join("php"), v) {
            Some(dir) => {
                let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                progress(format!("Copie de PHP {v} ({name})…"));
                let mut n = 0;
                copy_tree(&dir, &db.join("php").join(&name), &["dev"], &mut n, progress)?;
                done.push(format!("php {name}"));
            }
            None => progress(format!("PHP {v} absent de Laragon : ignoré")),
        }
    }
    let composer = sb.join("composer").join("composer.phar");
    if composer.is_file() {
        let dst = paths.composer_phar();
        fs::create_dir_all(dst.parent().unwrap_or(&db))?;
        fs::copy(&composer, &dst)?;
        done.push("composer.phar".into());
    }
    let cacert = src.join("etc").join("ssl").join("cacert.pem");
    if cacert.is_file() {
        fs::create_dir_all(paths.ssl_dir())?;
        fs::copy(&cacert, paths.ssl_dir().join("cacert.pem"))?;
        done.push("cacert.pem".into());
    }
    let fcgid = src.join("etc").join("apache2").join("modules").join("mod_fcgid-2.3.10-win64-VS18.so");
    if fcgid.is_file() {
        let dst = paths.etc().join("apache2").join("modules");
        fs::create_dir_all(&dst)?;
        fs::copy(&fcgid, dst.join("mod_fcgid.so"))?;
        done.push("mod_fcgid".into());
    }
    for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
        fs::create_dir_all(d)?;
    }
    for dir in paths.php_dirs() {
        write_php_ini(&dir, &paths.ssl_dir().join("cacert.pem"))?;
    }
    progress("Import terminé.".into());
    Ok(done)
}

// ---------- générateurs ----------

/// Extensions activées si le fichier `ext/php_<nom>.dll` existe.
const PHP_EXT: [&str; 17] = [
    "curl", "fileinfo", "gd", "intl", "mbstring", "exif", "mysqli", "openssl", "pdo_mysql", "pdo_sqlite",
    "sodium", "sqlite3", "zip", "redis", "bcmath", "xsl", "ftp",
];

pub fn php_ini_text(php_dir: &Path, cacert: &Path) -> String {
    let ext_dir = php_dir.join("ext");
    let mut s = String::from("; Généré par astral-dev (mode autonome) — régénéré à chaque démarrage.\n[PHP]\n");
    s.push_str(&format!("extension_dir = \"{}\"\n", slash(&ext_dir)));
    s.push_str("memory_limit = 512M\nmax_execution_time = 120\nupload_max_filesize = 100M\npost_max_size = 100M\n");
    s.push_str("display_errors = On\nerror_reporting = E_ALL\ndate.timezone = \"Europe/Paris\"\n");
    for e in PHP_EXT {
        if ext_dir.join(format!("php_{e}.dll")).is_file() {
            s.push_str(&format!("extension={e}\n"));
        }
    }
    s.push_str(&format!("[curl]\ncurl.cainfo = \"{}\"\n[openssl]\nopenssl.cafile = \"{}\"\n", slash(cacert), slash(cacert)));
    s
}

pub fn write_php_ini(php_dir: &Path, cacert: &Path) -> Result<(), AppError> {
    fs::write(php_dir.join("php.ini"), php_ini_text(php_dir, cacert))?;
    Ok(())
}

pub fn my_ini_text(cfg: &Config, paths: &AppPaths, mysql_dir: &Path) -> String {
    format!(
        "[client]\nport={port}\n\n[mysqld]\nbasedir=\"{base}\"\ndatadir=\"{data}\"\nport={port}\nbind-address=127.0.0.1\nmysqlx=OFF\ncharacter-set-server=utf8mb4\nmax_allowed_packet=512M\nsecure-file-priv=\"\"\nexplicit_defaults_for_timestamp=1\nskip-name-resolve\n\n[mysqldump]\nquick\nmax_allowed_packet=512M\n",
        port = cfg.mysql.port,
        base = slash(mysql_dir),
        data = slash(&paths.mysql_data()),
    )
}

pub fn httpd_conf_text(cfg: &Config, paths: &AppPaths, apache_dir: &Path, php_dir: Option<&Path>) -> String {
    let www = slash(&paths.www);
    let logs = slash(&paths.logs());
    let mut s = String::from("# Généré par astral-dev (mode autonome) — régénéré à chaque démarrage.\n");
    s.push_str(&format!("Define SRVROOT \"{}\"\nServerRoot \"{}\"\nServerName localhost\n", slash(apache_dir), slash(apache_dir)));
    s.push_str(&format!("Listen 127.0.0.1:{}\n", cfg.web.port));
    let has_certs = std::fs::read_dir(paths.ssl_dir())
        .map(|r| r.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().ends_with("-key.pem")))
        .unwrap_or(false);
    if has_certs {
        s.push_str("Listen 127.0.0.1:443\n");
    }
    for m in MODULES {
        s.push_str(&format!("LoadModule {m}_module modules/mod_{m}.so\n"));
    }
    s.push_str(&format!(
        "PidFile \"{pid}\"\nErrorLog \"{logs}/apache-error.log\"\nLogLevel warn\n<IfModule log_config_module>\n    LogFormat \"%h %l %u %t \\\"%r\\\" %>s %b\" common\n    CustomLog \"{logs}/apache-access.log\" common\n</IfModule>\n<IfModule mime_module>\n    TypesConfig conf/mime.types\n</IfModule>\nSSLRandomSeed startup builtin\nSSLRandomSeed connect builtin\nDirectoryIndex index.php index.html\n\n<Directory />\n    AllowOverride None\n    Require all denied\n</Directory>\nDocumentRoot \"{www}\"\n<Directory \"{www}\">\n    Options Indexes FollowSymLinks\n    AllowOverride All\n    Require all granted\n</Directory>\n\n",
        pid = slash(&paths.data.join("run").join("httpd.pid")),
    ));
    if let Some(php) = php_dir {
        if php.join("php8apache2_4.dll").is_file() {
            for dll in ["libcrypto-3-x64.dll", "libssl-3-x64.dll"] {
                if php.join(dll).is_file() {
                    s.push_str(&format!("LoadFile \"{}\"\n", slash(&php.join(dll))));
                }
            }
            s.push_str(&format!(
                "LoadModule php_module \"{}\"\nPHPIniDir \"{}\"\n<IfModule mime_module>\n    AddType application/x-httpd-php .php\n</IfModule>\n",
                slash(&php.join("php8apache2_4.dll")),
                slash(php)
            ));
        } else {
            // Builds NTS : FastCGI via mod_fcgid.
            let p = slash(php);
            s.push_str(&format!(
                "LoadModule fcgid_module \"{fc}\"\n<IfModule fcgid_module>\n    FcgidInitialEnv PATH \"{p};C:/Windows/system32;C:/Windows\"\n    FcgidInitialEnv SystemRoot \"C:/Windows\"\n    FcgidInitialEnv TEMP \"C:/Windows/Temp\"\n    FcgidInitialEnv TMP \"C:/Windows/Temp\"\n    FcgidInitialEnv PHPRC \"{p}\"\n    FcgidInitialEnv PHP_FCGI_MAX_REQUESTS 0\n    FcgidIOTimeout 3600\n    FcgidConnectTimeout 16\n    FcgidMaxRequestsPerProcess 0\n    FcgidMaxProcesses 50\n    FcgidMaxRequestLen 81310720\n    <FilesMatch \"\\.php$\">\n        AddHandler fcgid-script .php\n        Options +ExecCGI\n        FcgidWrapper \"{p}/php-cgi.exe\" .php\n    </FilesMatch>\n</IfModule>\n",
                fc = slash(&paths.etc().join("apache2").join("modules").join("mod_fcgid.so")),
            ));
        }
    }
    s.push_str(&format!(
        "\n# Hôte par défaut (premier VirtualHost) : tout nom inconnu → www/\n<VirtualHost *:{port}>\n    ServerName localhost\n    DocumentRoot \"{www}\"\n</VirtualHost>\n\nIncludeOptional \"{sites}/*.conf\"\n",
        port = cfg.web.port,
        sites = slash(&paths.sites_enabled()),
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_paths() -> (Config, AppPaths) {
        let c = Config::standalone("C:/astral-dev", |_| false);
        let p = AppPaths::from_config(&c);
        (c, p)
    }

    #[test]
    fn httpd_conf_mod_php_when_thread_safe_build() {
        let t = std::env::temp_dir().join(format!("astral-dev-php-{}", std::process::id()));
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("php8apache2_4.dll"), "").unwrap();
        let (c, p) = cfg_paths();
        let s = httpd_conf_text(&c, &p, Path::new("C:/astral-dev/bin/apache/a"), Some(&t));
        assert!(s.contains("LoadModule php_module") && !s.contains("fcgid_module"));
        assert!(s.contains("Listen 127.0.0.1:80\n"));
        assert!(s.contains("IncludeOptional \"C:/astral-dev/etc/apache2/sites-enabled/*.conf\""));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn httpd_conf_fcgid_for_nts_build() {
        let (c, p) = cfg_paths();
        let s = httpd_conf_text(&c, &p, Path::new("C:/a"), Some(Path::new("C:/php-8.4-nts")));
        assert!(s.contains("fcgid_module") && s.contains("FcgidWrapper \"C:/php-8.4-nts/php-cgi.exe\" .php"));
        assert!(!s.contains("php_module"));
    }

    #[test]
    fn my_ini_is_loopback_without_mysqlx() {
        let (c, p) = cfg_paths();
        let s = my_ini_text(&c, &p, Path::new("C:/m"));
        assert!(s.contains("bind-address=127.0.0.1") && s.contains("mysqlx=OFF"));
        assert!(s.contains("datadir=\"C:/astral-dev/data/mysql\""));
    }

    #[test]
    fn php_ini_lists_only_existing_extensions() {
        let t = std::env::temp_dir().join(format!("astral-dev-ini-{}", std::process::id()));
        std::fs::create_dir_all(t.join("ext")).unwrap();
        std::fs::write(t.join("ext").join("php_redis.dll"), "").unwrap();
        let s = php_ini_text(&t, Path::new("C:/ssl/cacert.pem"));
        assert!(s.contains("extension=redis\n") && !s.contains("extension=curl"));
        assert!(s.contains("curl.cainfo = \"C:/ssl/cacert.pem\""));
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn import_refuses_a_non_laragon_folder() {
        let (_, p) = cfg_paths();
        assert!(import_from_laragon(Path::new("C:/nope-astral"), &p, &["8.3"], &|_| {}).is_err());
    }

    fn http_get(port: u16, host: &str) -> String {
        use std::io::{Read, Write};
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
        let req = format!("GET /index.php HTTP/1.0\r\nHost: {host}\r\n\r\n");
        c.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out);
        out
    }

    /// Import réel depuis Laragon puis pile complète depuis la racine autonome (ports hors Laragon).
    /// PHP 8.3 (mod_php, TS) puis 8.4 (FastCGI, NTS). `cargo test standalone_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn standalone_real_import_and_stack() {
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test".into());
        let mut cfg = Config::standalone(&root, |_| false);
        cfg.web.port = 18080;
        cfg.mysql.port = 13307;
        cfg.redis.port = 16380;
        cfg.mailpit.http = 18026;
        cfg.mailpit.smtp = 11026;
        let paths = AppPaths::from_config(&cfg);
        let laragon = find_laragon().expect("Laragon");
        let done = import_from_laragon(&laragon, &paths, &["8.3", "8.4"], &|l| println!("{l}")).unwrap();
        println!("{done:?}");
        assert!(status(&cfg, &paths).ready, "{:?}", status(&cfg, &paths).missing);

        let public = paths.www.join("demo").join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::write(public.join("index.php"), "<?php echo 'ok-' . PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION . '-' . (extension_loaded('redis') ? 'redis' : 'noredis') . '-' . (extension_loaded('pdo_mysql') ? 'mysql' : 'nomysql');").unwrap();
        crate::vhosts::ensure(&paths.sites_enabled(), "demo.test", &public, &paths.ssl_dir(), None, cfg.web.port).unwrap();

        for (ver, expect) in [("8.3", "ok-8.3"), ("8.4", "ok-8.4")] {
            let mut c = cfg.clone();
            c.php.version = ver.into();
            let p = AppPaths::from_config(&c);
            for d in p.php_dirs() {
                write_php_ini(&d, &p.ssl_dir().join("cacert.pem")).unwrap();
            }
            let sup = Supervisor::new(crate::services::build(&c, &p), p.logs());
            let res = sup.start_all();
            for r in &res {
                assert!(r.ok, "PHP {ver} : {} : {:?}", r.id, r.note);
            }
            let body = http_get(c.web.port, "demo.test");
            println!("PHP {ver} -> {}", body.lines().last().unwrap_or(""));
            assert!(body.contains(expect), "PHP {ver} : {body}");
            assert!(crate::probe::redis_ping(c.redis.port));
            let stopped = sup.stop_all();
            assert!(stopped.iter().all(|r| r.ok), "{stopped:?}");
        }
    }
}
