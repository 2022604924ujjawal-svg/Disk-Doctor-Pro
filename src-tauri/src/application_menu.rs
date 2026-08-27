use tauri::{
    menu::{Menu, MenuEvent, MenuItem, MenuItemKind},
    AppHandle, Manager,
};

use crate::events;

const ABOUT_MENU_ITEM_ID: &str = "open-about";

pub fn build(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let Some(MenuItemKind::Submenu(application_menu)) = menu.items()?.into_iter().next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "default macOS application menu is unavailable",
        )
        .into());
    };
    let Some(MenuItemKind::Predefined(native_about_item)) =
        application_menu.items()?.into_iter().next()
    else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "default macOS About menu item is unavailable",
        )
        .into());
    };

    // Replace only the native About item. The remaining standard macOS
    // application menu entries retain their platform behavior and ordering.
    // Reusing the native label also preserves the system-provided localization.
    let about_text = native_about_item.text()?;
    application_menu.remove_at(0)?;
    let about_item = MenuItem::with_id(app, ABOUT_MENU_ITEM_ID, about_text, true, None::<&str>)?;
    application_menu.prepend(&about_item)?;
    Ok(menu)
}

pub fn handle(app: &AppHandle, event: MenuEvent) {
    if event.id().as_ref() != ABOUT_MENU_ITEM_ID {
        return;
    }

    let Some(window) = app.get_webview_window("main") else {
        log::warn!("about_menu_main_window_missing");
        return;
    };

    if let Err(error) = window.show() {
        log::warn!("about_menu_window_show_failed error={error}");
    }
    if let Err(error) = window.unminimize() {
        log::warn!("about_menu_window_unminimize_failed error={error}");
    }
    if let Err(error) = window.set_focus() {
        log::warn!("about_menu_window_focus_failed error={error}");
    }
    events::emit(app, events::OPEN_ABOUT, ());
}
