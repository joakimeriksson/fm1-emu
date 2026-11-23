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
    let args = last().ok()?;
    let opts = crate::parse_args(
        std::iter::once("fm1-live".into())
            .chain(args.clone())
            .collect(),
    )?;
    (opts.bench.is_none()
        && opts.shot.is_none()
        && opts.fwsc.as_ref().is_some_and(|p| Path::new(p).is_file()))
    .then_some(args)
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
