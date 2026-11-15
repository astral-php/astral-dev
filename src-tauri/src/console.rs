//! Exécution de processus avec sortie en flux (F-013) : `php bin/console …` et Composer.
use crate::error::AppError;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Sous-commandes autorisées depuis l'UI (SECURITE.md : liste blanche).
pub const ALLOWED: [&str; 7] =
    ["list", "help", "migrate", "migrate:status", "migrate:rollback", "cache:clear", "make:module"];

/// Sous-commande en liste blanche ; arguments limités à [A-Za-z0-9:_.=-] (jamais de shell de toute façon).
pub fn check_args(args: &[String]) -> Result<(), AppError> {
    let Some(first) = args.first() else {
        return Err(AppError::Io("commande vide".into()));
    };
    if !ALLOWED.contains(&first.as_str()) {
        return Err(AppError::NotStartable(format!("commande non autorisée : {first}")));
    }
    if args.len() > 8 {
        return Err(AppError::Io("trop d'arguments".into()));
    }
    for a in args {
        if a.is_empty() || a.len() > 80 || !a.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '=' | '-')) {
            return Err(AppError::Io(format!("argument refusé : {a}")));
        }
    }
    Ok(())
}

pub fn pump<R: Read + Send + 'static>(r: R, err: bool, tx: mpsc::Sender<(bool, String)>) {
    std::thread::spawn(move || {
        let mut br = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match br.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    if tx.send((err, line)).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Lance `program args` dans `cwd`, appelle `on_line(is_stderr, ligne)` en direct, renvoie le code de sortie.
/// `path_prepend` est ajouté au PATH du processus (pour que `php` soit trouvable par les scripts Composer).
pub fn run<F: FnMut(bool, String)>(
    program: &Path,
    args: &[String],
    cwd: &Path,
    path_prepend: Option<&Path>,
    mut on_line: F,
) -> Result<i32, AppError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("COMPOSER_NO_INTERACTION", "1");
    if let Some(p) = path_prepend {
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![p.to_path_buf()];
        paths.extend(std::env::split_paths(&old));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::StartFailed(format!("lancement impossible : {} ({e})", program.display())))?;
    let (tx, rx) = mpsc::channel();
    if let Some(o) = child.stdout.take() {
        pump(o, false, tx.clone());
    }
    if let Some(e) = child.stderr.take() {
        pump(e, true, tx.clone());
    }
    drop(tx);
    for (is_err, line) in rx {
        on_line(is_err, line);
    }
    Ok(child.wait()?.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn whitelist_and_argument_rules() {
        assert!(check_args(&a(&["list"])).is_ok());
        assert!(check_args(&a(&["make:module", "Article", "--api", "--no-migrate"])).is_ok());
        assert!(check_args(&a(&[])).is_err());
        assert!(check_args(&a(&["rm"])).is_err());
        assert!(check_args(&a(&["migrate", "x; del *"])).is_err());
        assert!(check_args(&a(&["migrate", "a b"])).is_err());
        assert!(check_args(&a(&["list", "$(x)"])).is_err());
    }

    #[test]
    fn streams_output_and_exit_code() {
        let mut lines = vec![];
        let code = run(
            Path::new("cmd.exe"),
            &a(&["/C", "echo bonjour & echo oups 1>&2 & exit 3"]),
            &std::env::temp_dir(),
            None,
            |e, l| lines.push((e, l)),
        )
        .unwrap();
        assert_eq!(code, 3);
        assert!(lines.iter().any(|(e, l)| !e && l.trim() == "bonjour"));
        assert!(lines.iter().any(|(e, l)| *e && l.trim() == "oups"));
    }

    /// Réel : `php bin/console list` sur astral-starter (lecture seule). `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_astral_console_list() {
        use crate::{config::Config, paths::AppPaths};
        let paths = AppPaths::from_config(&Config::default());
        let php = paths.php_exe().expect("php.exe");
        let mut out = vec![];
        let code = run(&php, &a(&["bin/console", "list"]), &paths.www.join("astral-starter"), paths.active_php().as_deref(), |_, l| out.push(l)).unwrap();
        assert_eq!(code, 0, "{out:?}");
        assert!(out.iter().any(|l| l.contains("migrate")), "{out:?}");
    }

    /// Réel + réseau (Packagist) : crée astral-starter dans un dossier temporaire.
    #[test]
    #[ignore]
    fn real_composer_create_project() {
        use crate::{config::Config, paths::AppPaths};
        let paths = AppPaths::from_config(&Config::default());
        let php = paths.php_exe().expect("php.exe");
        let tmp = std::env::temp_dir().join(format!("astral-dev-create-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("demo-app");
        let args = a(&[
            &paths.composer_phar().to_string_lossy(), "create-project", "astral-php/astral-starter",
            &target.to_string_lossy(), "--no-interaction", "--no-progress", "--prefer-dist",
        ]);
        let mut out = vec![];
        let code = run(&php, &args, &tmp, paths.active_php().as_deref(), |_, l| out.push(l)).unwrap();
        assert_eq!(code, 0, "{}", out.join("\n"));
        assert!(target.join("bin/console").is_file() && target.join("composer.json").is_file());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(run(Path::new("Z:\\nope.exe"), &[], &std::env::temp_dir(), None, |_, _| {}).is_err());
    }
}