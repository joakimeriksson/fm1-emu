//! Portable v1 `.fm1save` files, shared with the browser's web/persistence.js.
//! Firmware parsing/boot validation happens on the machine thread before replacing state.
use crate::{Firmware, Sector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

pub const SECTOR_SIZE: usize = 4096;
pub const MAX_PACKAGE: usize = 8 << 20;
pub const MAX_SAVE: usize = 16 << 20;
const FORMAT: &str = "fm1-emulator-save";

#[derive(Clone)]
pub struct SaveData {
    pub firmware: Firmware,
    pub sectors: Vec<Sector>,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    hash: String,
    name: String,
    #[serde(default, with = "optional_package")]
    package: Option<Arc<Vec<u8>>>,
    sectors: Vec<Sector>,
}

mod optional_package {
    use super::*;
    use base64::Engine;
    pub fn serialize<S: serde::Serializer>(v: &Option<Arc<Vec<u8>>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().map(|p| base64::engine::general_purpose::STANDARD.encode(p.as_slice())).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Arc<Vec<u8>>>, D::Error> {
        Option::<String>::deserialize(d)?.map(|s| base64::engine::general_purpose::STANDARD.decode(s)
            .map(Arc::new).map_err(serde::de::Error::custom)).transpose()
    }
}

pub fn validate_sectors(sectors: &[Sector]) -> Result<(), String> {
    let mut seen = [false; fm1_soc::FLASH_SIZE / SECTOR_SIZE];
    if sectors.len() > seen.len() { return Err("too many flash sectors".into()); }
    for sector in sectors {
        let at = sector.at as usize;
        if at >= fm1_soc::FLASH_SIZE || at % SECTOR_SIZE != 0 || sector.data.len() != SECTOR_SIZE {
            return Err("invalid flash sector offset or length".into());
        }
        if std::mem::replace(&mut seen[at / SECTOR_SIZE], true) {
            return Err("duplicate flash sector".into());
        }
    }
    Ok(())
}

fn validate(data: &SaveData) -> Result<(), String> {
    if !(20 * 0x30..=MAX_PACKAGE).contains(&data.firmware.pkg.len()) {
        return Err("invalid firmware package size".into());
    }
    if data.firmware.name.encode_utf16().count() > 256 { return Err("save name too long".into()); }
    validate_sectors(&data.sectors)
}

fn hash(pkg: &[u8]) -> String { format!("{:x}", Sha256::digest(pkg)) }

pub fn decode(bytes: &[u8]) -> Result<SaveData, String> {
    decode_with_firmware(bytes, None)
}

pub fn decode_with_firmware(bytes: &[u8], fallback: Option<&Firmware>) -> Result<SaveData, String> {
    if bytes.len() > MAX_SAVE { return Err("save file too large".into()); }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| format!("invalid .fm1save: {e}"))?;
    if envelope.format != FORMAT || envelope.version != 1 { return Err("unsupported emulator save format or version".into()); }
    let pkg = envelope.package.or_else(|| fallback.map(|f| f.pkg.clone()))
        .ok_or("load the matching .fwsc firmware first, then import this save again")?;
    let data = SaveData { firmware: Firmware { name: envelope.name, pkg }, sectors: envelope.sectors };
    validate(&data)?;
    if hash(&data.firmware.pkg) != envelope.hash { return Err("firmware hash mismatch".into()); }
    Ok(data)
}

pub fn encode(data: &SaveData) -> Result<Vec<u8>, String> {
    validate(data)?;
    serde_json::to_vec(&Envelope { format: FORMAT.into(), version: 1, hash: hash(&data.firmware.pkg),
        name: data.firmware.name.clone(), package: Some(data.firmware.pkg.clone()), sectors: data.sectors.clone() })
        .map_err(|e| format!("could not encode save: {e}"))
}

/// Limit reads even when a file grows after the dialog opens or metadata was misleading.
pub fn read(path: &Path) -> Result<SaveData, String> { read_with_firmware(path, None) }

pub fn read_with_firmware(path: &Path, fallback: Option<&Firmware>) -> Result<SaveData, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(io_error)?.take((MAX_SAVE + 1) as u64)
        .read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() > MAX_SAVE { return Err("save file too large".into()); }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("fwsc")) {
        let data = SaveData { firmware: Firmware {
            name: path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            pkg: Arc::new(bytes),
        }, sectors: Vec::new() };
        validate(&data)?;
        Ok(data)
    } else {
        decode_with_firmware(&bytes, fallback)
    }
}

/// Replace only after the entire save has been encoded and written successfully.
pub fn write(path: &Path, data: &SaveData) -> Result<(), String> {
    let bytes = encode(data)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.as_file().sync_all().map_err(io_error)?;
    file.persist(path).map_err(|e| io_error(e.error))?;
    Ok(())
}

/// A file error as the status bar shows it. On a Mac, "Operation not permitted" on a file the
/// user just picked is the system keeping the plugin's process (a DAW's plugin host runs apart
/// from the DAW, and nothing asks the user on its behalf) out of the folders it protects:
/// Desktop, Documents, Downloads, iCloud Drive and removable drives. The file is fine
/// anywhere else in the home folder; say so, as the dialog's choice gave no sign (#56).
fn io_error(e: std::io::Error) -> String {
    if cfg!(target_os = "macos") && e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("{e}; macOS keeps plugins out of Desktop, Documents, Downloads and iCloud Drive: move the file to ~/Music or another folder in your home")
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> SaveData {
        SaveData { firmware: Firmware { name: "共享 setup 🎹".into(), pkg: Arc::new(vec![42; 960]) },
            sectors: vec![Sector { at: 0xff000, data: Arc::new(vec![0xff; 4096]) }] }
    }
    #[test]
    fn portable_round_trip_and_atomic_file_write() {
        let original = sample();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.fm1save");
        write(&path, &original).unwrap();
        let restored = read(&path).unwrap();
        assert_eq!(restored.firmware.pkg, original.firmware.pkg);
        assert_eq!(restored.firmware.name, original.firmware.name);
        assert_eq!(restored.sectors[0].data, original.sectors[0].data);
        let before = std::fs::read(&path).unwrap();
        let mut invalid = sample(); invalid.sectors[0].at = 1;
        assert!(write(&path, &invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn package_less_browser_save_requires_matching_loaded_firmware() {
        let original = sample();
        let mut json: serde_json::Value = serde_json::from_slice(&encode(&original).unwrap()).unwrap();
        json["package"] = serde_json::Value::Null;
        let bytes = serde_json::to_vec(&json).unwrap();
        assert!(decode(&bytes).is_err());
        let restored = decode_with_firmware(&bytes, Some(&original.firmware)).unwrap();
        assert_eq!(restored.sectors[0].data, original.sectors[0].data);
        let mut wrong = original.firmware.clone(); wrong.pkg = Arc::new(vec![0; 960]);
        assert!(decode_with_firmware(&bytes, Some(&wrong)).is_err());
    }
    #[test]
    fn malformed_files_and_sectors_are_rejected() {
        let data = encode(&sample()).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&data).unwrap();
        for field in ["hash", "format", "package"] {
            let mut bad = original.clone(); bad[field] = "wrong".into();
            assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut future = original.clone(); future["version"] = 2.into();
        assert!(decode(&serde_json::to_vec(&future).unwrap()).is_err());
        let mut duplicate = sample(); duplicate.sectors.push(duplicate.sectors[0].clone());
        assert!(encode(&duplicate).is_err());
        for at in [1, 0x100000, u32::MAX] {
            let mut bad = sample(); bad.sectors[0].at = at;
            assert!(encode(&bad).is_err());
        }
        let mut short = sample(); short.sectors[0].data = Arc::new(vec![0; 4095]);
        assert!(encode(&short).is_err());
        assert!(decode(b"{}").is_err());
        assert!(decode(&vec![b' '; MAX_SAVE + 1]).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_file_the_plugin_may_not_read_says_where_to_put_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("felucca.fwsc");
        std::fs::write(&path, b"not read anyway").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() { return; } // (root reads anything: nothing to test)
        let message = match read(&path) { Err(m) => m, Ok(_) => panic!("read a file with no permissions") };
        assert!(message.contains("ermission denied"), "{message}");
        assert_eq!(message.contains("~/Music"), cfg!(target_os = "macos"), "{message}");
        let plain = io_error(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
        assert_eq!(plain, "gone");
    }
}

#[cfg(test)]
mod browser_fixture {
    use super::*;
    // Generated by PR #9's browser exportSave(), with synthetic bytes (no third-party firmware).
    #[test]
    fn accepts_a_browser_generated_v1_save() {
        let data = decode(include_bytes!("../tests/fixtures/browser-v1.fm1save")).unwrap();
        assert_eq!(data.firmware.name, "Browser shared setup");
        assert_eq!(data.firmware.pkg.len(), 960);
        assert_eq!(data.firmware.pkg[511], 255);
        assert_eq!(data.sectors[0].at, 0xff000);
        assert!(data.sectors[0].data.iter().all(|&b| b == 0x56));
        let again = decode(&encode(&data).unwrap()).unwrap();
        assert_eq!(again.firmware.pkg, data.firmware.pkg);
        assert_eq!(again.sectors[0].data, data.sectors[0].data);
    }
}
