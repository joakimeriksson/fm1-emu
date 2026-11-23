//! Menu items use the same handlers as the window's shortcuts.
// minifb maps CTRL to Command on macOS; COMMAND alone produces no modifier.
use minifb::{Key, Menu, MENU_KEY_CTRL, MENU_KEY_SHIFT};
pub const OPEN: usize = 1001;
pub const CLOSE: usize = 1002;
pub const RECORD: usize = 1003;
pub const RESET: usize = 1004;
pub const LABELS: usize = 1005;
pub const CONTROLLER_AUTO: usize = 1006;
pub const CONTROLLER_USER: usize = 1007;
pub const CONTROLLER_RAW: usize = 1008;
#[derive(Default)]
pub struct Menus {
    _owned: Vec<Menu>,
    controllers: Vec<(usize, minifb::MenuItemHandle)>,
}
impl Menus {
    pub fn select_controller(&self, selection: &str) {
        for &(id, handle) in &self.controllers {
            // These native items remain retained by this menu set.
            let item = unsafe { &*(handle.0 as *const objc2_app_kit::NSMenuItem) };
            item.setState(if controller_selection(id) == Some(selection) {
                1
            } else {
                0
            });
        }
    }
}
pub fn controller_selection(id: usize) -> Option<&'static str> {
    match id {
        CONTROLLER_AUTO => Some("auto"),
        CONTROLLER_USER => Some(crate::ctl::MINILAB_USER),
        CONTROLLER_RAW => Some("none"),
        _ => None,
    }
}
pub fn install(selection: &str) -> Result<Menus, minifb::Error> {
    let mtm = objc2::MainThreadMarker::new().expect("menus must be installed on the main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let main_menu = app
        .mainMenu()
        .expect("the instrument window installs an app menu");
    let mut menus = Menus::default();
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
        (
            "Controller",
            vec![
                ("Automatic", CONTROLLER_AUTO, None),
                ("MiniLab FM-1 User", CONTROLLER_USER, None),
                ("Raw MIDI", CONTROLLER_RAW, None),
            ],
        ),
    ] {
        let mut menu = Menu::new(title)?;
        let mut first_item = None;
        for (label, id, shortcut) in items {
            let mut item = menu.add_item(label, id);
            if let Some((key, modifiers)) = shortcut {
                item = item.shortcut(key, modifiers);
            }
            let handle = item.build();
            first_item.get_or_insert(handle);
            if controller_selection(id).is_some() {
                menus.controllers.push((id, handle));
            }
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
        menus._owned.push(menu);
    }
    menus.select_controller(selection);
    Ok(menus)
}

pub fn confirm_reset() -> bool {
    rfd::MessageDialog::new()
        .set_title("Reset firmware state")
        .set_description("Current saved state will be archived. The instrument will restart with its initial state.")
        .set_buttons(rfd::MessageButtons::OkCancelCustom("Reset instrument".into(), "Cancel".into()))
        .show() == rfd::MessageDialogResult::Custom("Reset instrument".into())
}
