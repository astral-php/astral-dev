//! Secrets locaux (F-050) : data/secrets/redis.pass, jamais dans astral-dev.toml, jamais journalisé.
use crate::error::AppError;
use std::path::Path;

const REDIS_FILE: &str = "redis.pass";

fn random_hex(bytes: usize) -> Result<String, AppError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| AppError::Io(format!("générateur aléatoire indisponible : {e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Lit le mot de passe Redis, le génère (32 octets → 64 hex) s'il n'existe pas.
pub fn redis_password(secrets_dir: &Path) -> Result<String, AppError> {
    let path = secrets_dir.join(REDIS_FILE);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let v = existing.trim().to_string();
        if v.len() >= 32 && v.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(v);
        }
    }
    std::fs::create_dir_all(secrets_dir)?;
    let pw = random_hex(32)?;
    std::fs::write(&path, &pw)?;
    restrict_to_user(&path);
    Ok(pw)
}

/// Retire l'héritage des droits et ne laisse l'accès qu'à l'utilisateur courant (Windows, icacls).
/// Meilleur effort : un échec n'empêche rien (poste personnel).
pub fn restrict_to_user(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(user) = std::env::var("USERNAME") {
            let _ = std::process::Command::new("icacls")
                .arg(path)
                .args(["/inheritance:r", "/grant:r"])
                .arg(format!("{user}:F"))
                .creation_flags(0x0800_0000)
                .output();
        }
    }
    #[cfg(not(windows))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_once_then_reuses() {
        let d = std::env::temp_dir().join(format!("astral-dev-sec-{}", std::process::id()));
        let a = redis_password(&d).unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(redis_password(&d).unwrap(), a);
        std::fs::write(d.join("redis.pass"), "court").unwrap(); // invalide → régénéré
        assert_ne!(redis_password(&d).unwrap(), "court");
        std::fs::remove_dir_all(&d).unwrap();
    }
}