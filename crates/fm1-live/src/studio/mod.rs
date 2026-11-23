//! Standalone customizations; keep upstream execution and host APIs intact.
#[cfg(test)]
mod controller_tests;
#[cfg(target_os = "macos")]
pub mod menus;
pub mod midi;
pub mod panel;
#[cfg(target_os = "macos")]
pub mod session;
pub mod shortcuts;
pub mod app_icon;
