//! Fichier hosts : bloc balisé uniquement (F-020), sauvegarde (F-022), élévation à la demande (ADR-006).
use crate::error::AppError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const BEGIN: &str = "# astral-dev begin";
pub const END: &str = "# astral-dev end";

pub fn hosts_path() -> PathBuf {
    let win = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&win).join("System32").join("drivers").join("etc").join("hosts")
}

/// Le domaine apparaît-il déjà sur une ligne active (ex. entrées « laragon magic ») ?
pub fn has_domain(text: &str, domain: &str) -> bool {
    text.lines().any(|l| {
        let l = l.split('#').next().unwrap_or("");
        let mut it = l.split_whitespace();
        it.next().is_some() && it.any(|d| d.eq_ignore_ascii_case(domain))
    })
}

/// Domaine strict : labels [a-z0-9-] séparés par des points (anti-injection dans hosts et script).
pub fn valid_domain(d: &str) -> bool {
    d.len() <= 100
        && d.split('.').count() >= 2
        && d.split('.').all(|l| !l.is_empty() && l.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
}

/// Ajoute `domain` dans le bloc astral-dev. `None` si déjà présent (ailleurs ou dans le bloc).
/// Tout ce qui est hors du bloc reste inchangé octet pour octet.
pub fn add_domain(text: &str, domain: &str) -> Option<String> {
    if has_domain(text, domain) {
        return None;
    }
    let nl = if text.contains("\r\n") || text.is_empty() { "\r\n" } else { "\n" };
    let line = format!("127.0.0.1 {domain}");
    if let (Some(b), Some(e)) = (text.find(BEGIN), text.find(END)) {
        if b < e {
            let mut out = String::from(&text[..e]);
            if !out.ends_with('\n') {
                out.push_str(nl);
            }
            out.push_str(&line);
            out.push_str(nl);
            out.push_str(&text[e..]);
            return Some(out);
        }
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    out.push_str(&format!("{BEGIN}{nl}{line}{nl}{END}{nl}"));
    Some(out)
}

/// Retire `domain` du bloc astral-dev (jamais ailleurs). `None` si absent du bloc.
/// Le bloc vide est supprimé avec ses deux balises.
pub fn remove_domain(text: &str, domain: &str) -> Option<String> {
    let (b, e) = (text.find(BEGIN)?, text.find(END)?);
    if b >= e {
        return None;
    }
    let block_end = e + END.len();
    let block = &text[b..block_end];
    let mut found = false;
    let mut kept: Vec<&str> = Vec::new();
    for l in block.split_inclusive('\n') {
        let t = l.trim();
        let is_line = !t.starts_with('#') && {
            let mut it = t.split_whitespace();
            it.next().is_some() && it.next().map_or(false, |d| d.eq_ignore_ascii_case(domain)) && it.next().is_none()
        };
        if is_line {
            found = true;
        } else {
            kept.push(l);
        }
    }
    if !found {
        return None;
    }
    let empty = !kept.iter().any(|l| {
        let t = l.trim();
        !t.is_empty() && t != BEGIN && t != END
    });
    let mut out = String::from(&text[..b]);
    if !empty {
        out.push_str(&kept.concat());
    } else if text[block_end..].starts_with("\r\n") {
        return Some(format!("{out}{}", &text[block_end + 2..]));
    } else if text[block_end..].starts_with('\n') {
        return Some(format!("{out}{}", &text[block_end + 1..]));
    }
    out.push_str(&text[block_end..]);
    Some(out)
}

/// Écrit `new_text` dans hosts via un PowerShell élevé (UAC affiché à l'utilisateur).
/// Sauvegarde `hosts.astral-dev.bak` avant remplacement ; vérifie le résultat ensuite.
pub fn apply_elevated(new_text: &str, work_dir: &Path, domain: &str) -> Result<(), AppError> {
    apply_elevated_with(new_text, work_dir, domain, true)
}

/// `present` : le domaine doit figurer dans hosts après l'opération (ajout) ou ne plus y figurer (retrait).
pub fn apply_elevated_with(new_text: &str, work_dir: &Path, domain: &str, present: bool) -> Result<(), AppError> {
    if !valid_domain(domain) {
        return Err(AppError::Config(format!("domaine invalide : {domain}")));
    }
    std::fs::create_dir_all(work_dir)?;
    let staged = work_dir.join("hosts.new");
    let script = work_dir.join("apply-hosts.ps1");
    for p in [&staged, &script] {
        if p.to_string_lossy().contains('\'') {
            return Err(AppError::Config("chemin de travail avec apostrophe refusé".into()));
        }
    }
    std::fs::write(&staged, new_text)?;
    let hosts = hosts_path();
    std::fs::write(
        &script,
        format!(
            "$ErrorActionPreference='Stop'\r\nCopy-Item -LiteralPath '{h}' -Destination '{h}.astral-dev.bak' -Force\r\nCopy-Item -LiteralPath '{s}' -Destination '{h}' -Force\r\n",
            h = hosts.display(),
            s = staged.display()
        ),
    )?;
    let launcher = format!(
        "$p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{}'; exit $p.ExitCode",
        script.display()
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-Command", &launcher]).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
    let after = std::fs::read_to_string(&hosts).unwrap_or_default();
    if ok && has_domain(&after, domain) == present {
        Ok(())
    } else {
        Err(AppError::StartFailed("modification du fichier hosts non effectuée (élévation refusée ou échec)".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "# Copyright\r\n127.0.0.1 localhost\r\n127.0.0.1      astral-blog.local    #laragon magic!   \r\n";

    #[test]
    fn removes_only_inside_the_block() {
        let with = format!("{BASE}# astral-dev begin\r\n127.0.0.1 a.test\r\n127.0.0.1 b.test\r\n# astral-dev end\r\n");
        let out = remove_domain(&with, "a.test").unwrap();
        assert!(out.contains("b.test") && !out.contains("a.test") && out.starts_with(BASE));
        // dernier domaine : le bloc disparaît, le reste est identique
        let last = remove_domain(&out, "b.test").unwrap();
        assert_eq!(last, BASE);
        // domaine hors bloc (Laragon) : jamais touché
        assert!(remove_domain(&with, "astral-blog.local").is_none());
        assert!(remove_domain(BASE, "a.test").is_none());
    }

    #[test]
    fn detects_existing_domains_ignoring_comments() {
        assert!(has_domain(BASE, "astral-blog.local"));
        assert!(has_domain(BASE, "ASTRAL-BLOG.LOCAL"));
        assert!(!has_domain(BASE, "astral-shop.local"));
        assert!(!has_domain("# 127.0.0.1 x.local\r\n", "x.local"));
    }

    #[test]
    fn appends_a_marked_block_and_keeps_the_rest_intact() {
        let out = add_domain(BASE, "demo.local").unwrap();
        assert!(out.starts_with(BASE));
        assert!(out.ends_with("# astral-dev begin\r\n127.0.0.1 demo.local\r\n# astral-dev end\r\n"));
    }

    #[test]
    fn extends_existing_block_and_is_idempotent() {
        let one = add_domain(BASE, "a.local").unwrap();
        let two = add_domain(&one, "b.local").unwrap();
        assert_eq!(two.matches(BEGIN).count(), 1);
        assert!(two.contains("127.0.0.1 a.local\r\n127.0.0.1 b.local\r\n# astral-dev end"));
        assert!(add_domain(&two, "b.local").is_none());
        assert!(add_domain(BASE, "astral-blog.local").is_none()); // déjà géré par Laragon
    }

    #[test]
    fn domain_validation() {
        assert!(valid_domain("demo.local") && valid_domain("a-b.c1.local"));
        for bad in ["demo", "De.local", "a b.local", "x.local;rm", "a..local", "'x.local"] {
            assert!(!valid_domain(bad), "{bad}");
        }
    }
}