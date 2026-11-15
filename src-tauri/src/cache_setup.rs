//! « Utiliser Redis pour ce projet » (SPEC F-047/F-048, REDIS.md §3). Aucune écriture PHP : seulement le .env.
use crate::envfile;
use crate::error::AppError;
use crate::probe;
use crate::projects::env_value;
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const BACKUP: &str = ".env.astral-dev.bak";

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CacheStatus {
    pub astral: bool,
    pub has_env: bool,
    pub driver: Option<String>,
    /// "astral-cache" (dépendance Composer) | "integre" (cache propre au projet, ex. shop) | "absent"
    pub support: &'static str,
    /// CacheServiceProvider listé dans config/dependencies.php
    pub provider_registered: bool,
    pub ext_redis: bool,
    pub redis_up: bool,
    pub redis_port: u16,
    pub has_backup: bool,
}

pub fn php_has_ext_redis(php: &Path) -> bool {
    let mut c = Command::new(php);
    c.arg("-m").stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim().eq_ignore_ascii_case("redis")))
        .unwrap_or(false)
}

pub fn detect_support(dir: &Path) -> &'static str {
    let dep = std::fs::read_to_string(dir.join("composer.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|c| c.get("require").and_then(|r| r.get("astral-php/astral-cache")).is_some())
        .unwrap_or(false);
    if dep {
        "astral-cache"
    } else if dir.join("app").join("Cache").join("RedisCacheStore.php").is_file() {
        "integre"
    } else {
        "absent"
    }
}

pub fn status(dir: &Path, php: Option<&Path>, redis_port: u16) -> CacheStatus {
    let env = std::fs::read_to_string(dir.join(".env")).ok();
    CacheStatus {
        astral: dir.join("bin").join("console").is_file(),
        has_env: env.is_some(),
        driver: env.as_deref().and_then(|t| env_value(t, "CACHE_DRIVER")),
        support: detect_support(dir),
        provider_registered: std::fs::read_to_string(dir.join("config").join("dependencies.php"))
            .map(|t| t.contains("CacheServiceProvider"))
            .unwrap_or(false),
        ext_redis: php.map(php_has_ext_redis).unwrap_or(false),
        redis_up: probe::redis_ping(redis_port),
        redis_port,
        has_backup: dir.join(BACKUP).is_file(),
    }
}

fn refuse(msg: &str) -> Result<(), AppError> {
    Err(AppError::NotStartable(msg.into()))
}

/// Écrit CACHE_DRIVER=redis + REDIS_* (noms confirmés dans astral-cache). Sauvegarde le .env d'origine une seule fois.
/// `password` : mot de passe Redis (vide si Redis n'en demande pas) — il n'est jamais journalisé.
pub fn enable(dir: &Path, s: &CacheStatus, password: &str) -> Result<(), AppError> {
    if !s.astral {
        return refuse("ce dossier n'est pas un projet Astral (bin/console absent)");
    }
    if !s.has_env {
        return refuse(".env introuvable : copiez .env.example en .env d'abord");
    }
    if s.support == "absent" {
        return refuse("aucun cache compatible : installez astral-php/astral-cache (puis enregistrez CacheServiceProvider)");
    }
    if !s.ext_redis {
        return refuse("l'extension PHP redis (ext-redis) est absente du PHP actif");
    }
    if !s.redis_up {
        return refuse(&format!("Redis ne répond pas sur 127.0.0.1:{} — démarrez-le", s.redis_port));
    }
    let path = dir.join(".env");
    let text = std::fs::read_to_string(&path)?;
    let backup = dir.join(BACKUP);
    if !backup.exists() {
        std::fs::write(&backup, &text)?;
    }
    let port = s.redis_port.to_string();
    let out = envfile::set_keys(
        &text,
        &[
            ("CACHE_DRIVER", "redis"),
            ("REDIS_HOST", "127.0.0.1"),
            ("REDIS_PORT", &port),
            ("REDIS_PASSWORD", password),
            ("REDIS_DB", "0"),
        ],
    );
    std::fs::write(&path, out)?;
    Ok(())
}

/// Retour arrière : remet le driver d'origine (celui de la sauvegarde, jamais « redis » → « file »).
/// Les clés REDIS_* restent en place (inoffensives hors CACHE_DRIVER=redis).
pub fn disable(dir: &Path) -> Result<String, AppError> {
    let path = dir.join(".env");
    let text = std::fs::read_to_string(&path).map_err(|_| AppError::NotStartable(".env introuvable".into()))?;
    let previous = std::fs::read_to_string(dir.join(BACKUP))
        .ok()
        .and_then(|b| env_value(&b, "CACHE_DRIVER"))
        .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("redis"))
        .unwrap_or_else(|| "file".into());
    std::fs::write(&path, envfile::set_keys(&text, &[("CACHE_DRIVER", &previous)]))?;
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_status() -> CacheStatus {
        CacheStatus {
            astral: true, has_env: true, driver: Some("file".into()), support: "astral-cache",
            provider_registered: true, ext_redis: true, redis_up: true, redis_port: 6379, has_backup: false,
        }
    }

    fn project(tag: &str, env: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("astral-dev-cache-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/console"), "").unwrap();
        std::fs::write(d.join(".env"), env).unwrap();
        d
    }

    #[test]
    fn enable_writes_keys_backs_up_once_and_disable_restores_driver() {
        let d = project("cycle", "APP_NAME=x\nCACHE_DRIVER=array\n");
        enable(&d, &ok_status(), "").unwrap();
        let env = std::fs::read_to_string(d.join(".env")).unwrap();
        assert!(env.contains("CACHE_DRIVER=redis\n") && env.contains("REDIS_HOST=127.0.0.1\n") && env.contains("REDIS_PORT=6379\n"));
        assert!(env.contains("REDIS_DB=0") && env.starts_with("APP_NAME=x\n"));
        enable(&d, &ok_status(), "").unwrap(); // idempotent, sauvegarde d'origine conservée
        assert_eq!(std::fs::read_to_string(d.join(BACKUP)).unwrap(), "APP_NAME=x\nCACHE_DRIVER=array\n");
        assert_eq!(disable(&d).unwrap(), "array");
        assert!(env_value(&std::fs::read_to_string(d.join(".env")).unwrap(), "CACHE_DRIVER").as_deref() == Some("array"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn enable_writes_the_redis_password_when_given() {
        let d = project("pw", "CACHE_DRIVER=file\n");
        enable(&d, &ok_status(), "abc123").unwrap();
        let env = std::fs::read_to_string(d.join(".env")).unwrap();
        assert_eq!(env_value(&env, "REDIS_PASSWORD").as_deref(), Some("abc123"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn disable_never_returns_redis_and_defaults_to_file() {
        let d = project("dis", "CACHE_DRIVER=redis\n");
        assert_eq!(disable(&d).unwrap(), "file");
        std::fs::write(d.join(BACKUP), "CACHE_DRIVER=redis\n").unwrap();
        assert_eq!(disable(&d).unwrap(), "file");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn preconditions_block_any_write() {
        let d = project("pre", "CACHE_DRIVER=file\n");
        for mutate in [
            |s: &mut CacheStatus| s.astral = false,
            |s: &mut CacheStatus| s.has_env = false,
            |s: &mut CacheStatus| s.support = "absent",
            |s: &mut CacheStatus| s.ext_redis = false,
            |s: &mut CacheStatus| s.redis_up = false,
        ] {
            let mut s = ok_status();
            mutate(&mut s);
            assert!(enable(&d, &s, "").is_err());
        }
        assert_eq!(std::fs::read_to_string(d.join(".env")).unwrap(), "CACHE_DRIVER=file\n");
        assert!(!d.join(BACKUP).exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Réel : Redis (16381) + php.exe + phpredis lisent les valeurs écrites dans le .env. `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn enable_then_real_phpredis_uses_the_env() {
        use crate::{config::Config, paths::AppPaths, redis_client, services, supervisor::Supervisor};
        let mut cfg = Config::default();
        cfg.redis.port = 16381;
        cfg.paths.data = std::env::temp_dir().join(format!("astral-dev-c4-{}", std::process::id())).to_string_lossy().to_string();
        let paths = AppPaths::from_config(&cfg);
        let sup = Supervisor::new(services::build(&cfg, &paths), paths.logs());
        sup.start("redis").unwrap();

        let d = project("real", "APP_NAME=x\nCACHE_DRIVER=file\n");
        std::fs::write(d.join("composer.json"), r#"{"require":{"astral-php/astral-cache":"^1.0"}}"#).unwrap();
        let php = paths.php_exe().expect("php.exe");
        let st = status(&d, Some(&php), 16381);
        assert!(st.ext_redis && st.redis_up && st.support == "astral-cache", "{st:?}");
        enable(&d, &st, "").unwrap();

        let script = "$e=parse_ini_file('.env'); $r=new Redis(); \
            $r->connect($e['REDIS_HOST'],(int)$e['REDIS_PORT']); \
            if($e['REDIS_PASSWORD']!==''){$r->auth($e['REDIS_PASSWORD']);} $r->select((int)$e['REDIS_DB']); \
            $r->set('astral:cache:probe','ok'); echo $e['CACHE_DRIVER'];";
        let mut out = vec![];
        let code = crate::console::run(&php, &["-r".into(), script.into()], &d, None, |_, l| out.push(l)).unwrap();
        assert_eq!((code, out.join("")), (0, "redis".to_string()));
        let t = redis_client::Target::new(16381);
        assert_eq!(redis_client::get(&t, "astral:cache:probe").unwrap().value, serde_json::json!("ok"));
        assert_eq!(redis_client::flush_prefix(&t, "astral:cache:").unwrap(), 1);

        assert_eq!(disable(&d).unwrap(), "file");
        sup.stop("redis").unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn detects_cache_support() {
        let d = project("sup", "");
        assert_eq!(detect_support(&d), "absent");
        std::fs::create_dir_all(d.join("app/Cache")).unwrap();
        std::fs::write(d.join("app/Cache/RedisCacheStore.php"), "").unwrap();
        assert_eq!(detect_support(&d), "integre");
        std::fs::write(d.join("composer.json"), r#"{"require":{"astral-php/astral-cache":"^1.0"}}"#).unwrap();
        assert_eq!(detect_support(&d), "astral-cache");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
