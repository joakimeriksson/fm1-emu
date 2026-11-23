//! Finder remembers launch options; firmware-owned saves remain ordinary flash files.
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};
fn directory() -> io::Result<PathBuf> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("Home folder unavailable"))?;
    let dir = PathBuf::from(home).join("Library/Application Support/FM-1 Studio/sessions");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
pub fn last() -> io::Result<Vec<String>> {
    let bytes = std::fs::read(directory()?.join("last.json"))?;
    if bytes.len() > 65536 {
        return Err(io::Error::other("Invalid launch settings"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
pub fn recalled() -> Option<Vec<String>> {
    validate_recall(last().ok()?)
}
fn validate_recall(args: Vec<String>) -> Option<Vec<String>> {
    let opts = crate::parse_args(
        std::iter::once("fm1-live".into())
            .chain(args.clone())
            .collect(),
    )?;
    if opts.bench.is_some() || opts.shot.is_some() || opts.fwsc.is_none() {
        return None;
    }
    // An existing file can still be incompatible with this upstream loader.
    // Validate the complete saved launch before hiding the firmware picker.
    if let Err(error) = crate::Machine::new(&opts) {
        eprintln!("Previous session unavailable: {error}");
        return None;
    }
    Some(args)
}
pub fn remember(args: &[String], firmware: &str) -> io::Result<()> {
    let mut args = args.to_vec();
    for i in 1..args.len() {
        if args[i - 1] == "--fw" {
            args[i - 1] = "--fwsc".into();
            args[i] = firmware.into();
        }
        if ["--fwsc", "--area", "--bin", "--syx", "--flash"].contains(&args[i - 1].as_str())
            || (args[i - 1] == "--map" && args[i].ends_with(".json"))
        {
            let path = PathBuf::from(&args[i]);
            args[i] = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            }
            .to_string_lossy()
            .into_owned();
        }
    }
    let path = directory()?.join("last.json");
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec(&args)?)?;
    std::fs::rename(temp, path)
}
pub fn memory(firmware: &str) -> io::Result<String> {
    let bytes = std::fs::read(firmware)?;
    Ok(directory()?
        .join(format!("{:x}.bin", Sha256::digest(bytes)))
        .to_string_lossy()
        .into_owned())
}
pub fn archive(path: &Path) -> io::Result<()> {
    if path.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        std::fs::rename(
            path,
            path.with_extension(format!("before-reset-{stamp}.bin")),
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn existing_unsupported_firmware_returns_to_picker_without_changing_files() {
        let dir = std::env::temp_dir().join(format!("fm1-recall-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let firmware = dir.join("unsupported.area");
        let memory = dir.join("state.bin");
        std::fs::write(&firmware, b"not an update package").unwrap();
        std::fs::write(&memory, b"preserved session memory").unwrap();
        let args = vec![
            "--fwsc".into(),
            firmware.to_string_lossy().into_owned(),
            "--flash".into(),
            memory.to_string_lossy().into_owned(),
        ];
        assert!(super::validate_recall(args).is_none());
        assert_eq!(std::fs::read(&firmware).unwrap(), b"not an update package");
        assert_eq!(std::fs::read(&memory).unwrap(), b"preserved session memory");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compatible_firmware_keeps_its_saved_launch_options() {
        let firmware = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/firmwares/felucca.fwsc");
        if !firmware.is_file() {
            return;
        }
        let args = vec![
            "--fwsc".into(),
            firmware.to_string_lossy().into_owned(),
            "--midi".into(),
            "none".into(),
        ];
        assert_eq!(super::validate_recall(args.clone()), Some(args));
    }
    #[test]
    fn reset_keeps_original_flash() {
        let dir = std::env::temp_dir().join(format!("fm1-lean-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.bin");
        std::fs::write(&file, b"saved instrument").unwrap();
        super::archive(&file).unwrap();
        assert!(!file.exists());
        let backup = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(std::fs::read(backup).unwrap(), b"saved instrument");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
