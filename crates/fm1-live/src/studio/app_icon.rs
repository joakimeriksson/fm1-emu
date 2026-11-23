//! Window artwork; macOS uses the app bundle's ICNS.
pub fn apply(window: &mut minifb::Window) {
    #[cfg(target_os = "windows")]
    unsafe {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        }
        #[link(name = "user32")]
        extern "system" {
            fn LoadImageW(
                module: *mut c_void,
                name: *const u16,
                kind: u32,
                width: i32,
                height: i32,
                flags: u32,
            ) -> *mut c_void;
            fn SendMessageW(
                window: *mut c_void,
                message: u32,
                wparam: usize,
                lparam: isize,
            ) -> isize;
        }
        for (kind, size) in [(0, 16), (1, 32)] {
            let icon = LoadImageW(
                GetModuleHandleW(std::ptr::null()),
                1usize as *const u16,
                1,
                size,
                size,
                0x8000,
            );
            if !icon.is_null() {
                SendMessageW(window.get_window_handle(), 0x80, kind, icon as isize);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut pixels = vec![64u64, 64u64];
        pixels.extend(
            include_bytes!("../../../../assets/icons/linux-64.argb")
                .chunks_exact(4)
                .map(|p| u32::from_le_bytes(p.try_into().unwrap()) as u64),
        );
        if let Ok(icon) = minifb::Icon::try_from(pixels.as_slice()) {
            window.set_icon(icon);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let _ = window;
}
