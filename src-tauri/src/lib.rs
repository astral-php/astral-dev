mod catalog;
mod components;
mod config;
mod download;
mod engines;
mod install;
mod cache_setup;
mod console;
mod envfile;
mod error;
mod hosts;
mod mkcert;
mod projects;
mod php_pool;
mod provision;
mod vhosts;
mod watcher;
mod paths;
mod probe;
mod secrets;
mod sites;
mod redis_client;
mod services;
mod supervisor;
mod commands;
mod tray;

use commands::settings::relaunch_and_exit;
use commands::{Ctx, RedisTarget};
use redis_client::Target;
use tray::{build_tray, show_main};
use config::Config;
use paths::AppPaths;
use std::sync::Arc;
use std::time::Duration;
use supervisor::Supervisor;
use tauri::{Emitter, Manager};

pub fn run() {
    let cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("astral-dev : {e} — valeurs par défaut utilisées");
        Config::default()
    });
    let paths = AppPaths::from_config(&cfg);
    let mut warnings: Vec<String> = vec![];
    if paths.standalone {
        for d in [paths.www.clone(), paths.data.clone(), paths.logs(), paths.sites_enabled()] {
            let _ = std::fs::create_dir_all(d);
        }
        for dir in paths.php_dirs() {
            let _ = provision::write_php_ini(&dir, &paths.ssl_dir().join("cacert.pem"));
        }
        php_pool::prune_logs(&cfg, &paths);
        warnings.extend(php_pool::ts_only_warnings(&paths));
        if let Some(w) = engines::sync_all(&cfg, &paths) {
            eprintln!("astral-dev : {w}");
            warnings.push(w);
        }
    }
    let sup = Arc::new(
        Supervisor::new(services::build(&cfg, &paths), paths.logs())
            .with_own_root(if paths.standalone { Some(cfg.paths.root.as_str()) } else { None })
            .with_wanted_series({
                let (c, p) = (cfg.clone(), paths.clone());
                std::sync::Arc::new(move || php_pool::wanted_series(&c, &p))
            }),
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
        .manage(commands::StartupWarnings(warnings))
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
                    poller.heal_workers(); // un worker php-cgi tué ou planté est relancé sans attendre l'interface
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
            commands::services::list_services,
            commands::services::start_service,
            commands::services::stop_service,
            commands::services::restart_service,
            commands::services::start_all,
            commands::services::stop_all,
            commands::services::reload_web,
            commands::services::service_logs,
            commands::redis::redis_info,
            commands::redis::redis_scan,
            commands::redis::redis_get,
            commands::redis::redis_del,
            commands::redis::redis_flush_prefix,
            commands::redis::redis_exec,
            commands::projects::list_templates,
            commands::projects::list_projects,
            commands::projects::create_project,
            commands::projects::run_console,
            commands::projects::set_project_backend,
            commands::projects::expose_project,
            commands::cache::cache_status,
            commands::cache::cache_enable,
            commands::cache::cache_disable,
            commands::cache::add_cache_package,
            commands::https::mkcert_status,
            commands::components::setup_status,
            commands::components::import_laragon,
            commands::settings::restart_app,
            commands::settings::quit_app,
            commands::settings::open_config,
            commands::settings::fix_port_conflict,
            commands::settings::startup_warnings,
            commands::projects::list_php_versions,
            commands::projects::set_project_php,
            commands::https::disable_https,
            commands::projects::unexpose_project,
            commands::cache::cache_test,
            commands::https::mkcert_install_ca,
            commands::https::mkcert_uninstall_ca,
            commands::https::enable_https,
            commands::logs::list_logs,
            commands::logs::tail_log,
            commands::settings::settings_get,
            commands::settings::settings_save,
            commands::components::components_list,
            commands::components::install_component,
            commands::components::remove_component,
            commands::components::cancel_install
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
