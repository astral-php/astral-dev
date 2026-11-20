//! Téléchargement vérifié (v0.2, ADR-012) : HTTPS + liste blanche (redirections revérifiées),
//! reprise par `Range`, SHA-256 calculé à la volée. Rien n'est exécuté ici.
use crate::catalog::{host_allowed, is_hex64};
use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_REDIRECTS: u8 = 5;
const USER_AGENT: &str = concat!("astral-dev/", env!("CARGO_PKG_VERSION"));

pub struct Request<'a> {
    pub url: &'a str,
    pub headers: &'a BTreeMap<String, String>,
    pub allowed_hosts: &'a [String],
    /// Tests uniquement : autorise http://127.0.0.1 (jamais en production).
    pub allow_local_http: bool,
}

fn io(msg: impl Into<String>) -> AppError {
    AppError::Io(msg.into())
}

fn url_ok(r: &Request, url: &str) -> bool {
    if r.allow_local_http && url.starts_with("http://127.0.0.1") {
        return true;
    }
    host_allowed(url, r.allowed_hosts)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(900)))
        .timeout_connect(Some(Duration::from_secs(20)))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// Suit les redirections à la main : chaque hôte est revérifié.
fn open(r: &Request, from: u64) -> Result<(ureq::http::Response<ureq::Body>, u64), AppError> {
    let ag = agent();
    let mut url = r.url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !url_ok(r, &url) {
            return Err(io(format!("adresse refusée (hors liste blanche ou non HTTPS) : {url}")));
        }
        let mut req = ag.get(&url);
        for (k, v) in r.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if from > 0 {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let resp = req.call().map_err(|e| io(format!("téléchargement : {e}")))?;
        let st = resp.status().as_u16();
        if matches!(st, 301 | 302 | 303 | 307 | 308) {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io("redirection sans destination"))?;
            url = loc.to_string();
            continue;
        }
        if st == 200 || st == 206 {
            let offset = if st == 206 { from } else { 0 };
            return Ok((resp, offset));
        }
        return Err(io(format!("serveur : HTTP {st} pour {url}")));
    }
    Err(io("trop de redirections"))
}

/// Télécharge vers `dest` (fichier `.part` conservé en cas d'échec pour reprise) et renvoie le SHA-256.
/// `progress(octets_reçus, total_connu)`. `cancel` interrompt proprement.
pub fn fetch(
    r: &Request,
    dest: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<String, AppError> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let (resp, offset) = open(r, existing)?;
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|n| n + offset);

    let mut hasher = Sha256::new();
    let mut file = if offset > 0 {
        // reprise : la partie déjà reçue entre dans la somme
        let mut f = std::fs::File::open(dest)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut left = offset;
        while left > 0 {
            let lim = left.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..lim])?;
            if n == 0 {
                return Err(io("fichier partiel plus court que prévu"));
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
        OpenOptions::new().append(true).open(dest)?
    } else {
        std::fs::File::create(dest)?
    };

    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = offset;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(io("téléchargement annulé"));
        }
        let n = reader.read(&mut buf).map_err(|e| io(format!("lecture réseau : {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        got += n as u64;
        progress(got, total);
    }
    file.flush()?;
    if let Some(t) = total {
        if got != t {
            return Err(io(format!("téléchargement incomplet ({got}/{t} octets)")));
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// Petit document texte en mémoire (sommes, JSON de versions), borné à `max` octets.
pub fn fetch_text(r: &Request, max: usize) -> Result<String, AppError> {
    let (resp, _) = open(r, 0)?;
    let mut out = Vec::new();
    resp.into_body()
        .into_reader()
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| io(format!("lecture réseau : {e}")))?;
    if out.len() > max {
        return Err(io("document distant trop volumineux"));
    }
    String::from_utf8(out).map_err(|_| io("document distant non UTF-8"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// SHA-256 d'un fichier local.
#[allow(dead_code)] // utilitaire de vérification (tests, prochains lots)
pub fn sha256_file(path: &Path) -> Result<String, AppError> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Extrait la somme d'un fichier « sidecar » (`<64 hex>  nom` ou `<64 hex>` seul).
pub fn parse_sidecar(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?.to_ascii_lowercase();
    if is_hex64(&first) {
        Some(first)
    } else {
        None
    }
}

/// Compare la somme obtenue à celle attendue ; sur écart, le fichier est supprimé.
pub fn verify_or_delete(path: &Path, actual: &str, expected: &str) -> Result<(), AppError> {
    if actual.eq_ignore_ascii_case(expected) {
        return Ok(());
    }
    let _ = std::fs::remove_file(path);
    Err(io(format!("somme SHA-256 incorrecte : fichier supprimé (obtenu {actual}, attendu {expected})")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn serve_once(body: &'static [u8]) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut b = [0u8; 2048];
                let _ = s.read(&mut b);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            }
        });
        port
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("astral-dev-dl-{}-{name}", std::process::id()))
    }

    #[test]
    fn fetch_hashes_and_verifies() {
        let port = serve_once(b"bonjour");
        let url = format!("http://127.0.0.1:{port}/a.bin");
        let headers = BTreeMap::new();
        let req = Request { url: &url, headers: &headers, allowed_hosts: &[], allow_local_http: true };
        let dest = tmp("ok.bin");
        let _ = std::fs::remove_file(&dest);
        let sum = fetch(&req, &dest, &AtomicBool::new(false), |_, _| {}).unwrap();
        // sha256("bonjour")
        assert_eq!(sum, sha256_file(&dest).unwrap());
        assert!(verify_or_delete(&dest, &sum, &sum).is_ok());
        assert!(verify_or_delete(&dest, &sum, &"0".repeat(64)).is_err());
        assert!(!dest.exists(), "fichier à somme incorrecte supprimé");
    }

    #[test]
    fn refuses_foreign_and_plain_http_hosts() {
        let headers = BTreeMap::new();
        let allowed = vec!["curl.se".to_string()];
        for u in ["http://curl.se/ca.pem", "https://evil.example/ca.pem", "https://user@curl.se/x"] {
            let req = Request { url: u, headers: &headers, allowed_hosts: &allowed, allow_local_http: false };
            assert!(fetch(&req, &tmp("x.bin"), &AtomicBool::new(false), |_, _| {}).is_err(), "{u}");
        }
    }

    #[test]
    fn cancel_stops_download() {
        let port = serve_once(b"0123456789");
        let url = format!("http://127.0.0.1:{port}/a.bin");
        let headers = BTreeMap::new();
        let req = Request { url: &url, headers: &headers, allowed_hosts: &[], allow_local_http: true };
        let dest = tmp("cancel.bin");
        assert!(fetch(&req, &dest, &AtomicBool::new(true), |_, _| {}).is_err());
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn sidecar_parsing() {
        let h = "a".repeat(64);
        assert_eq!(parse_sidecar(&format!("{h}  composer.phar\n")), Some(h.clone()));
        assert_eq!(parse_sidecar(&h.to_uppercase()), Some(h));
        assert_eq!(parse_sidecar("<html>"), None);
    }
}
