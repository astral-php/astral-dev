//! AppPaths : racine du stack (mode adopt = Laragon), www/, data/.
use crate::config::Config;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub www: PathBuf,
    pub data: PathBuf,
    pub standalone: bool,
    pub php_version: String,
}

impl AppPaths {
    pub fn from_config(c: &Config) -> Self {
        Self {
            standalone: c.is_standalone(),
            php_version: c.php.version.clone(),
            root: PathBuf::from(&c.paths.root),
            www: PathBuf::from(&c.paths.www),
            data: PathBuf::from(&c.paths.data),
        }
    }

    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }
    pub fn sites_enabled(&self) -> PathBuf {
        self.root.join("etc").join("apache2").join("sites-enabled")
    }
    pub fn ssl_dir(&self) -> PathBuf {
        self.root.join("etc").join("ssl")
    }
    pub fn composer_phar(&self) -> PathBuf {
        self.bin().join("composer").join("composer.phar")
    }
    pub fn php_exe(&self) -> Option<PathBuf> {
        self.active_php().map(|d| d.join("php.exe")).filter(|p| p.is_file())
    }
    pub fn etc(&self) -> PathBuf {
        self.root.join("etc")
    }
    pub fn httpd_conf(&self) -> PathBuf {
        self.etc().join("apache2").join("httpd.conf")
    }
    pub fn mysql_ini(&self) -> PathBuf {
        self.etc().join("mysql").join("my.ini")
    }
    /// Datadir MySQL : propre à astral-dev en mode autonome, celui de Laragon sinon.
    pub fn mysql_data(&self) -> PathBuf {
        if self.standalone { self.data.join("mysql") } else { self.root.join("data").join("mysql-8.4") }
    }
    pub fn secrets(&self) -> PathBuf {
        self.data.join("secrets")
    }
    pub fn redis_data(&self) -> PathBuf {
        self.data.join("redis")
    }

    /// Dossier le plus récent (ordre alphabétique) sous `bin/<name>/`.
    pub fn latest_in(&self, name: &str) -> Option<PathBuf> {
        latest_dir(&self.bin().join(name))
    }

    /// Dossier Redis à utiliser : `version` vide = le plus récent installé ; `5` ou `8.10` = la plus haute version dont
    /// le numéro commence par ce préfixe (plusieurs builds peuvent coexister sous `bin/redis/`).
    pub fn redis_dir(&self, version: &str) -> Option<PathBuf> {
        let mut found: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(self.bin().join("redis"))
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter_map(|p| {
                let v = parse_version(&p.file_name()?.to_string_lossy());
                (!v.is_empty()).then_some((v, p))
            })
            .collect();
        if !version.is_empty() {
            let want: Vec<u32> = version.split('.').filter_map(|x| x.parse().ok()).collect();
            found.retain(|(v, _)| v.len() >= want.len() && v[..want.len()] == want[..]);
        }
        found.sort();
        found.pop().map(|(_, p)| p)
    }

    /// Dossier PHP réellement utilisé par Laragon (lu dans etc/apache2/mod_php.conf), sinon le plus récent.
    pub fn active_php(&self) -> Option<PathBuf> {
        if self.standalone {
            if let Some(d) = self.php_dir_for(&self.php_version) {
                return Some(d);
            }
            let prefix = format!("php-{}.", self.php_version);
            return self.php_dirs().into_iter().rev().find(|p| {
                p.file_name().map_or(false, |n| n.to_string_lossy().starts_with(&prefix))
            });
        }
        let conf = self.root.join("etc").join("apache2").join("mod_php.conf");
        if let Ok(text) = std::fs::read_to_string(conf) {
            let norm = text.replace('\\', "/");
            if let Some(i) = norm.find("/php/php-") {
                let rest = &norm[i + "/php/".len()..];
                let name: String = rest.chars().take_while(|c| *c != '/' && *c != '"').collect();
                let p = self.bin().join("php").join(name);
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
        self.latest_in("php")
    }
}

impl AppPaths {
    /// Dossier PHP d'une série (`8.3`) : build NTS de préférence, le plus récent ; il doit contenir `php-cgi.exe`.
    pub fn php_dir_for(&self, series: &str) -> Option<PathBuf> {
        let prefix = format!("php-{series}.");
        let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut c: Vec<PathBuf> = self
            .php_dirs()
            .into_iter()
            .filter(|p| name(p).starts_with(&prefix) && p.join("php-cgi.exe").is_file())
            .collect();
        c.sort_by_key(|p| (name(p).contains("-nts-"), name(p)));
        c.pop()
    }

    /// Séries de PHP installées (avec `php-cgi.exe`), ordre croissant : `["8.3", "8.4"]`.
    pub fn php_series(&self) -> Vec<String> {
        let mut v: Vec<(Vec<u32>, String)> = self
            .php_dirs()
            .iter()
            .filter(|d| d.join("php-cgi.exe").is_file())
            .filter_map(|d| {
                let n = d.file_name()?.to_string_lossy().to_string();
                let s = crate::php_pool::series_of(&n)?;
                Some((parse_version(&s), s))
            })
            .collect();
        v.sort();
        v.dedup();
        v.into_iter().map(|(_, s)| s).collect()
    }

    /// Tous les dossiers php-* sous bin/php (ordre alphabétique).
    pub fn php_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.bin().join("php"))
            .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        v.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
        v
    }
}

/// Premier numéro de version `x.y.z` d'un nom de dossier (`redis-x64-5.0.14.1` → [5, 0, 14, 1]).
pub fn parse_version(name: &str) -> Vec<u32> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || (bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit())) {
                i += 1;
            }
            let v: Vec<u32> = name[start..i].split('.').filter_map(|x| x.parse().ok()).collect();
            if v.len() >= 2 {
                return v;
            }
        } else {
            i += 1;
        }
    }
    vec![]
}

pub fn latest_dir(parent: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    dirs.pop()
}

pub fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_dir_picks_last_alphabetically() {
        let t = std::env::temp_dir().join(format!("astral-dev-test-{}", std::process::id()));
        for d in ["a-1.0", "a-2.0", "a-1.5"] {
            std::fs::create_dir_all(t.join(d)).unwrap();
        }
        assert_eq!(latest_dir(&t).unwrap().file_name().unwrap(), "a-2.0");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn redis_dir_picks_the_newest_or_the_requested_major() {
        assert_eq!(parse_version("redis-x64-5.0.14.1"), vec![5, 0, 14, 1]);
        assert_eq!(parse_version("redis8-8.10.3"), vec![8, 10, 3]);
        assert!(parse_version("sans-version").is_empty());
        let t = std::env::temp_dir().join(format!("astral-dev-rd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        for d in ["redis-x64-5.0.14.1", "redis8-8.10.3", "redis8-8.8.3"] {
            std::fs::create_dir_all(t.join("bin/redis").join(d)).unwrap();
        }
        let mut cfg = crate::config::Config::default();
        cfg.paths.root = t.to_string_lossy().replace('\\', "/");
        let p = AppPaths::from_config(&cfg);
        assert!(p.redis_dir("").unwrap().ends_with("redis8-8.10.3"), "automatique : le plus récent (8.10.3 > 8.8.3 > 5.0)");
        assert!(p.redis_dir("5").unwrap().ends_with("redis-x64-5.0.14.1"));
        assert!(p.redis_dir("8.8").unwrap().ends_with("redis8-8.8.3"));
        assert!(p.redis_dir("7").is_none());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn slash_normalizes() {
        assert_eq!(slash(Path::new("D:\\laragon\\www")), "D:/laragon/www");
    }
}