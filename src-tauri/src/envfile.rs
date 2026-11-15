//! Édition minimale d'un .env : remplace/ajoute des clés, laisse tout le reste intact (F-047).

/// Remplace la valeur des clés existantes (ligne active uniquement) ou les ajoute en fin de fichier.
/// Commentaires, ordre, fins de ligne et lignes inconnues sont conservés tels quels.
pub fn set_keys(text: &str, pairs: &[(&str, &str)]) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut done = vec![false; pairs.len()];
    let mut out = String::with_capacity(text.len() + 64);
    for raw in text.split_inclusive('\n') {
        let term = if raw.ends_with("\r\n") { "\r\n" } else if raw.ends_with('\n') { "\n" } else { "" };
        let body = &raw[..raw.len() - term.len()];
        let t = body.trim();
        let hit = if t.starts_with('#') {
            None
        } else {
            t.split_once('=').and_then(|(k, _)| pairs.iter().position(|(pk, _)| *pk == k.trim()))
        };
        match hit {
            Some(i) if !done[i] => {
                done[i] = true;
                out.push_str(&format!("{}={}{}", pairs[i].0, pairs[i].1, if term.is_empty() { "" } else { term }));
            }
            _ => out.push_str(raw),
        }
    }
    let missing: Vec<_> = pairs.iter().zip(&done).filter(|(_, d)| !**d).map(|(p, _)| p).collect();
    if !missing.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        for (k, v) in missing {
            out.push_str(&format!("{k}={v}{nl}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::env_value;

    #[test]
    fn replaces_in_place_and_keeps_everything_else() {
        let src = "APP_NAME=\"Astral Blog\"\r\n# CACHE_DRIVER=array\r\nCACHE_DRIVER=file\r\nOTHER=1\r\n";
        let out = set_keys(src, &[("CACHE_DRIVER", "redis")]);
        assert_eq!(out, "APP_NAME=\"Astral Blog\"\r\n# CACHE_DRIVER=array\r\nCACHE_DRIVER=redis\r\nOTHER=1\r\n");
    }

    #[test]
    fn appends_missing_keys_with_detected_line_endings() {
        let out = set_keys("A=1\nB=2", &[("REDIS_HOST", "127.0.0.1"), ("REDIS_PORT", "6379")]);
        assert_eq!(out, "A=1\nB=2\nREDIS_HOST=127.0.0.1\nREDIS_PORT=6379\n");
        assert_eq!(set_keys("", &[("X", "1")]), "X=1\n");
        assert_eq!(set_keys("A=1\r\n", &[("X", "1")]), "A=1\r\nX=1\r\n");
    }

    #[test]
    fn commented_key_is_not_touched_and_active_one_is_added() {
        let out = set_keys("# REDIS_PASSWORD=\n", &[("REDIS_PASSWORD", "")]);
        assert_eq!(out, "# REDIS_PASSWORD=\nREDIS_PASSWORD=\n");
    }

    #[test]
    fn idempotent() {
        let pairs = [("CACHE_DRIVER", "redis"), ("REDIS_PORT", "6379")];
        let once = set_keys("CACHE_DRIVER=file\n", &pairs);
        assert_eq!(set_keys(&once, &pairs), once);
        assert_eq!(env_value(&once, "CACHE_DRIVER").as_deref(), Some("redis"));
    }
}