mod buildinfo { include!("../fm1-emu/build.rs"); pub fn run() { main(); } }
mod icon { include!("../../tools/windows-icon.rs"); }
fn main() { buildinfo::run(); icon::embed(); }
