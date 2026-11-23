//! Menu items use the same handlers as the window's shortcuts.
// minifb maps CTRL to Command on macOS; COMMAND alone produces no modifier.
use minifb::{Key, Menu, MENU_KEY_CTRL, MENU_KEY_SHIFT};
pub const OPEN: usize = 1001;
pub const CLOSE: usize = 1002;
pub const RECORD: usize = 1003;
pub const RESET: usize = 1004;
pub const LABELS: usize = 1005;
pub fn install() -> Result<Vec<Menu>, minifb::Error> {
    let mtm = objc2::MainThreadMarker::new().expect("menus must be installed on the main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let main_menu = app
        .mainMenu()
        .expect("the instrument window installs an app menu");
    let mut menus = Vec::new();
    for (title, items) in [
        (
            "File",
            vec![
                ("Open firmware…", OPEN, Some((Key::O, MENU_KEY_CTRL))),
                ("Close window", CLOSE, Some((Key::W, MENU_KEY_CTRL))),
            ],
        ),
        (
            "Audio",
            vec![(
                "Toggle WAV recording",
                RECORD,
                Some((Key::R, MENU_KEY_CTRL | MENU_KEY_SHIFT)),
            )],
        ),
        ("Instrument", vec![("Reset firmware state…", RESET, None)]),
        ("View", vec![("Toggle MIDI labels", LABELS, None)]),
    ] {
        let mut menu = Menu::new(title)?;
        let mut first_item = None;
        for (label, id, shortcut) in items {
            let mut item = menu.add_item(label, id);
            if let Some((key, modifiers)) = shortcut {
                item = item.shortcut(key, modifiers);
            }
            first_item.get_or_insert(item.build());
        }
        // minifb's add_menu assigns each custom menu the system Window role.
        // Its public macOS item handle points to an NSMenuItem retained by menu.
        let handle = first_item.expect("app menus contain actions");
        let native_menu = unsafe { (&*(handle.0 as *const objc2_app_kit::NSMenuItem)).menu() }
            .expect("the action belongs to its menu");
        native_menu.setAutoenablesItems(false);
        let top = objc2_app_kit::NSMenuItem::new(mtm);
        top.setTitle(&native_menu.title());
        top.setSubmenu(Some(&native_menu));
        main_menu.addItem(&top);
        menus.push(menu);
    }
    Ok(menus)
}

pub fn confirm_reset() -> bool {
    rfd::MessageDialog::new()
        .set_title("Reset firmware state")
        .set_description("Current saved state will be archived. The instrument will restart with its initial state.")
        .set_buttons(rfd::MessageButtons::OkCancelCustom("Reset instrument".into(), "Cancel".into()))
        .show() == rfd::MessageDialogResult::Custom("Reset instrument".into())
}
