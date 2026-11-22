//! Pools PHP FastCGI (ADR-017) : un service `php-cgi` par worker, plusieurs workers par série de PHP.
//! Apache (et plus tard Caddy) répartit les requêtes entre les workers d'une série (`bybusyness` / `least_conn`) :
//! php-cgi ne traite qu'une requête à la fois, un seul worker bloquerait un site qui s'appelle lui-même (mesuré en phase 3).
use crate::config::Config;
use crate::paths::{parse_version, AppPaths};
use crate::services::{Launch, ServiceRole, ServiceSpec, Stop};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_WORKERS: u32 = 8;

/// Avec `SetHandler "proxy:balancer://…/"`, Apache transmet à php-cgi un SCRIPT_FILENAME erroné (« No input file
/// specified »). Mesuré : `REQUEST_FILENAME` vaut `proxy:fcgi://hôte:port/<chemin réel>` ; on en extrait le chemin réel.
/// Fonctionne avec les réécritures de .htaccess (index.php), PATH_INFO et DirectoryIndex.
pub const SCRIPT_FILENAME_RULE: &str = r##"ProxyFCGISetEnvIf "%{REQUEST_FILENAME} =~ m#^proxy:fcgi://[^/]+/(.+)$#" SCRIPT_FILENAME "$1""##;

/// Série (`8.3`) d'un nom de dossier PHP (`php-8.3.37-nts-Win32-vs16-x64`).
pub fn series_of(dir_name: &str) -> Option<String> {
    let v = parse_version(dir_name);
    (v.len() >= 2).then(|| format!("{}.{}", v[0], v[1]))
}

/// Port du premier worker d'une série : `9000 + majeure×10 + mineure` (8.3 → 9083). Le worker n est à `+100×n`.
pub fn base_port(series: &str) -> Option<u16> {
    let (a, b) = series.split_once('.')?;
    let (a, b): (u16, u16) = (a.parse().ok()?, b.parse().ok()?);
    (a < 10 && b < 10).then(|| 9000 + a * 10 + b)
}

pub fn worker_port(series: &str, index: u32) -> Option<u16> {
    base_port(series).map(|p| p + 100 * index as u16)
}

/// Nom du groupe de workers côté Apache (`balancer://php83`).
pub fn balancer_name(series: &str) -> String {
    format!("php{}", series.replace('.', ""))
}

/// Les identifiants de service sont `&'static str` : ceux des workers sont créés une fois puis réutilisés.
fn intern(s: String) -> &'static str {
    static POOL: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut g = POOL.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(v) = map.get(&s) {
        return v;
    }
    let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
    map.insert(s, leaked);
    leaked
}

pub fn worker_id(series: &str, index: u32) -> &'static str {
    intern(format!("php-{series}-{}", index + 1))
}

/// Nombre de workers par série, borné à 1–8.
pub fn workers_of(cfg: &Config) -> u32 {
    cfg.php.workers.clamp(1, MAX_WORKERS)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub series: String,
    pub dir: PathBuf,
    pub ports: Vec<u16>,
}

/// Une entrée par série de PHP installée (mode autonome uniquement : en mode adopt, Laragon garde mod_php).
pub fn pools(cfg: &Config, paths: &AppPaths) -> Vec<Pool> {
    if !paths.standalone {
        return vec![];
    }
    let n = workers_of(cfg);
    paths
        .php_series()
        .into_iter()
        .filter_map(|s| {
            let dir = paths.php_dir_for(&s)?;
            let ports: Vec<u16> = (0..n).filter_map(|i| worker_port(&s, i)).collect();
            (ports.len() == n as usize).then_some(Pool { series: s, dir, ports })
        })
        .collect()
}

/// Série par défaut : celle de `[php] version` si elle est installée, sinon la plus récente.
pub fn default_series<'a>(cfg: &Config, pools: &'a [Pool]) -> Option<&'a str> {
    pools
        .iter()
        .find(|p| p.series == cfg.php.version)
        .or_else(|| pools.last())
        .map(|p| p.series.as_str())
}

/// Séries à faire tourner : la série par défaut et celles que des sites demandent (`sites.toml`, champ `php`).
pub fn wanted_series(cfg: &Config, paths: &AppPaths) -> std::collections::BTreeSet<String> {
    let pools = pools(cfg, paths);
    let mut out = std::collections::BTreeSet::new();
    if let Some(d) = default_series(cfg, &pools) {
        out.insert(d.to_string());
    }
    if let Ok(sites) = crate::sites::Sites::load_or_migrate(paths, &cfg.web.tld) {
        for s in &sites.sites {
            if let Some(v) = &s.php {
                if pools.iter().any(|p| &p.series == v) {
                    out.insert(v.clone());
                }
            }
        }
    }
    out
}

/// Un `ServiceSpec` par worker (identifiant `php-8.3-1`, `php-8.3-2`…), lancé avant le moteur web.
pub fn specs(cfg: &Config, paths: &AppPaths) -> Vec<ServiceSpec> {
    let mut out = vec![];
    for p in pools(cfg, paths) {
        let build = p.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let nts = build.contains("-nts-");
        for (i, port) in p.ports.iter().enumerate() {
            let launch = if p.dir.join("php-cgi.exe").is_file() {
                Ok(Launch {
                    program: p.dir.join("php-cgi.exe"),
                    args: vec!["-b".into(), format!("127.0.0.1:{port}")],
                    cwd: p.dir.clone(),
                    write_files: vec![],
                    init: None,
                    // php-cgi se recycle seul après N requêtes si on ne le désactive pas ; php.ini = celui du dossier PHP
                    env: vec![
                        ("PHP_FCGI_MAX_REQUESTS".into(), "0".into()),
                        ("PHPRC".into(), crate::paths::slash(&p.dir)),
                    ],
                })
            } else {
                Err(format!("php-cgi.exe introuvable dans {}", p.dir.display()))
            };
            out.push(ServiceSpec {
                id: worker_id(&p.series, i as u32),
                name: intern(format!("PHP {} · worker {}", p.series, i + 1)),
                port: Some(*port),
                meta: format!("php-cgi {}{} · port {port}", p.series, if nts { "" } else { " (TS)" }),
                enabled: true,
                start_timeout_secs: 10,
                runtime: false,
                extra_ports: vec![],
                role: ServiceRole::PhpWorker,
                pool: Some(p.series.clone()),
                preflight: None,
                launch,
                stop: Stop::Kill,
            });
        }
    }
    out
}

/// Séries dont seul un build « Thread Safe » (TS) est installé : il fonctionne en FastCGI, mais le build NTS est
/// celui que recommande astral-dev (migration guidée, rien n'est supprimé automatiquement).
pub fn ts_only_warnings(paths: &AppPaths) -> Vec<String> {
    if !paths.standalone {
        return vec![];
    }
    let mut out = vec![];
    for s in paths.php_series() {
        let has_nts = paths.php_dirs().iter().any(|d| {
            let n = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            n.starts_with(&format!("php-{s}.")) && n.contains("-nts-") && d.join("php-cgi.exe").is_file()
        });
        if !has_nts {
            out.push(format!(
                "PHP {s} est installé en version Thread Safe (TS). Il fonctionne, mais astral-dev recommande la version NTS : installez « PHP {s} » dans Composants (vos projets ne sont pas touchés), puis supprimez l'ancienne version."
            ));
        }
    }
    out
}

/// Groupes de workers pour Apache : un `<Proxy balancer://phpXX>` par série installée.
pub fn balancer_conf(pools: &[Pool]) -> String {
    let mut s = String::new();
    for p in pools {
        s.push_str(&format!("<Proxy \"balancer://{}\">\n", balancer_name(&p.series)));
        for port in &p.ports {
            s.push_str(&format!("    BalancerMember \"fcgi://127.0.0.1:{port}\" disablereuse=On connectiontimeout=5 timeout=3600\n"));
        }
        s.push_str("    ProxySet lbmethod=bybusyness\n</Proxy>\n");
    }
    s
}

/// Supprime les journaux de workers qui n'existent plus (`php-8.3-3.log` quand le pool est revenu à 2 workers).
/// Retourne les fichiers supprimés ; ne touche que les noms `php-<majeur>.<mineur>-<n>.log` avec `n` > nombre de workers.
pub fn prune_logs(cfg: &Config, paths: &AppPaths) -> Vec<String> {
    let n = workers_of(cfg);
    let mut removed = vec![];
    let Ok(rd) = std::fs::read_dir(paths.logs()) else { return removed };
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_prefix("php-").and_then(|s| s.strip_suffix(".log")) else { continue };
        let Some((series, idx)) = stem.rsplit_once('-') else { continue };
        let (Some(_), Ok(i)) = (base_port(series), idx.parse::<u32>()) else { continue };
        if i > n && std::fs::remove_file(e.path()).is_ok() {
            removed.push(name);
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_worker_logs_are_pruned_and_other_logs_kept() {
        let t = std::env::temp_dir().join(format!("astral-dev-prune-{}", std::process::id()));
        let mut cfg = Config::standalone(&t.to_string_lossy().replace('\\', "/"), false, |_| false);
        cfg.php.workers = 2;
        let paths = AppPaths::from_config(&cfg);
        std::fs::create_dir_all(paths.logs()).unwrap();
        for n in ["php-8.3-1.log", "php-8.3-2.log", "php-8.3-3.log", "php-8.4-4.log", "apache.log", "php-notes.log", "php-8.3-x.log"] {
            std::fs::write(paths.logs().join(n), "x").unwrap();
        }
        let mut gone = prune_logs(&cfg, &paths);
        gone.sort();
        assert_eq!(gone, ["php-8.3-3.log", "php-8.4-4.log"]);
        assert!(paths.logs().join("php-8.3-2.log").is_file() && paths.logs().join("apache.log").is_file() && paths.logs().join("php-notes.log").is_file());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn ports_follow_the_series_and_the_worker_index() {
        assert_eq!(base_port("8.3"), Some(9083));
        assert_eq!(base_port("8.6"), Some(9086));
        assert_eq!(worker_port("8.4", 0), Some(9084));
        assert_eq!(worker_port("8.4", 1), Some(9184));
        assert_eq!(worker_port("8.4", 7), Some(9784));
        assert_eq!(base_port("8.10"), None);
        assert_eq!(base_port("x"), None);
        assert_eq!(series_of("php-8.3.37-nts-Win32-vs16-x64").as_deref(), Some("8.3"));
        assert_eq!(series_of("php-8.5.13-Win32-vs17-x64").as_deref(), Some("8.5"));
        assert_eq!(series_of("sans-version"), None);
        assert_eq!(balancer_name("8.3"), "php83");
    }

    #[test]
    fn worker_ids_are_stable_and_unique() {
        assert_eq!(worker_id("8.3", 0), "php-8.3-1");
        assert!(std::ptr::eq(worker_id("8.3", 1), worker_id("8.3", 1)), "même identifiant réutilisé, pas de fuite répétée");
        assert_ne!(worker_id("8.3", 0), worker_id("8.4", 0));
    }

    fn php_tree(tag: &str, dirs: &[&str]) -> (Config, AppPaths, PathBuf) {
        let t = std::env::temp_dir().join(format!("astral-dev-pool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        for d in dirs {
            std::fs::create_dir_all(t.join("bin/php").join(d)).unwrap();
            std::fs::write(t.join("bin/php").join(d).join("php-cgi.exe"), "").unwrap();
        }
        let mut cfg = Config::default();
        cfg.mode = "standalone".into();
        let r = t.to_string_lossy().replace('\\', "/");
        cfg.paths.root = r.clone();
        cfg.paths.www = format!("{r}/www");
        cfg.paths.data = format!("{r}/data");
        let paths = AppPaths::from_config(&cfg);
        (cfg, paths, t)
    }

    #[test]
    fn pools_and_specs_for_installed_series() {
        let (cfg, paths, t) = php_tree("specs", &["php-8.3.37-nts-Win32-vs16-x64", "php-8.4.28-nts-Win32-vs17-x64"]);
        let ps = pools(&cfg, &paths);
        assert_eq!(ps.iter().map(|p| p.series.as_str()).collect::<Vec<_>>(), ["8.3", "8.4"]);
        assert_eq!(ps[0].ports, [9083, 9183]);
        assert_eq!(default_series(&cfg, &ps), Some("8.3"));
        let sp = specs(&cfg, &paths);
        assert_eq!(sp.iter().map(|s| s.id).collect::<Vec<_>>(), ["php-8.3-1", "php-8.3-2", "php-8.4-1", "php-8.4-2"]);
        let l = sp[1].launch.as_ref().unwrap();
        assert_eq!(l.args, ["-b", "127.0.0.1:9183"]);
        assert!(l.env.iter().any(|(k, v)| k == "PHP_FCGI_MAX_REQUESTS" && v == "0"));
        assert!(sp.iter().all(|s| s.role == ServiceRole::PhpWorker && s.pool.is_some() && !s.runtime));
        let conf = balancer_conf(&ps);
        assert!(conf.contains("<Proxy \"balancer://php83\">") && conf.contains("fcgi://127.0.0.1:9183") && conf.contains("lbmethod=bybusyness"));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn worker_count_is_configurable_and_bounded() {
        let (mut cfg, paths, t) = php_tree("count", &["php-8.3.37-nts-Win32-vs16-x64"]);
        cfg.php.workers = 4;
        assert_eq!(pools(&cfg, &paths)[0].ports, [9083, 9183, 9283, 9383]);
        cfg.php.workers = 0;
        assert_eq!(workers_of(&cfg), 1);
        cfg.php.workers = 99;
        assert_eq!(workers_of(&cfg), 8);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn adopt_mode_has_no_pool_and_default_falls_back_to_newest() {
        let (mut cfg, paths, t) = php_tree("adopt", &["php-8.4.28-nts-Win32-vs17-x64", "php-8.5.13-nts-Win32-vs17-x64"]);
        cfg.php.version = "8.3".into(); // absente : on retombe sur la plus récente
        let ps = pools(&cfg, &paths);
        assert_eq!(default_series(&cfg, &ps), Some("8.5"));
        let mut adopt = cfg.clone();
        adopt.mode = "adopt".into();
        assert!(pools(&adopt, &AppPaths::from_config(&adopt)).is_empty());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn thread_safe_build_is_used_when_it_is_the_only_one() {
        let (cfg, paths, t) = php_tree("ts", &["php-8.3.30-Win32-vs16-x64", "php-8.4.28-nts-Win32-vs17-x64"]);
        let sp = specs(&cfg, &paths);
        assert!(sp[0].meta.contains("(TS)"));
        let w = ts_only_warnings(&paths);
        assert_eq!(w.len(), 1, "seule la série 8.3 (TS sans NTS) est signalée");
        assert!(w[0].contains("PHP 8.3") && w[0].contains("NTS"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
