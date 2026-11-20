//! Fichier hosts : bloc balisé uniquement (F-020), sauvegarde (F-022), élévation à la demande (ADR-006).
use crate::error::AppError;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};
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

/// Ajoute plusieurs domaines au bloc astral-dev. `None` si aucun n'a changé (tous déjà présents).
pub fn add_domains(text: &str, domains: &[&str]) -> Option<String> {
    let mut cur = text.to_string();
    let mut changed = false;
    for d in domains {
        if let Some(next) = add_domain(&cur, d) {
            cur = next;
            changed = true;
        }
    }
    changed.then_some(cur)
}

/// Retire plusieurs domaines du bloc astral-dev. `None` si aucun n'était présent dans le bloc.
pub fn remove_domains(text: &str, domains: &[&str]) -> Option<String> {
    let mut cur = text.to_string();
    let mut changed = false;
    for d in domains {
        if let Some(next) = remove_domain(&cur, d) {
            cur = next;
            changed = true;
        }
    }
    changed.then_some(cur)
}

// ---------- élévation (ADR-018) ----------

/// Codes de sortie du script élevé (en plus de 0 = succès et 1 = erreur PowerShell ou UAC refusé).
const EXIT_HOSTS_CHANGED: i32 = 10;
const EXIT_STAGED_ALTERED: i32 = 11;

/// SHA-256 en hexadécimal minuscule.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Windows PowerShell par chemin absolu : un processus élevé ne doit jamais être résolu via PATH.
fn powershell_exe() -> PathBuf {
    let win = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&win).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe")
}

/// Littéral PowerShell entre apostrophes. Refuse tout ce qui pourrait en sortir : PowerShell traite aussi
/// les apostrophes typographiques (U+2018 à U+201B) comme des apostrophes, et un chemin n'a rien à faire
/// avec des caractères de contrôle.
fn ps_literal(s: &str) -> Result<String, AppError> {
    if s.chars().any(|c| c.is_control() || matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')) {
        return Err(AppError::Config(format!("chemin refusé (caractère non autorisé) : {s}")));
    }
    Ok(format!("'{s}'"))
}

/// Script exécuté avec élévation. Il est passé en ligne de commande (`-EncodedCommand`), donc figé à la
/// création du processus : aucun fichier de script n'est lu. Les deux fichiers qu'il lit sont épinglés par
/// leur SHA-256 et le contenu vérifié est exactement celui qui est écrit (lu une seule fois en mémoire),
/// ce qui supprime la fenêtre « vérifier puis utiliser ».
fn elevated_script(hosts: &Path, staged: &Path, current_sha: &str, new_sha: &str) -> Result<String, AppError> {
    let hosts = ps_literal(&hosts.to_string_lossy())?;
    let staged = ps_literal(&staged.to_string_lossy())?;
    Ok(format!(
        r#"$ErrorActionPreference = 'Stop'
$hosts = {hosts}
$staged = {staged}
$sha = [System.Security.Cryptography.SHA256]::Create()
function Get-Hex([byte[]]$b) {{ ([BitConverter]::ToString($sha.ComputeHash($b)) -replace '-', '').ToLowerInvariant() }}
$cur = [System.IO.File]::ReadAllBytes($hosts)
if ((Get-Hex $cur) -ne '{current_sha}') {{ exit {EXIT_HOSTS_CHANGED} }}
$new = [System.IO.File]::ReadAllBytes($staged)
if ((Get-Hex $new) -ne '{new_sha}') {{ exit {EXIT_STAGED_ALTERED} }}
[System.IO.File]::WriteAllBytes($hosts + '.astral-dev.bak', $cur)
[System.IO.File]::WriteAllBytes($hosts, $new)
exit 0
"#
    ))
}

/// Format attendu par `powershell -EncodedCommand` : UTF-16 little-endian puis Base64.
fn encode_command(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(utf16)
}

/// Commande (non élevée) qui demande l'UAC et relaie le code de sortie du processus élevé.
fn launcher(ps: &Path, encoded: &str) -> Result<String, AppError> {
    let ps = ps_literal(&ps.to_string_lossy())?;
    Ok(format!(
        "$p = Start-Process -FilePath {ps} -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $p.ExitCode"
    ))
}

fn explain_exit(code: Option<i32>) -> String {
    match code {
        Some(EXIT_HOSTS_CHANGED) => "le fichier hosts a changé pendant l'opération : rien n'a été écrit, réessayez".into(),
        Some(EXIT_STAGED_ALTERED) => "fichier intermédiaire modifié par un autre processus : opération annulée par sécurité".into(),
        _ => "modification du fichier hosts non effectuée (élévation refusée ou échec)".into(),
    }
}

fn run_elevated(hosts: &Path, staged: &Path, current_sha: &str, new_sha: &str) -> Result<Option<i32>, AppError> {
    let ps = powershell_exe();
    let encoded = encode_command(&elevated_script(hosts, staged, current_sha, new_sha)?);
    let launcher = launcher(&ps, &encoded)?;
    let mut cmd = Command::new(&ps);
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &launcher]).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    Ok(cmd.status().ok().and_then(|s| s.code()))
}

/// Remplace `hosts` par `new_text` via un PowerShell élevé (une seule invite UAC pour tout le lot de `domains`).
/// `current` = octets de `hosts` tels que lus pour calculer `new_text` : si le fichier a changé entre-temps,
/// rien n'est écrit. `present` : les domaines doivent figurer dans `hosts` après l'opération (ajout) ou ne plus y
/// figurer (retrait). Sauvegarde `hosts.astral-dev.bak` avant remplacement ; vérifie le résultat ensuite.
pub fn apply_elevated(current: &[u8], new_text: &str, work_dir: &Path, domains: &[&str], present: bool) -> Result<(), AppError> {
    if domains.is_empty() {
        return Ok(());
    }
    if let Some(bad) = domains.iter().find(|d| !valid_domain(d)) {
        return Err(AppError::Config(format!("domaine invalide : {bad}")));
    }
    let staged = work_dir.join(format!("hosts.{}.new", std::process::id()));
    // Les chemins sont validés avant toute écriture (même la création du dossier) et avant toute invite UAC.
    let hosts = hosts_path();
    elevated_script(&hosts, &staged, "0", "0")?;
    std::fs::create_dir_all(work_dir)?;
    std::fs::write(&staged, new_text)?;
    let code = run_elevated(&hosts, &staged, &sha256_hex(current), &sha256_hex(new_text.as_bytes()));
    let _ = std::fs::remove_file(&staged);
    match code? {
        Some(0) => {}
        other => return Err(AppError::StartFailed(explain_exit(other))),
    }
    let after = std::fs::read_to_string(&hosts).unwrap_or_default();
    if domains.iter().all(|d| has_domain(&after, d) == present) {
        Ok(())
    } else {
        Err(AppError::StartFailed("modification du fichier hosts non vérifiée après écriture".into()))
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

#[cfg(test)]
mod elevation_tests {
    use super::*;

    const HOSTS: &str = "C:\\Windows\\System32\\drivers\\etc\\hosts";

    #[test]
    fn sha256_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn encoded_command_matches_powershell_format() {
        // Références calculées indépendamment : base64(utf-16le(texte)).
        assert_eq!(encode_command("exit 7"), "ZQB4AGkAdAAgADcA");
        assert_eq!(encode_command("\u{e9}"), "6QA=");
    }

    #[test]
    fn encoded_command_round_trips_the_whole_script() {
        let script = elevated_script(Path::new(HOSTS), Path::new("D:\\data dir\\tmp\\hosts.1.new"), &sha256_hex(b"a"), &sha256_hex(b"b")).unwrap();
        let raw = STANDARD.decode(encode_command(&script)).unwrap();
        let units: Vec<u16> = raw.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), script);
    }

    #[test]
    fn script_pins_both_files_by_hash_and_runs_no_script_file() {
        let cur = sha256_hex(b"avant");
        let new = sha256_hex(b"apres");
        let s = elevated_script(Path::new(HOSTS), Path::new("D:\\d\\hosts.1.new"), &cur, &new).unwrap();
        assert!(s.contains(&format!("-ne '{cur}'")) && s.contains(&format!("-ne '{new}'")));
        assert!(s.contains("exit 10") && s.contains("exit 11"));
        assert!(s.contains("$hosts = 'C:\\Windows\\System32\\drivers\\etc\\hosts'"));
        // le fichier intermédiaire n'est lu qu'une fois, vérifié, puis écrit tel quel
        assert_eq!(s.matches("ReadAllBytes($staged)").count(), 1);
        assert!(s.find("-ne '").unwrap() < s.find("WriteAllBytes").unwrap());
        assert!(!s.contains("-File") && !s.contains("Invoke-Expression") && !s.contains("iex "));
    }

    #[test]
    fn launcher_uses_absolute_powershell_and_encoded_command_only() {
        let l = launcher(Path::new("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"), "QUJD").unwrap();
        assert!(l.starts_with("$p = Start-Process -FilePath 'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe'"));
        assert!(l.contains("-Verb RunAs") && l.contains("'-EncodedCommand','QUJD'"));
        assert!(!l.contains("'-File'") && !l.contains("Bypass"));
        assert!(l.ends_with("exit $p.ExitCode"));
    }

    #[test]
    fn powershell_is_addressed_by_full_path() {
        let p = powershell_exe();
        assert_eq!(p.file_name().unwrap(), "powershell.exe");
        assert!(p.to_string_lossy().contains("WindowsPowerShell"));
        assert!(p.components().count() > 4);
    }

    #[test]
    fn literals_reject_anything_that_could_escape_the_quotes() {
        assert!(ps_literal("D:\\mon dossier\\tmp\\hosts.1.new").is_ok());
        for bad in ["a'b", "a\u{2018}b", "a\u{2019}b", "a\u{201A}b", "a\u{201B}b", "a\nb", "a\rb", "a\0b"] {
            assert!(ps_literal(bad).is_err(), "{bad:?}");
        }
        assert!(elevated_script(Path::new("C:\\h"), Path::new("D:\\o'brien\\x"), "a", "b").is_err());
    }

    #[test]
    fn exit_codes_are_explained() {
        assert!(explain_exit(Some(10)).contains("a chang\u{e9}"));
        assert!(explain_exit(Some(11)).contains("interm\u{e9}diaire"));
        assert!(explain_exit(Some(1)).contains("\u{e9}l\u{e9}vation refus\u{e9}e"));
        assert!(explain_exit(None).contains("\u{e9}l\u{e9}vation refus\u{e9}e"));
    }

    #[test]
    fn invalid_domain_is_refused_before_anything_is_written() {
        let t = std::env::temp_dir().join(format!("astral-dev-el-{}", std::process::id()));
        assert!(apply_elevated(b"x", "y", &t, &["x.local;rm"], true).is_err());
        assert!(apply_elevated(b"x", "y", &t, &["ok.test", "a b.test"], false).is_err());
        assert!(!t.exists());
    }

    #[test]
    fn empty_batch_does_nothing() {
        let t = std::env::temp_dir().join(format!("astral-dev-el0-{}", std::process::id()));
        assert!(apply_elevated(b"x", "y", &t, &[], true).is_ok());
        assert!(!t.exists());
    }

    #[test]
    fn batches_add_and_remove_several_domains_in_one_text() {
        let base = "127.0.0.1 localhost\r\n";
        let added = add_domains(base, &["a.test", "b.test", "c.test"]).unwrap();
        assert!(added.starts_with(base));
        assert_eq!(added.matches("# astral-dev begin").count(), 1, "un seul bloc balisé");
        assert!(added.contains("127.0.0.1 a.test") && added.contains("127.0.0.1 b.test") && added.contains("127.0.0.1 c.test"));
        assert!(add_domains(&added, &["a.test", "b.test"]).is_none(), "déjà présents : aucun changement, donc aucune invite UAC");
        let some = remove_domains(&added, &["a.test", "c.test"]).unwrap();
        assert!(some.contains("b.test") && !some.contains("a.test") && !some.contains("c.test"));
        let none = remove_domains(&some, &["b.test", "absent.test"]).unwrap();
        assert_eq!(none, base, "le dernier domaine retiré supprime le bloc ; le reste est identique");
        assert!(remove_domains(base, &["a.test"]).is_none());
    }

    #[test]
    fn staged_name_never_collides_with_the_hosts_file() {
        // le fichier intermédiaire est propre au processus : hosts.<pid>.new
        let name = format!("hosts.{}.new", std::process::id());
        assert!(name.starts_with("hosts.") && name.ends_with(".new"));
    }

    /// Exécute RÉELLEMENT le script généré (sans élévation, sur des fichiers temporaires) : codes 0, 10, 11.
    #[cfg(windows)]
    #[test]
    fn generated_script_runs_without_elevation_and_returns_expected_codes() {
        let dir = std::env::temp_dir().join(format!("astral-dev-hosts-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hosts = dir.join("hosts");
        let staged = dir.join("hosts.1.new");
        let run = |cur: &[u8], new: &[u8]| -> i32 {
            let script = elevated_script(&hosts, &staged, &sha256_hex(cur), &sha256_hex(new)).unwrap();
            Command::new(powershell_exe())
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_command(&script)])
                .status()
                .unwrap()
                .code()
                .unwrap()
        };
        let orig = b"127.0.0.1 localhost
";
        let newb = b"127.0.0.1 localhost
# astral-dev begin
127.0.0.1 a.test
# astral-dev end
";
        // nominal
        std::fs::write(&hosts, orig).unwrap();
        std::fs::write(&staged, newb).unwrap();
        assert_eq!(run(orig, newb), 0);
        assert_eq!(std::fs::read(&hosts).unwrap(), newb);
        assert_eq!(std::fs::read(dir.join("hosts.astral-dev.bak")).unwrap(), orig);
        // hosts modifié entre le calcul et l'écriture : rien n'est écrit
        std::fs::write(&hosts, b"127.0.0.1 localhost
127.0.0.1 manuel.test
").unwrap();
        assert_eq!(run(orig, newb), EXIT_HOSTS_CHANGED);
        assert!(String::from_utf8_lossy(&std::fs::read(&hosts).unwrap()).contains("manuel.test"));
        // fichier intermédiaire altéré
        std::fs::write(&hosts, orig).unwrap();
        std::fs::write(&staged, b"127.0.0.1 evil.test
").unwrap();
        assert_eq!(run(orig, newb), EXIT_STAGED_ALTERED);
        assert_eq!(std::fs::read(&hosts).unwrap(), orig);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Chemins de travail dangereux : refus explicite AVANT toute écriture et toute invite UAC (T58).
    #[test]
    fn apostrophe_in_work_dir_is_refused_before_any_write_or_uac() {
        let t = std::env::temp_dir().join(format!("astral-dev-o'brien-{}", std::process::id()));
        let err = apply_elevated(b"x", "y", &t, &["ok.test"], true).unwrap_err().to_string();
        assert!(err.contains("chemin refusé"), "{err}");
        assert!(!t.exists(), "aucun dossier ni fichier intermédiaire créé");
    }

    /// Un chemin AVEC espace fonctionne de bout en bout (exécution réelle du script, sans élévation) (T58).
    #[cfg(windows)]
    #[test]
    fn generated_script_handles_a_path_with_spaces() {
        let dir = std::env::temp_dir().join(format!("astral dev espace {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hosts = dir.join("mon hosts");
        let staged = dir.join("hosts 1.new");
        let orig = b"127.0.0.1 localhost\r\n";
        let newb = b"127.0.0.1 localhost\r\n127.0.0.1 a.test\r\n";
        std::fs::write(&hosts, orig).unwrap();
        std::fs::write(&staged, newb).unwrap();
        let script = elevated_script(&hosts, &staged, &sha256_hex(orig), &sha256_hex(newb)).unwrap();
        let code = Command::new(powershell_exe())
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encode_command(&script)])
            .status()
            .unwrap()
            .code();
        assert_eq!(code, Some(0));
        assert_eq!(std::fs::read(&hosts).unwrap(), newb);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
