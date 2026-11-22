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
const MODULES: [&str; 27] = [
    "access_compat", "auth_basic", "authn_core", "authn_file", "authz_core", "authz_groupfile",
    "authz_host", "authz_user", "socache_shmcb", "filter", "reqtimeout", "autoindex", "mime",
    "log_config", "alias", "dir", "rewrite", "env", "headers", "setenvif", "version", "ssl",
    // PHP en FastCGI (php-cgi) répartie sur plusieurs workers
    "proxy", "proxy_fcgi", "proxy_balancer", "slotmem_shm", "lbmethod_bybusyness",
];

#[derive(Serialize, Clone)]
pub struct SetupStatus {
    pub standalone: bool,
    pub ready: bool,
    pub missing: Vec<String>,
    /// Nombre de composants contrôlés (missing < total = installation partielle).
    pub total: usize,
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
        // la topologie décide du serveur web nécessaire (Apache seul, Caddy seul, ou les deux)
        let mode = cfg.web_mode();
        if mode != crate::config::WebMode::Caddy {
            need("apache", "bin/httpd.exe", &mut missing);
        }
        if mode != crate::config::WebMode::Apache {
            need("caddy", "caddy.exe", &mut missing);
        }
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
        total: 6,
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

pub fn httpd_conf_text(cfg: &Config, paths: &AppPaths, apache_dir: &Path, pools: &[crate::php_pool::Pool]) -> String {
    let www = slash(&paths.www);
    let logs = slash(&paths.logs());
    let mut s = String::from("# Généré par astral-dev (mode autonome) — régénéré à chaque démarrage.\n");
    s.push_str(&format!("Define SRVROOT \"{}\"\nServerRoot \"{}\"\nServerName localhost\n", slash(apache_dir), slash(apache_dir)));
    let http_port = cfg.apache_http_port();
    let behind_caddy = cfg.web_mode() == crate::config::WebMode::CaddyApache;
    s.push_str(&format!("Listen 127.0.0.1:{http_port}\n"));
    let has_certs = std::fs::read_dir(paths.ssl_dir())
        .map(|r| r.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().ends_with("-key.pem")))
        .unwrap_or(false);
    // Derrière Caddy, c'est lui qui termine le TLS : Apache n'écoute qu'en HTTP.
    if has_certs && !behind_caddy {
        s.push_str(&format!("Listen 127.0.0.1:{}\n", cfg.ssl_port()));
    }
    for m in MODULES {
        s.push_str(&format!("LoadModule {m}_module modules/mod_{m}.so\n"));
    }
    s.push_str(&format!(
        "PidFile \"{pid}\"\nErrorLog \"{logs}/apache-error.log\"\nLogLevel warn\n<IfModule log_config_module>\n    LogFormat \"%h %l %u %t \\\"%r\\\" %>s %b\" common\n    CustomLog \"{logs}/apache-access.log\" common\n</IfModule>\n<IfModule mime_module>\n    TypesConfig conf/mime.types\n</IfModule>\nSSLRandomSeed startup builtin\nSSLRandomSeed connect builtin\nDirectoryIndex index.php index.html\n\n<Directory />\n    AllowOverride None\n    Require all denied\n</Directory>\nDocumentRoot \"{www}\"\n<Directory \"{www}\">\n    Options Indexes FollowSymLinks\n    AllowOverride All\n    Require all granted\n</Directory>\n\n",
        pid = slash(&paths.data.join("run").join("httpd.pid")),
    ));
    // PHP en FastCGI : un groupe de workers (balancer) par série installée ; la série par défaut sert tous les sites
    // qui n'en choisissent pas une autre (le vhost d'un site à série propre porte son propre SetHandler).
    if !pools.is_empty() {
        s.push_str(&crate::php_pool::balancer_conf(pools));
        if let Some(def) = crate::php_pool::default_series(cfg, pools) {
            s.push_str(&format!(
                "<FilesMatch \"\\.php$\">\n    SetHandler \"proxy:balancer://{}/\"\n    {}\n</FilesMatch>\n",
                crate::php_pool::balancer_name(def),
                crate::php_pool::SCRIPT_FILENAME_RULE
            ));
        }
    }
    if behind_caddy {
        // Sans cela, les applications PHP généreraient des liens http:// derrière Caddy (qui sert le HTTPS).
        s.push_str("SetEnvIf X-Forwarded-Proto \"https\" HTTPS=on\n");
    }
    s.push_str(&format!(
        "\n# Hôte par défaut (premier VirtualHost) : tout nom inconnu → www/\n<VirtualHost *:{port}>\n    ServerName localhost\n    DocumentRoot \"{www}\"\n</VirtualHost>\n\nIncludeOptional \"{sites}/*.conf\"\n",
        port = http_port,
        sites = slash(&paths.sites_enabled()),
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_paths() -> (Config, AppPaths) {
        let c = Config::standalone("C:/astral-dev", false, |_| false);
        let p = AppPaths::from_config(&c);
        (c, p)
    }

    fn pool(series: &str, dir: &str, ports: &[u16]) -> crate::php_pool::Pool {
        crate::php_pool::Pool { series: series.into(), dir: PathBuf::from(dir), ports: ports.to_vec() }
    }

    #[test]
    fn httpd_conf_serves_php_through_fastcgi_workers() {
        let (c, p) = cfg_paths();
        let pools = [pool("8.3", "C:/php-8.3-nts", &[9083, 9183]), pool("8.4", "C:/php-8.4-nts", &[9084, 9184])];
        let s = httpd_conf_text(&c, &p, Path::new("C:/astral-dev/bin/apache/a"), &pools);
        for m in ["proxy_module", "proxy_fcgi_module", "proxy_balancer_module", "slotmem_shm_module", "lbmethod_bybusyness_module"] {
            assert!(s.contains(&format!("LoadModule {m} ")), "{m}");
        }
        assert!(!s.contains("php_module") && !s.contains("fcgid"), "mod_php et mod_fcgid sont retirés");
        assert!(s.contains("<Proxy \"balancer://php83\">") && s.contains("fcgi://127.0.0.1:9184"));
        assert!(s.contains("SetHandler \"proxy:balancer://php83/\""), "la série par défaut (8.3) sert les sites sans série propre");
        assert!(!s.contains("SetHandler \"proxy:balancer://php84/\""));
        assert!(s.contains("Listen 127.0.0.1:80\n"));
        assert!(s.contains("IncludeOptional \"C:/astral-dev/etc/apache2/sites-enabled/*.conf\""));
    }

    #[test]
    fn httpd_conf_without_php_has_no_handler() {
        let (c, p) = cfg_paths();
        let s = httpd_conf_text(&c, &p, Path::new("C:/a"), &[]);
        assert!(!s.contains("SetHandler") && !s.contains("<Proxy"));
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
        let mut cfg = Config::standalone(&root, false, |_| false);
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
        crate::vhosts::ensure(&paths.sites_enabled(), "demo.test", &public, &paths.ssl_dir(), None, cfg.web.port, cfg.ssl_port()).unwrap();

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

    fn http_get_path(port: u16, host: &str, path: &str) -> String {
        use std::io::{Read, Write};
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
        c.write_all(format!("GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes()).unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out);
        out
    }

    /// Phase 3 (réel) : Apache + deux séries de PHP en FastCGI (8.3 par défaut, 8.4 pour un site), pool de 2 workers,
    /// charge parallèle, site qui s'appelle lui-même, relance d'un worker tué.
    /// `cargo test standalone_real_fastcgi_pool -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn standalone_real_fastcgi_pool() {
        use crate::sites::SiteConfig;
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-p3".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.web.port = 18090;
        cfg.mysql.port = 13317;
        cfg.redis.port = 16390;
        cfg.mailpit.http = 18036;
        cfg.mailpit.smtp = 11036;
        let paths = AppPaths::from_config(&cfg);
        let laragon = find_laragon().expect("Laragon");
        import_from_laragon(&laragon, &paths, &["8.3", "8.4"], &|l| println!("{l}")).unwrap();
        let _ = std::fs::remove_dir_all(paths.sites_enabled());
        std::fs::create_dir_all(paths.sites_enabled()).unwrap();
        let _ = std::fs::remove_file(paths.data.join("sites.toml"));
        println!("séries : {:?}", paths.php_series());
        assert!(paths.php_series().len() >= 2);

        let mk = |name: &str, code: &str| {
            let public = paths.www.join(name).join("public");
            std::fs::create_dir_all(&public).unwrap();
            std::fs::write(public.join("index.php"), code).unwrap();
            public
        };
        let info = "<?php echo 'v=' . PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION . ';sapi=' . php_sapi_name() . ';script=' . $_SERVER['SCRIPT_FILENAME'] . ';redis=' . (extension_loaded('redis') ? 'y' : 'n') . ';mysql=' . (extension_loaded('pdo_mysql') ? 'y' : 'n');";
        let pub_a = mk("demo", info);
        mk("demo84", info);
        std::fs::write(pub_a.join("sleep.php"), "<?php sleep(1); echo 'slept';").unwrap();
        std::fs::write(pub_a.join("ping.php"), "<?php echo 'pong';").unwrap();
        std::fs::write(
            pub_a.join("loop.php"),
            format!("<?php $c = stream_context_create(['http' => ['header' => 'Host: demo.test', 'timeout' => 20]]); echo 'loop:' . @file_get_contents('http://127.0.0.1:{}/ping.php', false, $c);", cfg.web.port),
        )
        .unwrap();
        std::fs::write(pub_a.join("pathinfo.php"), "<?php echo 'pi=' . ($_SERVER['PATH_INFO'] ?? '-') . ';uri=' . $_SERVER['REQUEST_URI'];").unwrap();

        let engine = crate::engines::from_config(&cfg).unwrap();
        let ectx = crate::engines::EngineCtx { cfg: &cfg, paths: &paths };
        let site = |name: &str, php: Option<&str>| SiteConfig {
            name: name.into(), domain: format!("{name}.test"), root: format!("www/{name}/public"), php: php.map(str::to_string), ssl: false, backend: None,
        };
        let (_, rep) = crate::engines::update_sites(
            &ectx, engine.as_ref(),
            |s| { s.upsert(site("demo", None)); s.upsert(site("demo84", Some("8.4"))); },
            &crate::engines::ApplyOpts { validate: false, reload: false, force: vec![] },
        )
        .unwrap();
        println!("sites écrits : {:?}", rep.written);
        let vh = std::fs::read_to_string(crate::vhosts::conf_path(&paths.sites_enabled(), "demo84.test")).unwrap();
        assert!(vh.contains("proxy:balancer://php84"), "{vh}");

        let sup = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs())
            .with_wanted_series(std::sync::Arc::new({ let (c, p) = (cfg.clone(), paths.clone()); move || crate::php_pool::wanted_series(&c, &p) }));
        let res = sup.start_all();
        for r in &res {
            println!("start {} : {:?}", r.id, r.note);
        }
        assert!(res.iter().all(|r| r.ok), "{res:?}");
        let workers: Vec<_> = sup.statuses().into_iter().filter(|s| s.pool.is_some()).collect();
        println!("workers : {:?}", workers.iter().map(|w| (w.id, w.state)).collect::<Vec<_>>());
        assert_eq!(workers.iter().filter(|w| w.state == "running").count(), 4, "2 séries × 2 workers");

        let body = |h: &str, path: &str| http_get_path(cfg.web.port, h, path);
        let a = body("demo.test", "/index.php");
        let b = body("demo84.test", "/index.php");
        println!("demo   -> {}", a.lines().last().unwrap_or(""));
        println!("demo84 -> {}", b.lines().last().unwrap_or(""));
        assert!(a.contains("v=8.3;sapi=cgi-fcgi") && b.contains("v=8.4;sapi=cgi-fcgi"), "{a}\n{b}");
        assert!(a.contains("script=") && a.contains("demo/public/index.php") || a.contains("demo\\public\\index.php"), "SCRIPT_FILENAME : {a}");
        assert!(a.contains("redis=y") && a.contains("mysql=y"), "{a}");
        let dir = body("demo.test", "/");
        assert!(dir.contains("v=8.3"), "DirectoryIndex : {dir}");
        let pi = body("demo.test", "/pathinfo.php/extra/part?x=1");
        println!("pathinfo -> {}", pi.lines().last().unwrap_or(""));
        assert!(pi.contains("pi=/extra/part"), "PATH_INFO : {pi}");

        // charge parallèle : 4 requêtes d'une seconde sur 2 workers ≈ 2 s (1 worker : 4 s)
        let t = std::time::Instant::now();
        let hs: Vec<_> = (0..4).map(|_| { let port = cfg.web.port; std::thread::spawn(move || http_get_path(port, "demo.test", "/sleep.php")) }).collect();
        let outs: Vec<String> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        let el = t.elapsed().as_secs_f32();
        println!("4 requêtes de 1 s en parallèle : {el:.1} s");
        assert!(outs.iter().all(|o| o.contains("slept")), "{outs:?}");
        assert!(el < 3.6, "les deux workers doivent travailler en parallèle ({el:.1} s)");

        // le site s'appelle lui-même (un seul worker se bloquerait)
        let l = body("demo.test", "/loop.php");
        println!("boucle HTTP -> {}", l.lines().last().unwrap_or(""));
        assert!(l.contains("loop:pong"), "{l}");

        // un worker tué est relancé
        let victim = workers.iter().find(|w| w.id == "php-8.3-1").and_then(|w| w.pid).expect("pid du worker");
        let _ = std::process::Command::new("taskkill").args(["/PID", &victim.to_string(), "/T", "/F"]).output();
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(!crate::probe::port_open(9083), "worker tué");
        let healed = sup.heal_workers();
        println!("relancés : {healed:?}");
        assert_eq!(healed, ["php-8.3-1"]);
        assert!(crate::probe::port_open(9083));
        assert!(body("demo.test", "/index.php").contains("v=8.3"));

        let stopped = sup.stop_all();
        assert!(stopped.iter().all(|r| r.ok), "{stopped:?}");
        std::thread::sleep(std::time::Duration::from_millis(500));
        for port in [9083, 9183, 9084, 9184, cfg.web.port] {
            assert!(!crate::probe::port_open(port), "port {port} libéré");
        }
    }

    /// Sans Laragon : installe TOUT depuis le catalogue (≈ 400 Mo) puis démarre la pile.
    /// `cargo test standalone_real_download -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn standalone_real_download_and_stack() {
        use crate::catalog::Catalog;
        use crate::components::{default_set, install, Options, Outcome};
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-dl".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.web.port = 18080;
        cfg.mysql.port = 13307;
        cfg.redis.port = 16380;
        cfg.mailpit.http = 18026;
        cfg.mailpit.smtp = 11026;
        let paths = AppPaths::from_config(&cfg);
        for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let cat = Catalog::embedded().unwrap();
        let mut set = default_set(&cat);
        set.push(("php".into(), "8.4".into()));
        set.push(("ext-redis".into(), "6.3.0-php8.4".into()));
        for (id, ver) in &set {
            let c = cat.component(id).unwrap();
            let v = c.versions.iter().find(|v| &v.version == ver).unwrap();
            let t = std::time::Instant::now();
            let out = install(&paths.root, &paths.data, c, v, &Options { allow_local_http: false }, &std::sync::atomic::AtomicBool::new(false), |_, _| {}, None)
                .unwrap_or_else(|e| panic!("{id} {ver} : {e}"));
            println!("{id} {ver} : {} ({:.0}s)", if matches!(out, Outcome::Installed(_)) { "installé" } else { "déjà là" }, t.elapsed().as_secs_f32());
        }
        for d in paths.php_dirs() {
            write_php_ini(&d, &paths.ssl_dir().join("cacert.pem")).unwrap();
        }
        let st = status(&cfg, &paths);
        assert!(st.ready, "manquants : {:?}", st.missing);

        let public = paths.www.join("demo").join("public");
        std::fs::create_dir_all(&public).unwrap();
        let pw = crate::services::redis_secret(&cfg, &paths).unwrap_or_default();
        let php = format!(
            "<?php $r='noredis'; if (extension_loaded('redis')) {{ try {{ $c=new Redis(); $c->connect('127.0.0.1',{port}); $c->auth('{pw}'); $c->set('astral:test','1'); $r=($c->get('astral:test')==='1')?'redis-ok':'redis-ko'; }} catch (Throwable $e) {{ $r='redis-err'; }} }} echo 'ok-' . PHP_MAJOR_VERSION . '.' . PHP_MINOR_VERSION . '-' . $r . '-' . (extension_loaded('pdo_mysql') ? 'mysql' : 'nomysql');",
            port = cfg.redis.port
        );
        std::fs::write(public.join("index.php"), php).unwrap();
        crate::vhosts::ensure(&paths.sites_enabled(), "demo.test", &public, &paths.ssl_dir(), None, cfg.web.port, cfg.ssl_port()).unwrap();

        for (ver, expect) in [("8.3", "ok-8.3-redis-ok"), ("8.4", "ok-8.4-redis-ok")] {
            let mut c = cfg.clone();
            c.php.version = ver.into();
            let p = AppPaths::from_config(&c);
            let sup = Supervisor::new(crate::services::build(&c, &p), p.logs());
            for r in sup.start_all() {
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

    /// Après un redémarrage brutal : un processus d'astral-dev resté actif est reconnu (« orphelin ») et arrêtable.
    /// Installe Redis dans C:/astral-dev-test-orphan si besoin. `cargo test standalone_real_orphan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn standalone_real_orphan_is_recognised_and_stopped() {
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-orphan".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.redis.port = 16390;
        let paths = AppPaths::from_config(&cfg);
        if paths.latest_in("redis").is_none() {
            // Installe seulement Redis (≈ 12 Mo) depuis le catalogue.
            let cat = crate::catalog::Catalog::embedded().unwrap();
            let c = cat.component("redis").unwrap();
            crate::components::install(&paths.root, &paths.data, c, &c.versions[0], &crate::components::Options { allow_local_http: false },
                &std::sync::atomic::AtomicBool::new(false), |_, _| {}, None).unwrap();
        }
        std::fs::create_dir_all(paths.logs()).unwrap();
        let first = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs()).with_own_root(Some(root.as_str()));
        first.start("redis").unwrap();
        assert!(crate::probe::redis_ping(cfg.redis.port));
        // « redémarrage brutal » : un nouveau superviseur sans mémoire des processus
        let second = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs()).with_own_root(Some(root.as_str()));
        let st = second.statuses().into_iter().find(|s| s.id == "redis").unwrap();
        println!("redis : external={} orphan={} conflict={:?}", st.external, st.orphan, st.conflict);
        assert!(st.external && st.orphan && st.conflict.is_none());
        let res = second.stop_everything();
        println!("{res:?}");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!crate::probe::port_open(cfg.redis.port), "le port doit être libéré");
        std::mem::forget(first);
    }

    /// HTTPS réel : installe Apache + PHP + mkcert depuis le catalogue (≈ 50 Mo), génère un certificat (sans `-install`),
    /// sert un vhost HTTPS sur 18443 et le lit avec curl -k. `cargo test standalone_real_https -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn standalone_real_https() {
        use crate::catalog::Catalog;
        use crate::components::{install, Options};
        use crate::supervisor::Supervisor;
        let root = std::env::var("ASTRAL_TEST_ROOT").unwrap_or_else(|_| "C:/astral-dev-test-https".into());
        let mut cfg = Config::standalone(&root, false, |_| false);
        cfg.web.port = 18081;
        cfg.web.ssl_port = 18443;
        cfg.mysql.port = 13308;
        cfg.redis.port = 16381;
        cfg.mailpit.http = 18027;
        cfg.mailpit.smtp = 11027;
        let paths = AppPaths::from_config(&cfg);
        for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let cat = Catalog::embedded().unwrap();
        for (id, ver) in [("php", "8.3"), ("cacert", "latest"), ("apache", "2.4.71"), ("mkcert", "1.4.4")] {
            let c = cat.component(id).unwrap();
            let v = c.versions.iter().find(|v| v.version == ver).unwrap();
            install(&paths.root, &paths.data, c, v, &Options { allow_local_http: false }, &std::sync::atomic::AtomicBool::new(false), |_, _| {}, None)
                .unwrap_or_else(|e| panic!("{id} : {e}"));
        }
        for d in paths.php_dirs() {
            write_php_ini(&d, &paths.ssl_dir().join("cacert.pem")).unwrap();
        }
        let mk = crate::mkcert::find(&paths.root, "").expect("mkcert installé");
        let (crt, key) = crate::mkcert::generate(&mk, &paths.ssl_dir(), "demo.test").expect("certificat");
        assert!(crt.is_file() && key.is_file());
        let public = paths.www.join("demo").join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::write(public.join("index.php"), "<?php echo 'https-ok-' . (($_SERVER['HTTPS'] ?? '') === 'on' ? 'on' : 'off');").unwrap();
        crate::vhosts::ensure(&paths.sites_enabled(), "demo.test", &public, &paths.ssl_dir(), Some((crt, key)), cfg.web.port, cfg.ssl_port()).unwrap();
        let sup = Supervisor::new(crate::services::build(&cfg, &paths), paths.logs());
        sup.start("apache").unwrap();
        let out = std::process::Command::new("curl.exe")
            .args(["-sk", "--resolve", &format!("demo.test:{}:127.0.0.1", cfg.ssl_port()), &format!("https://demo.test:{}/", cfg.ssl_port())])
            .output()
            .unwrap();
        let body = String::from_utf8_lossy(&out.stdout).to_string();
        println!("HTTPS -> {body}");
        sup.stop("apache").unwrap();
        assert!(body.contains("https-ok-on"), "{body}");
    }
}
