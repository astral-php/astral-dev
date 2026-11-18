mod catalog;
mod components;
mod config;
mod download;
mod install;
mod cache_setup;
mod console;
mod envfile;
mod error;
mod hosts;
mod mkcert;
mod projects;
mod provision;
mod vhosts;
mod watcher;
mod paths;
mod probe;
mod secrets;
mod redis_client;
mod services;
mod supervisor;

use config::Config;
use error::AppError;
use paths::AppPaths;
use redis_client::{KeyDetail, RedisInfo, ScanPage, Target};
use std::sync::Arc;
use std::time::Duration;
use supervisor::{OpResult, ServiceStatus, Supervisor};
use tauri::{Emitter, Manager, State};

struct RedisTarget(Target);

/// Contexte applicatif partagé (config + chemins + compteur de tâches).
struct Ctx {
    cfg: Config,
    paths: AppPaths,
    jobs: std::sync::atomic::AtomicU64,
    /// v0.2 : une seule installation de composant à la fois ; annulation demandée par l'UI.
    installing: std::sync::atomic::AtomicBool,
    install_cancel: std::sync::atomic::AtomicBool,
}

#[derive(serde::Serialize)]
struct ExposeResult {
    vhost: &'static str,
    hosts: &'static str,
    /// "mkcert" | "laragon" | "aucun"
    ssl: &'static str,
    apache_restart_needed: bool,
}

/// Lance un processus en tâche de fond ; émet `console-line` / `console-done`. Retourne l'id de tâche.
fn spawn_job(
    app: tauri::AppHandle,
    ctx: &Arc<Ctx>,
    program: std::path::PathBuf,
    args: Vec<String>,
    cwd: std::path::PathBuf,
) -> u64 {
    let id = ctx.jobs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let php_dir = ctx.paths.active_php();
    std::thread::spawn(move || {
        let res = console::run(&program, &args, &cwd, php_dir.as_deref(), |err, line| {
            let _ = app.emit("console-line", serde_json::json!({ "job": id, "stderr": err, "line": line }));
        });
        let payload = match res {
            Ok(code) => serde_json::json!({ "job": id, "code": code }),
            Err(e) => serde_json::json!({ "job": id, "code": -1, "error": e.to_string() }),
        };
        let _ = app.emit("console-done", payload);
    });
    id
}

#[tauri::command]
async fn cache_status(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<cache_setup::CacheStatus, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        Ok(cache_setup::status(&dir, c.paths.php_exe().as_deref(), c.cfg.redis.port))
    })
    .await
}

/// F-047 : active Redis pour un projet (écrit le .env après contrôles). `cache:clear` est lancé ensuite par l'UI.
#[tauri::command]
async fn cache_enable(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<(), AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        let s = cache_setup::status(&dir, c.paths.php_exe().as_deref(), c.cfg.redis.port);
        let pw = services::redis_secret(&c.cfg, &c.paths).unwrap_or_default();
        cache_setup::enable(&dir, &s, &pw)
    })
    .await
}

#[tauri::command]
async fn cache_disable(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || cache_setup::disable(&projects::project_dir(&c.paths.www, &project)?)).await
}

/// `composer require astral-php/astral-cache` (confirmation côté UI). L'enregistrement du provider reste manuel.
#[tauri::command]
fn add_cache_package(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, project: String) -> Result<u64, AppError> {
    let dir = projects::project_dir(&ctx.paths.www, &project)?;
    if !dir.join("composer.json").is_file() {
        return Err(AppError::NotStartable("composer.json introuvable".into()));
    }
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let args = vec![
        ctx.paths.composer_phar().to_string_lossy().to_string(),
        "require".into(),
        "astral-php/astral-cache".into(),
        "--no-interaction".into(),
        "--no-progress".into(),
    ];
    Ok(spawn_job(app, ctx.inner(), php, args, dir))
}

/// ADR-011 : l'application a-t-elle tous ses binaires ? (sinon l'interface propose l'import depuis Laragon)
#[tauri::command]
fn setup_status(ctx: State<'_, Arc<Ctx>>) -> provision::SetupStatus {
    provision::status(&ctx.cfg, &ctx.paths)
}

/// Copie les binaires depuis Laragon (tâche de fond). Événements : `import-line`, `import-done`.
#[tauri::command]
fn import_laragon(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>) -> Result<(), AppError> {
    if !ctx.paths.standalone {
        return Err(AppError::NotStartable("l'import ne concerne que le mode standalone".into()));
    }
    let src = provision::find_laragon().ok_or_else(|| AppError::BinaryNotFound("Laragon introuvable (D:/laragon, C:/laragon)".into()))?;
    let c = ctx.inner().clone();
    std::thread::spawn(move || {
        let h = app.clone();
        let res = provision::import_from_laragon(&src, &c.paths, &provision::PHP_VERSIONS, &|l| {
            let _ = h.emit("import-line", l);
        });
        let payload = match res {
            Ok(done) => serde_json::json!({ "ok": true, "done": done }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        let _ = app.emit("import-done", payload);
    });
    Ok(())
}

/// Lance une nouvelle instance après la fin de ce processus puis quitte (voir `restart_app`).
fn relaunch_and_exit(app: &tauri::AppHandle, logs: std::path::PathBuf) -> Result<(), AppError> {
    let exe = std::env::current_exe()?;
    let exe_s = exe.display().to_string();
    let log = logs.join("restart.log");
    let log_s = log.display().to_string();
    if exe_s.contains('\'') || log_s.contains('\'') {
        return Err(AppError::Config("chemin avec apostrophe : relance automatique impossible".into()));
    }
    let _ = std::fs::create_dir_all(logs);
    let pid = std::process::id();
    // La nouvelle instance ne démarre qu'une fois CE processus terminé (instance unique) ; journal pour diagnostic.
    let script = format!(
        "Add-Content -LiteralPath '{log_s}' (\"[\" + (Get-Date -Format s) + \"] attente de la fin du processus {pid}\"); \
         Wait-Process -Id {pid} -Timeout 60 -ErrorAction SilentlyContinue; \
         Add-Content -LiteralPath '{log_s}' (\"[\" + (Get-Date -Format s) + \"] relance\"); \
         Start-Process -FilePath '{exe_s}'"
    );
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP (DETACHED_PROCESS empêche PowerShell de démarrer)
    }
    c.spawn()?;
    // Sortie forcée si l'arrêt propre traîne : sans cela l'instance unique bloquerait la relance.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(4));
        std::process::exit(0);
    });
    app.exit(0);
    Ok(())
}

/// Relance l'application : arrête d'abord les services (sinon ils restent orphelins), puis lance une nouvelle
/// instance détachée après une courte pause (l'instance unique empêche un démarrage simultané) et quitte.
#[tauri::command]
async fn restart_app(app: tauri::AppHandle, sup: Sup<'_>, _ctx: State<'_, Arc<Ctx>>) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || {
        s.stop_all();
        Ok(())
    })
    .await?;
    relaunch_and_exit(&app, _ctx.paths.logs())
}

/// v0.2 (ADR-012) : composants du catalogue et état d'installation.
#[tauri::command]
fn components_list(ctx: State<'_, Arc<Ctx>>) -> Result<serde_json::Value, AppError> {
    let cat = catalog::Catalog::embedded()?;
    let inst = install::Installed::load(&ctx.paths.data.join("installed.json"));
    let list: Vec<_> = cat
        .components
        .iter()
        .map(|c| {
            let versions: Vec<_> = c
                .versions
                .iter()
                .map(|v| {
                    serde_json::json!({
                        "version": v.version,
                        "variant": v.variant,
                        "installed": components::is_installed(&ctx.paths, &inst, c, v),
                        "size": v.size,
                    })
                })
                .collect();
            serde_json::json!({ "id": c.id, "name": c.name, "required": c.required, "versions": versions })
        })
        .collect();
    Ok(serde_json::json!({ "components": list, "installed": inst.components }))
}

/// Supprime un composant installé (dossier de la version ; données MySQL et projets jamais touchés).
#[tauri::command]
async fn remove_component(ctx: State<'_, Arc<Ctx>>, sup: Sup<'_>, id: String, version: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    let s = sup.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("la suppression de composants concerne le mode standalone".into()));
        }
        if c.installing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::NotStartable("une installation est en cours".into()));
        }
        let cat = catalog::Catalog::embedded()?;
        let comp = cat.component(&id).ok_or_else(|| AppError::Config(format!("composant inconnu : {id}")))?;
        let ver = comp.versions.iter().find(|v| v.version == version).ok_or_else(|| AppError::Config(format!("version inconnue : {id} {version}")))?;
        // Un composant utilisé par un service en cours ne se supprime pas.
        let service = match id.as_str() {
            "apache" | "mod_fcgid" | "php" | "ext-redis" => Some("apache"),
            "mysql" => Some("mysql"),
            "redis" => Some("redis"),
            "mailpit" => Some("mailpit"),
            _ => None,
        };
        if let Some(sid) = service {
            if s.statuses().iter().any(|st| st.id == sid && st.state == "running") {
                return Err(AppError::NotStartable(format!("arrêtez d'abord le service « {sid} » (Tout arrêter), puis supprimez le composant")));
            }
        }
        let file = c.paths.data.join("installed.json");
        let mut inst = install::Installed::load(&file);
        let removed = components::remove(&c.paths, &mut inst, comp, ver)?;
        inst.save(&file)?;
        Ok(removed)
    })
    .await
}

/// Télécharge, vérifie (SHA-256) et installe un composant. Événements : `component-progress`, `component-done`.
#[tauri::command]
fn install_component(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, id: String, version: String) -> Result<(), AppError> {
    use std::sync::atomic::Ordering;
    if !ctx.paths.standalone {
        return Err(AppError::NotStartable("l'installation de composants concerne le mode standalone".into()));
    }
    let cat = catalog::Catalog::embedded()?;
    let comp = cat.component(&id).cloned().ok_or_else(|| AppError::Config(format!("composant inconnu : {id}")))?;
    let ver = comp.versions.iter().find(|v| v.version == version).cloned().ok_or_else(|| AppError::Config(format!("version inconnue : {id} {version}")))?;
    if ctx.installing.swap(true, Ordering::SeqCst) {
        return Err(AppError::NotStartable("une installation est déjà en cours".into()));
    }
    ctx.install_cancel.store(false, Ordering::SeqCst);
    let c = ctx.inner().clone();
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let (pid, pver) = (comp.id.clone(), ver.version.clone());
        let res = components::install(
            &c.paths.root,
            &c.paths.data,
            &comp,
            &ver,
            &components::Options { allow_local_http: false },
            &c.install_cancel,
            |got, total| {
                if last.elapsed() >= std::time::Duration::from_millis(250) {
                    last = std::time::Instant::now();
                    let _ = app.emit("component-progress", serde_json::json!({ "id": pid, "version": pver, "received": got, "total": total }));
                }
            },
            None,
        );
        let payload = match res {
            Ok(out) => {
                if comp.id == "php" || comp.id == "ext-redis" {
                    for dir in c.paths.php_dirs() {
                        let _ = provision::write_php_ini(&dir, &c.paths.ssl_dir().join("cacert.pem"));
                    }
                }
                let note = match out {
                    components::Outcome::Installed(r) => format!("installé : {} {}", r.id, r.version),
                    components::Outcome::AlreadyInstalled(p) => format!("déjà installé : {}", p.display()),
                };
                serde_json::json!({ "id": comp.id, "version": ver.version, "ok": true, "note": note })
            }
            Err(e) => serde_json::json!({ "id": comp.id, "version": ver.version, "ok": false, "error": e.to_string() }),
        };
        c.installing.store(false, Ordering::SeqCst);
        let _ = app.emit("component-done", payload);
    });
    Ok(())
}

#[tauri::command]
fn cancel_install(ctx: State<'_, Arc<Ctx>>) {
    ctx.install_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[derive(serde::Serialize)]
struct UnexposeResult {
    vhost: &'static str,
    hosts: &'static str,
    files: &'static str,
    apache_restart_needed: bool,
}

/// Retire l'exposition d'un projet (vhost, certificat mkcert, ligne hosts du bloc astral-dev).
/// Les fichiers du projet ne sont supprimés que si `delete_files` est vrai (case cochée + nom retapé côté interface).
#[tauri::command]
async fn unexpose_project(ctx: State<'_, Arc<Ctx>>, name: String, delete_files: Option<bool>) -> Result<UnexposeResult, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("le retrait ne concerne que le mode standalone (les vhosts de Laragon ne sont pas touchés)".into()));
        }
        if !projects::valid_name(&name) {
            return Err(AppError::Config("nom de projet invalide".into()));
        }
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let conf = vhosts::conf_path(&c.paths.sites_enabled(), &domain);
        let vhost = if conf.is_file() {
            std::fs::remove_file(&conf)?;
            "supprimé"
        } else {
            "absent"
        };
        let (crt, key) = mkcert::cert_paths(&c.paths.ssl_dir(), &domain);
        for f in [crt, key] {
            let _ = std::fs::remove_file(f);
        }
        let hosts_path = hosts::hosts_path();
        let text = String::from_utf8(std::fs::read(&hosts_path)?).map_err(|_| AppError::Io("hosts non UTF-8 : modification refusée".into()))?;
        let hosts_state = match hosts::remove_domain(&text, &domain) {
            None => "absent",
            Some(new_text) => {
                hosts::apply_elevated_with(&new_text, &c.paths.data.join("tmp"), &domain, false)?;
                "retiré"
            }
        };
        let files = if delete_files.unwrap_or(false) {
            let dir = projects::project_dir(&c.paths.www, &name)?;
            if dir == c.paths.www || !dir.starts_with(&c.paths.www) {
                return Err(AppError::Config("suppression refusée : dossier hors de www".into()));
            }
            std::fs::remove_dir_all(&dir)?;
            "supprimés"
        } else {
            "conservés"
        };
        Ok(UnexposeResult { vhost, hosts: hosts_state, files, apache_restart_needed: vhost == "supprimé" })
    })
    .await
}

/// Teste le cache Redis du projet avec les paramètres de son `.env` : PHP (ext-redis) écrit puis relit une clé.
#[tauri::command]
async fn cache_test(ctx: State<'_, Arc<Ctx>>, project: String) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &project)?;
        let php = c.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
        let env = std::fs::read_to_string(dir.join(".env")).map_err(|_| AppError::NotStartable(".env introuvable".into()))?;
        if projects::env_value(&env, "CACHE_DRIVER").as_deref() != Some("redis") {
            return Err(AppError::NotStartable("activez d'abord le cache Redis pour ce projet".into()));
        }
        let get = |k: &str, d: &str| projects::env_value(&env, k).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
        let port: u16 = get("REDIS_PORT", "6379").parse().unwrap_or(6379);
        if !probe::port_open(port) {
            return Err(AppError::NotStartable(format!("Redis ne répond pas sur 127.0.0.1:{port} — démarrez-le (Tout démarrer) puis relancez le test")));
        }
        let code = "try{$r=new Redis();$r->connect(getenv('RH'),(int)getenv('RP'),3);if(getenv('RPW')!=='')$r->auth(getenv('RPW'));$r->select((int)getenv('RDB'));$k='astral:cache:astral-dev-test';$r->setex($k,60,'ok');echo ($r->get($k)==='ok')?'OK':'KO';}catch(Throwable $e){echo 'ERR:'.$e->getMessage();}";
        let mut cmd = std::process::Command::new(&php);
        cmd.arg("-r").arg(code)
            .env("RH", get("REDIS_HOST", "127.0.0.1"))
            .env("RP", get("REDIS_PORT", "6379"))
            .env("RPW", projects::env_value(&env, "REDIS_PASSWORD").unwrap_or_default())
            .env("RDB", get("REDIS_DB", "0"))
            .stdin(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(msg) = text.strip_prefix("ERR:") {
            return Err(AppError::StartFailed(format!("le cache Redis ne répond pas correctement : {} (vérifiez REDIS_HOST / REDIS_PORT / REDIS_PASSWORD dans le .env)", msg.trim())));
        }
        if text == "OK" {
            Ok("Cache Redis opérationnel : PHP a écrit puis relu la clé astral:cache:astral-dev-test (expire dans 60 s). Visible dans la page Redis.".into())
        } else {
            let err = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").to_string();
            Err(AppError::StartFailed(format!("test du cache en échec ({}){}", if text.is_empty() { "pas de réponse" } else { &text }, if err.is_empty() { String::new() } else { format!(" : {err}") })))
        }
    })
    .await
}

/// HTTPS local (mkcert) : installé ? autorité de confiance ? port HTTPS.
#[tauri::command]
async fn mkcert_status(ctx: State<'_, Arc<Ctx>>) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let path = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert);
        let trusted = path.is_some() && mkcert::ca_trusted();
        Ok(serde_json::json!({
            "installed": path.is_some(),
            "path": path.map(|p| p.display().to_string()),
            "ca_trusted": trusted,
            "ssl_port": c.cfg.ssl_port(),
        }))
    })
    .await
}

/// `mkcert -install` sur demande explicite (Windows demande confirmation à l'utilisateur).
#[tauri::command]
async fn mkcert_install_ca(ctx: State<'_, Arc<Ctx>>) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        mkcert::install_ca(&m)
    })
    .await
}

/// `mkcert -uninstall` sur demande explicite : retire l'approbation de l'autorité locale.
#[tauri::command]
async fn mkcert_uninstall_ca(ctx: State<'_, Arc<Ctx>>) -> Result<String, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        mkcert::uninstall_ca(&m)
    })
    .await
}

/// Active HTTPS pour un projet déjà exposé : certificat mkcert + bloc HTTPS dans son vhost.
#[tauri::command]
async fn enable_https(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<serde_json::Value, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        if !c.paths.standalone {
            return Err(AppError::NotStartable("HTTPS guidé : mode standalone seulement (les vhosts de Laragon ne sont pas modifiés)".into()));
        }
        let dir = projects::project_dir(&c.paths.www, &name)?;
        let domain = format!("{name}.{}", c.cfg.web.tld);
        let m = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert).ok_or_else(|| AppError::BinaryNotFound("mkcert (installez-le depuis la page Composants)".into()))?;
        let conf = vhosts::conf_path(&c.paths.sites_enabled(), &domain);
        let current = std::fs::read_to_string(&conf).map_err(|_| AppError::NotStartable("exposez d'abord le projet (vhost absent)".into()))?;
        if !current.starts_with("define ROOT") {
            return Err(AppError::NotStartable("vhost personnalisé : modification refusée".into()));
        }
        let (crt, key) = mkcert::generate(&m, &c.paths.ssl_dir(), &domain)?;
        let public = dir.join("public");
        std::fs::write(&conf, vhosts::render(&domain, &public, Some((&crt, &key)), c.cfg.web.port, c.cfg.ssl_port()))?;
        let ssl = c.cfg.ssl_port();
        // Le httpd.conf généré au démarrage ne contient `Listen <port HTTPS>` que si un certificat existait alors.
        let listens = std::fs::read_to_string(c.paths.httpd_conf()).map_or(false, |t| t.contains(&format!("Listen 127.0.0.1:{ssl}")));
        let url = if ssl == 443 { format!("https://{domain}/") } else { format!("https://{domain}:{ssl}/") };
        Ok(serde_json::json!({ "url": url, "app_restart_needed": !listens, "ca_trusted": mkcert::ca_trusted() }))
    })
    .await
}

/// Fichiers de logs d'astral-dev (`data/logs/*.log`) : nom + taille.
#[tauri::command]
fn list_logs(ctx: State<'_, Arc<Ctx>>) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = std::fs::read_dir(ctx.paths.logs())
        .map(|r| {
            r.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    let ok = n.ends_with(".log") && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
                    ok.then(|| (n, e.metadata().map(|m| m.len()).unwrap_or(0)))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Dernières lignes d'un log (`name` validé : jamais de chemin).
#[tauri::command]
fn tail_log(ctx: State<'_, Arc<Ctx>>, name: String, lines: Option<usize>) -> Result<Vec<String>, AppError> {
    use std::io::{Read, Seek, SeekFrom};
    if !name.ends_with(".log") || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err(AppError::Config("nom de journal invalide".into()));
    }
    let mut f = std::fs::File::open(ctx.paths.logs().join(&name)).map_err(|_| AppError::Io(format!("{name} introuvable")))?;
    let len = f.metadata()?.len();
    let want = 512 * 1024;
    f.seek(SeekFrom::Start(len.saturating_sub(want)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let all: Vec<&str> = text.lines().collect();
    let n = lines.unwrap_or(400).min(5000);
    Ok(all[all.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect())
}

/// Réglages courants (lus dans la configuration chargée au démarrage) + versions de PHP installées.
#[tauri::command]
fn settings_get(ctx: State<'_, Arc<Ctx>>) -> serde_json::Value {
    let mut php: Vec<String> = ctx
        .paths
        .php_dirs()
        .iter()
        .filter_map(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter_map(|n| n.strip_prefix("php-").map(|r| r.split('.').take(2).collect::<Vec<_>>().join(".")))
        .collect();
    php.sort();
    php.dedup();
    let c = &ctx.cfg;
    serde_json::json!({
        "standalone": ctx.paths.standalone,
        "file": config::Config::find_file().map(|p| p.display().to_string()),
        "php_version": c.php.version, "php_installed": php,
        "web_port": c.web.port, "tld": c.web.tld,
        "mysql_port": c.mysql.port, "redis_port": c.redis.port,
        "mailpit_http": c.mailpit.http, "mailpit_smtp": c.mailpit.smtp,
        "close_to_tray": c.ui.close_to_tray, "start_minimized": c.ui.start_minimized, "autostart_services": c.ui.autostart_services,
        "www": c.paths.www,
    })
}

#[derive(serde::Deserialize)]
struct SettingsInput {
    php_version: String,
    web_port: u16,
    mysql_port: u16,
    redis_port: u16,
    mailpit_http: u16,
    mailpit_smtp: u16,
    close_to_tray: bool,
    start_minimized: bool,
    #[serde(default)]
    autostart_services: bool,
}

/// Enregistre les réglages dans astral-dev.toml (modification ciblée, sauvegarde `astral-dev.toml.bak` une fois).
/// Un redémarrage d'astral-dev est nécessaire.
#[tauri::command]
fn settings_save(input: SettingsInput) -> Result<(), AppError> {
    let file = config::Config::find_file().ok_or_else(|| AppError::Io("astral-dev.toml introuvable".into()))?;
    let ports = [input.web_port, input.mysql_port, input.redis_port, input.mailpit_http, input.mailpit_smtp];
    if ports.iter().any(|p| *p == 0) || (0..ports.len()).any(|i| (i + 1..ports.len()).any(|j| ports[i] == ports[j])) {
        return Err(AppError::Config("ports invalides ou en double".into()));
    }
    let text = std::fs::read_to_string(&file)?;
    let q = |s: &str| format!("\"{s}\"");
    let new = config::Config::patch_toml(
        &text,
        &[
            ("php", "version", q(&input.php_version)),
            ("web", "port", input.web_port.to_string()),
            ("mysql", "port", input.mysql_port.to_string()),
            ("redis", "port", input.redis_port.to_string()),
            ("mailpit", "http", input.mailpit_http.to_string()),
            ("mailpit", "smtp", input.mailpit_smtp.to_string()),
            ("ui", "close_to_tray", input.close_to_tray.to_string()),
            ("ui", "start_minimized", input.start_minimized.to_string()),
            ("ui", "autostart_services", input.autostart_services.to_string()),
        ],
    );
    config::Config::parse(&new)?; // refuse tout ce qui rendrait le fichier invalide
    let bak = file.with_extension("toml.bak");
    if !bak.exists() {
        std::fs::write(&bak, &text)?;
    }
    std::fs::write(&file, new)?;
    Ok(())
}

/// Ouvre astral-dev.toml dans le Bloc-notes (réglages jusqu'à la page Réglages).
#[tauri::command]
fn open_config() -> Result<(), AppError> {
    let file = config::Config::find_file().ok_or_else(|| AppError::Io("astral-dev.toml introuvable".into()))?;
    std::process::Command::new("notepad").arg(file).spawn()?;
    Ok(())
}

/// Quitte vraiment l'application (la croix de la fenêtre ne fait que la fermer / la masquer).
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn list_templates() -> Vec<(&'static str, &'static str)> {
    projects::TEMPLATES.to_vec()
}

#[tauri::command]
async fn list_projects(ctx: State<'_, Arc<Ctx>>) -> Result<Vec<projects::ProjectInfo>, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let hosts_text = std::fs::read_to_string(hosts::hosts_path()).unwrap_or_default();
        projects::list(&c.paths.www, &c.cfg.web.tld, &c.paths.sites_enabled(), &hosts_text)
    })
    .await
}

/// F-011/F-012 : crée un projet via `composer create-project` (refus si le dossier existe).
#[tauri::command]
fn create_project(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, name: String, template: String) -> Result<u64, AppError> {
    if !projects::valid_name(&name) {
        return Err(AppError::Config("nom invalide : minuscules, chiffres et tirets (2 à 41 caractères)".into()));
    }
    let package = projects::template_package(&template).ok_or_else(|| AppError::Config(format!("modèle inconnu : {template}")))?;
    let target = ctx.paths.www.join(&name);
    if target.exists() {
        return Err(AppError::Config(format!("PROJECT_EXISTS : {name} existe déjà")));
    }
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let phar = ctx.paths.composer_phar();
    if !phar.is_file() {
        return Err(AppError::BinaryNotFound(phar.display().to_string()));
    }
    let args = vec![
        phar.to_string_lossy().to_string(),
        "create-project".into(),
        package.into(),
        target.to_string_lossy().to_string(),
        "--no-interaction".into(),
        "--no-progress".into(),
        "--prefer-dist".into(),
    ];
    let cwd = ctx.paths.www.clone();
    Ok(spawn_job(app, ctx.inner(), php, args, cwd))
}

/// F-013 : `php bin/console <cmd>` dans un projet Astral (liste blanche).
#[tauri::command]
fn run_console(app: tauri::AppHandle, ctx: State<'_, Arc<Ctx>>, project: String, args: Vec<String>) -> Result<u64, AppError> {
    console::check_args(&args)?;
    let dir = projects::project_dir(&ctx.paths.www, &project)?;
    if !dir.join("bin").join("console").is_file() {
        return Err(AppError::NotStartable(format!("{project} n'est pas un projet Astral (bin/console absent)")));
    }
    let php = ctx.paths.php_exe().ok_or_else(|| AppError::BinaryNotFound("php.exe".into()))?;
    let mut full = vec!["bin/console".to_string()];
    full.extend(args);
    Ok(spawn_job(app, ctx.inner(), php, full, dir))
}

/// F-014/F-020 : vhost Apache + entrée hosts (UAC à la demande). N'écrase rien d'existant.
#[tauri::command]
async fn expose_project(ctx: State<'_, Arc<Ctx>>, name: String) -> Result<ExposeResult, AppError> {
    let c = ctx.inner().clone();
    blocking(move || {
        let dir = projects::project_dir(&c.paths.www, &name)?;
        let domain = format!("{name}.{}", c.cfg.web.tld);
        if !hosts::valid_domain(&domain) {
            return Err(AppError::Config(format!("domaine invalide : {domain}")));
        }
        let certs = mkcert::find(&c.paths.root, &c.cfg.ssl.mkcert)
            .and_then(|m| mkcert::generate(&m, &c.paths.ssl_dir(), &domain).ok());
        let ssl = if certs.is_some() { "mkcert" } else if c.paths.ssl_dir().join("laragon.crt").is_file() { "laragon" } else { "aucun" };
        let vhost = match vhosts::ensure(&c.paths.sites_enabled(), &domain, &dir.join("public"), &c.paths.ssl_dir(), certs, c.cfg.web.port, c.cfg.ssl_port())? {
            vhosts::VhostOutcome::Created => "créé",
            vhosts::VhostOutcome::AlreadyExists => "déjà présent",
        };
        let hosts_path = hosts::hosts_path();
        let bytes = std::fs::read(&hosts_path)?;
        let text = String::from_utf8(bytes).map_err(|_| AppError::Io("hosts non UTF-8 : modification refusée".into()))?;
        let hosts_state = match hosts::add_domain(&text, &domain) {
            None => "déjà présent",
            Some(new_text) => {
                hosts::apply_elevated(&new_text, &c.paths.data.join("tmp"), &domain)?;
                "ajouté"
            }
        };
        Ok(ExposeResult { vhost, hosts: hosts_state, ssl, apache_restart_needed: vhost == "créé" })
    })
    .await
}
type Sup<'a> = State<'a, Arc<Supervisor>>;

async fn blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Io(e.to_string()))?
}

#[tauri::command]
async fn list_services(sup: Sup<'_>) -> Result<Vec<ServiceStatus>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.statuses())).await
}

#[tauri::command]
async fn start_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.start(&id)).await
}

#[tauri::command]
async fn stop_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.stop(&id)).await
}

#[tauri::command]
async fn restart_service(sup: Sup<'_>, id: String) -> Result<(), AppError> {
    let s = sup.inner().clone();
    blocking(move || s.restart(&id)).await
}

#[tauri::command]
async fn start_all(sup: Sup<'_>) -> Result<Vec<OpResult>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.start_all())).await
}

#[tauri::command]
async fn stop_all(sup: Sup<'_>) -> Result<Vec<OpResult>, AppError> {
    let s = sup.inner().clone();
    blocking(move || Ok(s.stop_everything())).await
}

#[tauri::command]
async fn redis_info(rt: State<'_, RedisTarget>) -> Result<RedisInfo, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::info(&p)).await
}

#[tauri::command]
async fn redis_scan(
    rt: State<'_, RedisTarget>,
    pattern: String,
    cursor: String,
    count: Option<usize>,
) -> Result<ScanPage, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::scan(&p, &pattern, &cursor, count.unwrap_or(100))).await
}

#[tauri::command]
async fn redis_get(rt: State<'_, RedisTarget>, key: String) -> Result<KeyDetail, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::get(&p, &key)).await
}

#[tauri::command]
async fn redis_del(rt: State<'_, RedisTarget>, keys: Vec<String>) -> Result<i64, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::del(&p, &keys)).await
}

#[tauri::command]
async fn redis_flush_prefix(rt: State<'_, RedisTarget>, prefix: String) -> Result<u64, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::flush_prefix(&p, &prefix)).await
}

/// F-049 : console Redis. Lecture seule hors mode avancé ; KEYS et commandes bloquantes toujours refusées.
#[tauri::command]
async fn redis_exec(rt: State<'_, RedisTarget>, line: String, advanced: bool) -> Result<String, AppError> {
    let p = rt.0.clone();
    blocking(move || redis_client::exec(&p, &line, advanced)).await
}

#[tauri::command]
async fn service_logs(sup: Sup<'_>, id: String, lines: Option<usize>) -> Result<Vec<String>, AppError> {
    let s = sup.inner().clone();
    blocking(move || s.logs(&id, lines.unwrap_or(500).min(5000))).await
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// F-062 : icône de zone de notification (afficher, tout démarrer, tout arrêter, quitter).
fn build_tray(app: &tauri::App, sup: Arc<Supervisor>) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let show = MenuItem::with_id(app, "show", "Afficher astral-dev", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start_all", "Tout démarrer", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop_all", "Tout arrêter", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
    let mut b = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("astral-dev")
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, ev| match ev.id.as_ref() {
            "show" => show_main(app),
            "start_all" => {
                let s = sup.clone();
                std::thread::spawn(move || {
                    s.start_all();
                });
            }
            "stop_all" => {
                let s = sup.clone();
                std::thread::spawn(move || {
                    s.stop_everything();
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

pub fn run() {
    let cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("astral-dev : {e} — valeurs par défaut utilisées");
        Config::default()
    });
    let paths = AppPaths::from_config(&cfg);
    if paths.standalone {
        for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
            let _ = std::fs::create_dir_all(d);
        }
        for dir in paths.php_dirs() {
            let _ = provision::write_php_ini(&dir, &paths.ssl_dir().join("cacert.pem"));
        }
        vhosts::sync_port(&paths.sites_enabled(), cfg.web.port, cfg.ssl_port());
    }
    let sup = Arc::new(
        Supervisor::new(services::build(&cfg, &paths), paths.logs())
            .with_own_root(if paths.standalone { Some(cfg.paths.root.as_str()) } else { None }),
    );

    if cfg.ui.autostart_services {
        let s = sup.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            s.start_all();
        });
    }
    let poller = sup.clone();
    let on_exit = sup.clone();
    let tray_sup = sup.clone();
    let ui = cfg.ui.clone();
    let self_logs = paths.logs();
    let www = paths.www.clone();

    tauri::Builder::default()
        // F-076 : une seule instance ; un second lancement ramène la fenêtre existante au premier plan.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .manage(sup)
        .manage(RedisTarget(Target { port: cfg.redis.port, password: services::redis_secret(&cfg, &paths) }))
        .manage(Arc::new(Ctx { cfg: cfg.clone(), paths: paths.clone(), jobs: std::sync::atomic::AtomicU64::new(0), installing: Default::default(), install_cancel: Default::default() }))
        .on_window_event({
            let close_to_tray = ui.close_to_tray;
            move |window, event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if close_to_tray {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .setup(move |app| {
            if let Err(e) = build_tray(app, tray_sup.clone()) {
                eprintln!("astral-dev : icône de notification indisponible ({e})");
            }
            // Diagnostic : `astral-dev.exe --selftest-restart` redémarre l'application seule après 6 s (aucun clic).
            if std::env::args().any(|x| x == "--selftest-restart") {
                let h = app.handle().clone();
                let s = tray_sup.clone();
                let logs = self_logs.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(6));
                    s.stop_all();
                    let _ = relaunch_and_exit(&h, logs);
                });
            }
            if !ui.start_minimized {
                show_main(app.handle());
            }
            // F-016 : nouveau dossier sous www/ → événement `www-new-project`.
            let h = app.handle().clone();
            let last = std::sync::Mutex::new((String::new(), std::time::Instant::now()));
            match watcher::start(www.clone(), move |name| {
                let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
                if l.0 == name && l.1.elapsed() < Duration::from_secs(2) {
                    return; // doublons d'événements Windows
                }
                *l = (name.clone(), std::time::Instant::now());
                let _ = h.emit("www-new-project", name);
            }) {
                Ok(w) => {
                    app.manage(std::sync::Mutex::new(w));
                }
                Err(e) => eprintln!("astral-dev : surveillance de www/ indisponible ({e})"),
            }
            // F-005 : l'état est poussé au front dès qu'il change (crash, démarrage externe…).
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut last = String::new();
                loop {
                    let st = poller.statuses();
                    let json = serde_json::to_string(&st).unwrap_or_default();
                    if json != last {
                        let _ = handle.emit("service-state", &st);
                        last = json;
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_services,
            start_service,
            stop_service,
            restart_service,
            start_all,
            stop_all,
            service_logs,
            redis_info,
            redis_scan,
            redis_get,
            redis_del,
            redis_flush_prefix,
            redis_exec,
            list_templates,
            list_projects,
            create_project,
            run_console,
            expose_project,
            cache_status,
            cache_enable,
            cache_disable,
            add_cache_package,
            mkcert_status,
            setup_status,
            import_laragon,
            restart_app,
            quit_app,
            open_config,
            unexpose_project,
            cache_test,
            mkcert_install_ca,
            mkcert_uninstall_ca,
            enable_https,
            list_logs,
            tail_log,
            settings_get,
            settings_save,
            components_list,
            install_component,
            remove_component,
            cancel_install
        ])
        .build(tauri::generate_context!())
        .expect("erreur au lancement d'astral-dev")
        .run(move |_app, event| {
            // F-003 : aucun processus orphelin à la fermeture.
            if let tauri::RunEvent::Exit = event {
                on_exit.stop_all();
            }
        });
}