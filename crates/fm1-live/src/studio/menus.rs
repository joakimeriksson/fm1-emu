//! Menu items use the same handlers as the window's shortcuts.
use minifb::{Key, Menu, Window, MENU_KEY_COMMAND, MENU_KEY_SHIFT};
pub const OPEN: usize = 1001;
pub const CLOSE: usize = 1002;
pub const RECORD: usize = 1003;
pub const RESET: usize = 1004;
pub const LABELS: usize = 1005;
pub fn install(window: &mut Window) -> Result<Vec<Menu>, minifb::Error> {
    let mut menus = Vec::new();
    for (title, items) in [
        (
            "File",
            vec![
                ("Open firmware…", OPEN, Some((Key::O, MENU_KEY_COMMAND))),
                ("Close window", CLOSE, Some((Key::W, MENU_KEY_COMMAND))),
            ],
        ),
        (
            "Audio",
            vec![(
                "Toggle WAV recording",
                RECORD,
                Some((Key::R, MENU_KEY_COMMAND | MENU_KEY_SHIFT)),
            )],
        ),
        ("Instrument", vec![("Reset firmware state…", RESET, None)]),
        ("View", vec![("Toggle MIDI labels", LABELS, None)]),
    ] {
        let mut menu = Menu::new(title)?;
        for (label, id, shortcut) in items {
            let mut item = menu.add_item(label, id);
            if let Some((key, modifiers)) = shortcut {
                item = item.shortcut(key, modifiers);
            }
            item.build();
        }
        window.add_menu(&menu);
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
