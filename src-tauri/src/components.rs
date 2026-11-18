//! Installation d'un composant du catalogue (v0.2, lot 7.4, ADR-012) :
#![allow(dead_code)] // câblé aux commandes Tauri (lot 7.4)
//! résolution de l'URL et de la somme → téléchargement vérifié → installation atomique → `installed.json`.
use crate::catalog::{host_allowed, is_hex64, is_safe_rel, Component, Version};
use crate::download::{self, Request};
use crate::error::AppError;
use crate::install::{self, Installed, InstalledComponent};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const PHP_BASE: &str = "https://downloads.php.net/~windows/releases/";
const MAX_TEXT: usize = 8 * 1024 * 1024;

fn err(msg: impl Into<String>) -> AppError {
    AppError::Io(msg.into())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub url: String,
    pub sha256: String,
    /// Version / nom exact (ex. `php-8.3.36-1-Win32-vs16-x64`, `v1.33.1`).
    pub resolved: String,
    pub headers: BTreeMap<String, String>,
}

/// Lit un document texte distant (sommes, JSON de versions).
pub type TextGetter<'a> = &'a dyn Fn(&str, &BTreeMap<String, String>) -> Result<String, AppError>;

/// `releases.json` de PHP → (chemin du zip, sha256) pour une clé `8.3.ts` / `8.4.nts`.
pub fn php_from_releases(json: &str, key: &str) -> Result<(String, String), AppError> {
    let (series, variant) = key.split_once('.').and_then(|_| key.rsplit_once('.')).ok_or_else(|| err("clé PHP invalide"))?;
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| err(format!("releases.json : {e}")))?;
    let rel = v.get(series).ok_or_else(|| err(format!("PHP {series} absent de releases.json")))?;
    let prefix = format!("{variant}-vs");
    let (_, build) = rel
        .as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.starts_with(&prefix) && k.ends_with("-x64")))
        .ok_or_else(|| err(format!("PHP {series} {variant} x64 absent de releases.json")))?;
    let zip = build.get("zip").ok_or_else(|| err("entrée zip absente"))?;
    let path = zip.get("path").and_then(|p| p.as_str()).ok_or_else(|| err("chemin absent"))?;
    let sha = zip.get("sha256").and_then(|p| p.as_str()).ok_or_else(|| err("somme absente"))?;
    if !is_hex64(sha) || path.contains(['/', '\\']) || !path.ends_with(".zip") {
        return Err(err("entrée releases.json invalide"));
    }
    Ok((path.to_string(), sha.to_ascii_lowercase()))
}

/// Dernière version GitHub → (url, sha256, tag) pour l'actif `asset`.
pub fn github_asset(json: &str, asset: &str) -> Result<(String, String, String), AppError> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| err(format!("API GitHub : {e}")))?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).ok_or_else(|| err("tag absent"))?;
    let a = v
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|x| x.get("name").and_then(|n| n.as_str()) == Some(asset)))
        .ok_or_else(|| err(format!("fichier {asset} absent de la version {tag}")))?;
    let url = a.get("browser_download_url").and_then(|u| u.as_str()).ok_or_else(|| err("url absente"))?;
    let digest = a.get("digest").and_then(|d| d.as_str()).and_then(|d| d.strip_prefix("sha256:")).ok_or_else(|| err("GitHub ne fournit pas de somme pour ce fichier"))?;
    if !is_hex64(digest) {
        return Err(err("somme GitHub invalide"));
    }
    Ok((url.to_string(), digest.to_ascii_lowercase(), tag.to_string()))
}

fn clean_name(s: &str) -> Result<String, AppError> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        Ok(s.to_string())
    } else {
        Err(err(format!("nom de version non sûr : {s}")))
    }
}

pub fn resolve(comp: &Component, ver: &Version, get: TextGetter) -> Result<Resolved, AppError> {
    let mut headers = ver.headers.clone();
    if let Some(src) = &ver.source {
        match src.kind.as_str() {
            "php-releases" => {
                let url = src.url.as_deref().ok_or_else(|| err("source PHP sans url"))?;
                let key = src.key.as_deref().ok_or_else(|| err("source PHP sans clé"))?;
                let (path, sha) = php_from_releases(&get(url, &headers)?, key)?;
                return Ok(Resolved {
                    url: format!("{PHP_BASE}{path}"),
                    sha256: sha,
                    resolved: clean_name(path.trim_end_matches(".zip"))?,
                    headers,
                });
            }
            "github-release" => {
                let repo = src.repo.as_deref().ok_or_else(|| err("source GitHub sans dépôt"))?;
                let asset = src.asset.as_deref().ok_or_else(|| err("source GitHub sans fichier"))?;
                if repo.split('/').count() != 2 || !repo.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')) {
                    return Err(err("dépôt GitHub invalide"));
                }
                headers.insert("Accept".into(), "application/vnd.github+json".into());
                let (url, sha, tag) = github_asset(&get(&format!("https://api.github.com/repos/{repo}/releases/latest"), &headers)?, asset)?;
                headers.remove("Accept");
                return Ok(Resolved { url, sha256: sha, resolved: clean_name(&tag)?, headers });
            }
            other => return Err(err(format!("type de source inconnu : {other}"))),
        }
    }
    let url = ver.url.clone().ok_or_else(|| err("version sans url ni source"))?;
    let sha = if let Some(p) = &ver.sha256.pinned {
        p.to_ascii_lowercase()
    } else if ver.sha256.from.as_deref() == Some("sidecar") {
        let su = ver.sha256.url.as_deref().ok_or_else(|| err("somme sidecar sans url"))?;
        download::parse_sidecar(&get(su, &headers)?).ok_or_else(|| err("fichier de somme illisible"))?
    } else {
        return Err(err(format!("{} {} : aucune somme vérifiable, installation refusée", comp.id, ver.version)));
    };
    Ok(Resolved { url, sha256: sha, resolved: clean_name(&ver.version)?, headers })
}

/// Dossier PHP installé pour une clé `8.3.ts` / `8.4.nts` (le plus récent), sous `root/bin/php`.
pub fn find_php_dir(root: &Path, key: &str) -> Option<String> {
    let (series, variant) = key.rsplit_once('.')?;
    let prefix = format!("php-{series}.");
    let nts = variant == "nts";
    let mut names: Vec<String> = std::fs::read_dir(root.join("bin").join("php"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(&prefix) && n.contains("-nts-") == nts)
        .collect();
    names.sort();
    names.pop()
}

/// Dossier d'installation : `extract_to` avec `{resolved}` et `{php}` remplacés, toujours sous `root`.
pub fn target_dir(root: &Path, ver: &Version, resolved: &str) -> Result<PathBuf, AppError> {
    let mut rel = ver.extract_to.replace("{resolved}", &clean_name(resolved)?);
    if rel.contains("{php}") {
        let key = ver.for_php.as_deref().ok_or_else(|| err("{php} sans for_php"))?;
        let dir = find_php_dir(root, key).ok_or_else(|| err(format!("installez d'abord PHP {key}")))?;
        rel = rel.replace("{php}", &clean_name(&dir)?);
    }
    if !is_safe_rel(&rel) {
        return Err(err("destination non sûre"));
    }
    Ok(root.join(rel))
}

/// Jeu recommandé pour une première installation : la première version de chaque composant du catalogue
/// (PHP 8.3 ; les autres versions de PHP sont facultatives).
pub fn default_set(cat: &crate::catalog::Catalog) -> Vec<(String, String)> {
    cat.components
        .iter()
        .filter_map(|c| c.versions.first().map(|v| (c.id.clone(), v.version.clone())))
        .collect()
}

/// Un composant du catalogue est-il déjà présent sur ce poste ?
pub fn is_installed(paths: &crate::paths::AppPaths, inst: &Installed, comp: &Component, ver: &Version) -> bool {
    if let Some(name) = &ver.single_file {
        return paths.root.join(&ver.extract_to).join(name).is_file();
    }
    if let Some(pick) = &ver.pick_file {
        let n = Path::new(pick).file_name().map(|n| n.to_os_string()).unwrap_or_default();
        return match target_dir(&paths.root, ver, &ver.version) {
            Ok(dir) => dir.join(n).is_file(),
            Err(_) => false, // PHP correspondant absent
        };
    }
    if comp.id == "php" {
        let nts = ver.variant.as_deref() == Some("nts");
        let prefix = format!("php-{}.", ver.version);
        return paths.php_dirs().iter().any(|d| {
            d.file_name().map_or(false, |n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.contains("-nts-") == nts
            })
        });
    }
    if ver.extract_to.contains("{resolved}") {
        // version « dernière » : présent si un dossier existe sous bin/<id>/ ou si installed.json le mentionne
        return inst.components.iter().any(|c| c.id == comp.id) || paths.latest_in(&comp.id).is_some();
    }
    paths.root.join(&ver.extract_to).exists()
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Installed(InstalledComponent),
    AlreadyInstalled(PathBuf),
}

pub struct Options {
    /// Tests uniquement : autorise http://127.0.0.1.
    pub allow_local_http: bool,
}

fn file_name_of(url: &str) -> String {
    let n = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or("download.bin");
    let n: String = n.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    if n.is_empty() { "download.bin".into() } else { n }
}

fn now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}") // epoch secondes (suffisant pour l'état local)
}

/// Installe `ver` du composant `comp` sous `root`. `progress(reçus, total)`.
pub fn install(
    root: &Path,
    data: &Path,
    comp: &Component,
    ver: &Version,
    opt: &Options,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
    resolved: Option<Resolved>,
) -> Result<Outcome, AppError> {
    let allowed = &comp.allowed_hosts;
    let get = |url: &str, h: &BTreeMap<String, String>| -> Result<String, AppError> {
        download::fetch_text(&Request { url, headers: h, allowed_hosts: allowed, allow_local_http: opt.allow_local_http }, MAX_TEXT)
    };
    let r = match resolved {
        Some(r) => r,
        None => resolve(comp, ver, &get)?,
    };
    if !is_hex64(&r.sha256) {
        return Err(err("somme SHA-256 invalide"));
    }
    if !opt.allow_local_http && !host_allowed(&r.url, allowed) {
        return Err(err(format!("adresse refusée : {}", r.url)));
    }
    let target = target_dir(root, ver, &r.resolved)?;
    let single = ver.single_file.is_some() || ver.pick_file.is_some();
    if !single && target.exists() {
        return Ok(Outcome::AlreadyInstalled(target));
    }

    let dl_dir = data.join("downloads");
    let archive = dl_dir.join(format!("{}.part", file_name_of(&r.url)));
    let req = Request { url: &r.url, headers: &r.headers, allowed_hosts: allowed, allow_local_http: opt.allow_local_http };
    let actual = download::fetch(&req, &archive, cancel, &mut progress)?;
    download::verify_or_delete(&archive, &actual, &r.sha256)?;

    let result = (|| -> Result<(), AppError> {
        if let Some(name) = &ver.single_file {
            install::install_single(&archive, &target, name)?;
        } else if let Some(pick) = &ver.pick_file {
            let tmp = data.join("tmp").join(format!("{}-{}", comp.id, std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let res = (|| -> Result<(), AppError> {
                install::install_zip(&archive, &tmp, ver.strip_prefix.as_deref(), std::slice::from_ref(pick))?;
                let src = install::safe_join(&tmp, pick).ok_or_else(|| err("fichier à extraire non sûr"))?;
                let name = src.file_name().map(|n| n.to_string_lossy().to_string()).ok_or_else(|| err("nom de fichier"))?;
                install::install_single(&src, &target, &name)?;
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&tmp);
            res?;
        } else {
            install::install_zip(&archive, &target, ver.strip_prefix.as_deref(), &ver.check)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&archive);
    result?;

    let rel = target.strip_prefix(root).unwrap_or(&target).to_string_lossy().replace('\\', "/");
    let rec = InstalledComponent {
        id: comp.id.clone(),
        version: r.resolved.clone(),
        dir: rel,
        sha256: r.sha256.clone(),
        source: r.url.clone(),
        installed_at: now(),
        origin: "download".into(),
    };
    let file = data.join("installed.json");
    let mut inst = Installed::load(&file);
    inst.upsert(rec.clone());
    inst.save(&file)?;
    Ok(Outcome::Installed(rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, Sha256Spec};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const PHP_JSON: &str = r#"{"8.3":{"version":"8.3.36-1","ts-vs16-x64":{"zip":{"path":"php-8.3.36-1-Win32-vs16-x64.zip","size":1,"sha256":"1111111111111111111111111111111111111111111111111111111111111111"}},"nts-vs16-x64":{"zip":{"path":"php-8.3.36-1-nts-Win32-vs16-x64.zip","size":1,"sha256":"2222222222222222222222222222222222222222222222222222222222222222"}}}}"#;

    #[test]
    fn php_releases_picks_variant() {
        let (p, s) = php_from_releases(PHP_JSON, "8.3.ts").unwrap();
        assert_eq!(p, "php-8.3.36-1-Win32-vs16-x64.zip");
        assert!(s.starts_with("1111"));
        let (p, _) = php_from_releases(PHP_JSON, "8.3.nts").unwrap();
        assert!(p.contains("-nts-"));
        assert!(php_from_releases(PHP_JSON, "8.9.ts").is_err());
        assert!(php_from_releases(r#"{"8.3":{"ts-vs16-x64":{"zip":{"path":"../x.zip","sha256":"zz"}}}}"#, "8.3.ts").is_err());
    }

    #[test]
    fn github_asset_needs_a_digest() {
        let ok = r#"{"tag_name":"v1.33.1","assets":[{"name":"mailpit-windows-amd64.zip","browser_download_url":"https://github.com/x/y.zip","digest":"sha256:0bcf8f1e7c21005b30c206064ff2f37dac602ef66774e558379d326f5673d652"}]}"#;
        let (u, s, t) = github_asset(ok, "mailpit-windows-amd64.zip").unwrap();
        assert_eq!((u.as_str(), t.as_str()), ("https://github.com/x/y.zip", "v1.33.1"));
        assert!(s.starts_with("0bcf"));
        assert!(github_asset(ok, "autre.zip").is_err());
        let none = r#"{"tag_name":"v1","assets":[{"name":"a.zip","browser_download_url":"https://github.com/a.zip","digest":null}]}"#;
        assert!(github_asset(none, "a.zip").is_err());
    }

    #[test]
    fn embedded_catalog_resolves_with_fake_network() {
        let cat = Catalog::embedded().unwrap();
        let get = |url: &str, _h: &BTreeMap<String, String>| -> Result<String, AppError> {
            Ok(if url.ends_with("releases.json") {
                PHP_JSON.to_string()
            } else if url.contains("composer.phar.sha256sum") {
                format!("{}  composer.phar", "a".repeat(64))
            } else {
                String::new()
            })
        };
        let php = cat.component("php").unwrap();
        let r = resolve(php, &php.versions[0], &get).unwrap();
        assert!(r.url.starts_with("https://downloads.php.net/~windows/releases/php-8.3.36-1-Win32"));
        assert_eq!(target_dir(Path::new("C:/astral-dev"), &php.versions[0], &r.resolved).unwrap(), PathBuf::from("C:/astral-dev").join("bin/php/php-8.3.36-1-Win32-vs16-x64"));
        let comp = cat.component("composer").unwrap();
        assert_eq!(resolve(comp, &comp.versions[0], &get).unwrap().sha256, "a".repeat(64));
        let redis = cat.component("redis").unwrap();
        assert!(resolve(redis, &redis.versions[0], &get).unwrap().sha256.starts_with("018ea18a"));
    }

    #[test]
    fn no_verifiable_sum_means_refusal() {
        let cat = Catalog::embedded().unwrap();
        let mut v = cat.component("redis").unwrap().versions[0].clone();
        v.sha256 = Sha256Spec { pinned: None, from: None, url: None };
        let get = |_: &str, _: &BTreeMap<String, String>| -> Result<String, AppError> { Ok(String::new()) };
        assert!(resolve(cat.component("redis").unwrap(), &v, &get).is_err());
    }

    fn serve(body: Vec<u8>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = l.accept() {
                    let mut b = [0u8; 2048];
                    let _ = s.read(&mut b);
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(&body);
                }
            }
        });
        port
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (n, c) in entries {
                z.start_file(*n, o).unwrap();
                z.write_all(c).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn ext_target_follows_installed_php() {
        let root = std::env::temp_dir().join(format!("astral-dev-extphp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["php-8.3.36-1-Win32-vs16-x64", "php-8.3.36-1-nts-Win32-vs16-x64", "php-8.4.27-1-nts-Win32-vs17-x64"] {
            std::fs::create_dir_all(root.join("bin/php").join(d)).unwrap();
        }
        assert_eq!(find_php_dir(&root, "8.3.ts").as_deref(), Some("php-8.3.36-1-Win32-vs16-x64"));
        assert_eq!(find_php_dir(&root, "8.3.nts").as_deref(), Some("php-8.3.36-1-nts-Win32-vs16-x64"));
        assert_eq!(find_php_dir(&root, "8.4.nts").as_deref(), Some("php-8.4.27-1-nts-Win32-vs17-x64"));
        assert_eq!(find_php_dir(&root, "8.5.nts"), None);
        let cat = Catalog::embedded().unwrap();
        let ext = cat.component("ext-redis").unwrap();
        assert_eq!(
            target_dir(&root, &ext.versions[0], &ext.versions[0].version).unwrap(),
            root.join("bin/php/php-8.3.36-1-Win32-vs16-x64/ext")
        );
        assert!(target_dir(&root, &ext.versions[2], &ext.versions[2].version).is_err(), "8.5 non installé");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn comp_ver(extract: &str, check: &[&str], pick: Option<&str>) -> (Component, Version) {
        let v = Version {
            version: "1.0".into(), variant: None, url: None, source: None, headers: BTreeMap::new(),
            sha256: Sha256Spec { pinned: None, from: None, url: None }, size: None, strip_prefix: None,
            extract_to: extract.into(), check: check.iter().map(|s| s.to_string()).collect(),
            single_file: None, pick_file: pick.map(|s| s.to_string()), for_php: None,
        };
        (Component { id: "t".into(), name: "t".into(), required: false, allowed_hosts: vec![], versions: vec![v.clone()] }, v)
    }

    #[test]
    fn full_install_flow_with_local_server() {
        let root = std::env::temp_dir().join(format!("astral-dev-comp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        let bytes = zip_bytes(&[("redis-server.exe", b"exe"), ("notes.txt", b"n")]);
        let sha = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let port = serve(bytes);
        let (comp, ver) = comp_ver("bin/redis/redis-{resolved}", &["redis-server.exe"], None);
        let r = Resolved { url: format!("http://127.0.0.1:{port}/r.zip"), sha256: sha.clone(), resolved: "1.0".into(), headers: BTreeMap::new() };
        let opt = Options { allow_local_http: true };
        let out = install(&root, &data, &comp, &ver, &opt, &AtomicBool::new(false), |_, _| {}, Some(r.clone())).unwrap();
        assert!(matches!(out, Outcome::Installed(_)));
        assert!(root.join("bin/redis/redis-1.0/redis-server.exe").is_file());
        assert!(!data.join("downloads").join("r.zip.part").exists());
        let inst = Installed::load(&data.join("installed.json"));
        assert_eq!(inst.components.len(), 1);
        assert_eq!(inst.components[0].sha256, sha);
        // 2e appel : déjà installé, aucun téléchargement
        let again = install(&root, &data, &comp, &ver, &opt, &AtomicBool::new(false), |_, _| {}, Some(r)).unwrap();
        assert!(matches!(again, Outcome::AlreadyInstalled(_)));
        // mauvaise somme : rien installé
        let port2 = serve(zip_bytes(&[("redis-server.exe", b"x")]));
        let (c2, v2) = comp_ver("bin/redis/bad", &["redis-server.exe"], None);
        let bad = Resolved { url: format!("http://127.0.0.1:{port2}/r.zip"), sha256: "0".repeat(64), resolved: "1.0".into(), headers: BTreeMap::new() };
        assert!(install(&root, &data, &c2, &v2, &opt, &AtomicBool::new(false), |_, _| {}, Some(bad)).is_err());
        assert!(!root.join("bin/redis/bad").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_file_copies_only_that_file() {
        let root = std::env::temp_dir().join(format!("astral-dev-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bytes = zip_bytes(&[("mod_fcgid.so", b"so"), ("ReadMe.txt", b"r")]);
        let sha = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let port = serve(bytes);
        let (comp, ver) = comp_ver("etc/apache2/modules", &[], Some("mod_fcgid.so"));
        let r = Resolved { url: format!("http://127.0.0.1:{port}/f.zip"), sha256: sha, resolved: "2.3.10".into(), headers: BTreeMap::new() };
        install(&root, &root.join("data"), &comp, &ver, &Options { allow_local_http: true }, &AtomicBool::new(false), |_, _| {}, Some(r)).unwrap();
        assert!(root.join("etc/apache2/modules/mod_fcgid.so").is_file());
        assert!(!root.join("etc/apache2/modules/ReadMe.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Réseau réel : `cargo test real_install -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_install_small_components() {
        let root = std::env::temp_dir().join("astral-dev-real-install");
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        let cat = Catalog::embedded().unwrap();
        for (id, ver) in [("composer", "latest-stable"), ("cacert", "latest"), ("mailpit", "latest"), ("php", "8.3")] {
            let c = cat.component(id).unwrap();
            let v = c.versions.iter().find(|v| v.version == ver).unwrap();
            let out = install(&root, &data, c, v, &Options { allow_local_http: false }, &AtomicBool::new(false), |_, _| {}, None);
            println!("{id} {ver} -> {out:?}");
            assert!(out.is_ok(), "{id}: {:?}", out.err());
        }
        assert!(root.join("bin/composer/composer.phar").is_file());
        assert!(root.join("etc/ssl/cacert.pem").is_file());
        println!("ROOT={}", root.display());
    }
}
