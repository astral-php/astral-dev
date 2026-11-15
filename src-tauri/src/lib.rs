mod config;
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

#[derive(serde::Serialize)]
struct MkcertStatus {
    found: bool,
    path: Option<String>,
}

#[tauri::command]
fn mkcert_status(ctx: State<'_, Arc<Ctx>>) -> MkcertStatus {
    let p = mkcert::find(&ctx.paths.root, &ctx.cfg.ssl.mkcert);
    MkcertStatus { found: p.is_some(), path: p.map(|p| p.display().to_string()) }
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

/// Relance l'application (après l'import : les services sont reconstruits avec les nouveaux binaires).
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
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
        let vhost = match vhosts::ensure(&c.paths.sites_enabled(), &domain, &dir.join("public"), &c.paths.ssl_dir(), certs, c.cfg.web.port)? {
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
    blocking(move || Ok(s.stop_all())).await
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
                    s.stop_all();
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
        vhosts::sync_port(&paths.sites_enabled(), cfg.web.port);
    }
    let sup = Arc::new(
        Supervisor::new(services::build(&cfg, &paths), paths.logs())
            .with_own_root(if paths.standalone { Some(cfg.paths.root.as_str()) } else { None }),
    );

    let poller = sup.clone();
    let on_exit = sup.clone();
    let tray_sup = sup.clone();
    let ui = cfg.ui.clone();
    let www = paths.www.clone();

    tauri::Builder::default()
        // F-076 : une seule instance ; un second lancement ramène la fenêtre existante au premier plan.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .manage(sup)
        .manage(RedisTarget(Target { port: cfg.redis.port, password: services::redis_secret(&cfg, &paths) }))
        .manage(Arc::new(Ctx { cfg: cfg.clone(), paths: paths.clone(), jobs: std::sync::atomic::AtomicU64::new(0) }))
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
            open_config
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