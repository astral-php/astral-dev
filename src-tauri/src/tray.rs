//! Icône de zone de notification (F-062) et affichage de la fenêtre principale.
use std::sync::Arc;
use tauri::{Manager};
use crate::supervisor::{Supervisor};

pub(crate) fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// F-062 : icône de zone de notification (afficher, tout démarrer, tout arrêter, quitter).
pub(crate) fn build_tray(app: &tauri::App, sup: Arc<Supervisor>) -> tauri::Result<()> {
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
