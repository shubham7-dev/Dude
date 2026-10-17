use tauri::{
    image::Image,
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

pub const TRAY_ID: &str = "dude-tray";

pub fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let tray_icon = Image::from_bytes(include_bytes!("../icons/tray-icon.png"))
        .expect("failed to load tray icon image");

    let native_menu = MenuBuilder::new(app)
        .text("show_hide", "Show / Hide")
        .text("open_dude", "Open Dude")
        .separator()
        .text("quit", "Quit")
        .build()?;

    let native_menu_clone = native_menu.clone();

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon)
        .tooltip("Dude")
        .on_menu_event(|app, event| {
            match event.id.as_ref() {
                "show_hide" => {
                    let _ = crate::toggle_mascot_window_impl(app.clone());
                }
                "open_dude" => {
                    let _ = crate::open_dude_settings_impl(app.clone(), None);
                }
                "quit" => {
                    crate::quit_app_impl(app.clone());
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            let app = tray.app_handle();
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    if let Some(menu_win) = app.get_webview_window("context-menu") {
                        let _ = menu_win.hide();
                    }
                    let _ = crate::toggle_mascot_window_impl(app.clone());
                }
                TrayIconEvent::Click {
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                    position,
                    rect,
                    ..
                } => {
                    let res = position_context_menu_above_tray(app, position.x, position.y, rect);
                    if res.is_err() {
                        let _ = tray.set_menu(Some(native_menu_clone.clone()));
                    }
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

pub fn remove_tray(app: &tauri::AppHandle) {
    if let Some(tray) = app.remove_tray_by_id(TRAY_ID) {
        drop(tray);
    }
}

pub fn position_context_menu_above_tray(
    app: &tauri::AppHandle,
    click_x: f64,
    click_y: f64,
    rect: tauri::Rect,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut pos_x, mut pos_y) = (click_x, click_y);
    let monitor = app
        .monitor_from_point(pos_x, pos_y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());

    let scale_factor = monitor.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
    let mon_x = monitor.as_ref().map(|m| m.position().x as f64).unwrap_or(0.0);
    let mon_y = monitor.as_ref().map(|m| m.position().y as f64).unwrap_or(0.0);
    let mon_w = monitor
        .as_ref()
        .map(|m| m.size().width as f64)
        .unwrap_or(1920.0 * scale_factor);
    let mon_h = monitor
        .as_ref()
        .map(|m| m.size().height as f64)
        .unwrap_or(1080.0 * scale_factor);

    let (rect_x, rect_y) = match rect.position {
        tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
        tauri::Position::Logical(l) => (l.x * scale_factor, l.y * scale_factor),
    };
    let (rect_w, rect_h) = match rect.size {
        tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
        tauri::Size::Logical(l) => (l.width * scale_factor, l.height * scale_factor),
    };

    if pos_x <= 0.0 && pos_y <= 0.0 {
        if rect_w > 0.0 && rect_h > 0.0 {
            pos_x = rect_x + (rect_w / 2.0);
            pos_y = rect_y;
        } else if let Ok(cursor) = app.cursor_position() {
            pos_x = cursor.x;
            pos_y = cursor.y;
        }
    }

    if pos_x <= 0.0 && pos_y <= 0.0 {
        return Err("No valid coordinates available".into());
    }

    let win_w = 230.0 * scale_factor;
    let win_h = 190.0 * scale_factor;
    let offset = 8.0 * scale_factor;

    let mut target_x = pos_x - (win_w / 2.0);
    let tray_top = if rect_h > 0.0 { rect_y } else { pos_y };
    let mut target_y = tray_top - win_h - offset;

    let min_x = mon_x;
    let max_x = mon_x + mon_w;
    let min_y = mon_y;
    let max_y = mon_y + mon_h;

    if target_x + win_w > max_x {
        target_x = max_x - win_w - (8.0 * scale_factor);
    }
    if target_x < min_x {
        target_x = min_x + (8.0 * scale_factor);
    }

    if target_y < min_y {
        let tray_bottom = if rect_h > 0.0 { rect_y + rect_h } else { pos_y };
        target_y = tray_bottom + offset;
    }
    if target_y + win_h > max_y {
        target_y = max_y - win_h - (8.0 * scale_factor);
    }
    if target_y < min_y {
        target_y = min_y + (8.0 * scale_factor);
    }

    let target_pos = tauri::Position::Physical(tauri::PhysicalPosition {
        x: target_x.round() as i32,
        y: target_y.round() as i32,
    });

    if let Some(context_win) = app.get_webview_window("context-menu") {
        let _ = context_win.set_position(target_pos);
        let _ = context_win.show();
        let _ = context_win.set_focus();
    } else {
        let win = tauri::WebviewWindowBuilder::new(
            app,
            "context-menu",
            tauri::WebviewUrl::App("menu.html".into()),
        )
        .title("Dude Menu")
        .inner_size(230.0, 190.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .build()?;

        let _ = win.set_position(target_pos);
        let _ = win.show();
        let _ = win.set_focus();
    }

    Ok(())
}
