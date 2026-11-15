//! AppPaths : racine du stack (mode adopt = Laragon), www/, data/.
use crate::config::Config;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub www: PathBuf,
    pub data: PathBuf,
}

impl AppPaths {
    pub fn from_config(c: &Config) -> Self {
        Self {
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

    /// Dossier PHP réellement utilisé par Laragon (lu dans etc/apache2/mod_php.conf), sinon le plus récent.
    pub fn active_php(&self) -> Option<PathBuf> {
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
    fn slash_normalizes() {
        assert_eq!(slash(Path::new("D:\\laragon\\www")), "D:/laragon/www");
    }
}