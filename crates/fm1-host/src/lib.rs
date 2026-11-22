//! Shared desktop entry to the file-based hardware companion. Audio playback is consumer-owned in `audio`.
pub mod audio;

use std::{io, path::PathBuf, process::Command};
pub fn companion_html() -> String {
    let modules = [include_str!("../../../web/hardware/protocol.js"),
        include_str!("../../../web/hardware/transport.js"), include_str!("../../../web/hardware/page.js")];
    let js = modules.iter().flat_map(|s| s.lines()).filter(|line| !line.starts_with("import "))
        .map(|line| line.strip_prefix("export ").unwrap_or(line)).collect::<Vec<_>>().join("\n");
    include_str!("../../../web/hardware/index.html")
        .replace("../favicon.ico", include_str!("../../../assets/icons/favicon-data-uri.txt"))
        .replace("../icons/favicon-32.png", include_str!("../../../assets/icons/favicon-data-uri.txt"))
        .replace("../icons/favicon-16.png", include_str!("../../../assets/icons/favicon-data-uri.txt"))
        .replace("<link rel=\"manifest\" href=\"../site.webmanifest\">", "")
        .replace("<link rel=\"apple-touch-icon\" sizes=\"180x180\" href=\"../icons/apple-touch-icon-180.png\">", "")
        .replace("<script type=\"module\" src=\"./page.js\"></script>", &format!("<script type=\"module\">{js}</script>"))
}
/// Run on a host UI job/thread, never from a realtime callback or project recall.
pub fn open_hardware_library() -> io::Result<()> {
    let home=std::env::var_os("HOME").ok_or_else(|| io::Error::other("Home folder unavailable"))?;
    let path=PathBuf::from(home).join("Library/Application Support/FM-1 Studio/hardware-library.html");
    std::fs::create_dir_all(path.parent().unwrap())?;
    // A stable shared file gives standalone and plugin launches one browser/lock scope.
    let html=companion_html();
    if std::fs::read(&path).ok().as_deref()!=Some(html.as_bytes()) { std::fs::write(&path,html)?; }
    #[cfg(target_os="macos")]
    for browser in ["Google Chrome","Microsoft Edge"] {
        if Command::new("open").args(["-a",browser]).arg(&path).status()?.success(){return Ok(());}
    }
    Err(io::Error::other("Open the Hardware Library in Chrome or Edge; neither browser could be launched."))
}
#[cfg(test)] mod tests {
    #[test] fn embedded_companion_is_self_contained(){let s=super::companion_html();assert!(!s.contains("src=\"./page.js\""));assert!(!s.contains("import {"));assert!(s.contains("class Sloop3"));assert!(s.contains("class MidiConnection"));assert!(s.contains("data:image/png;base64,"));assert!(!s.contains("../icons/"));assert!(!s.contains("../site.webmanifest"));}
}

/// Keep Help usable when the app bundle is moved away from the source checkout.
pub fn open_user_guide() -> io::Result<()> {
    let home=std::env::var_os("HOME").ok_or_else(|| io::Error::other("Home folder unavailable"))?;
    let path=PathBuf::from(home).join("Library/Application Support/FM-1 Studio/User Guide.md");
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path,include_str!("../../../context/user-guide.md"))?;
    Command::new("open").arg("-a").arg("TextEdit").arg(path).status()?;
    Ok(())
}

pub fn session_folder() -> io::Result<PathBuf> {
    let home=std::env::var_os("HOME").ok_or_else(||io::Error::other("Home folder unavailable"))?;
    let p=PathBuf::from(home).join("Library/Application Support/FM-1 Studio/sessions");
    std::fs::create_dir_all(&p)?; Ok(p)
}
pub fn last_session() -> io::Result<Vec<String>> {
    let bytes=std::fs::read(session_folder()?.join("last.json"))?;
    if bytes.len()>65536{return Err(io::Error::other("Invalid session settings"));}
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
pub fn remember_session(args: &[String]) -> io::Result<()> {
    let path=session_folder()?.join("last.json");let tmp=path.with_extension("tmp");
    std::fs::write(&tmp,serde_json::to_vec(args)?)?;std::fs::rename(tmp,path)
}
pub fn firmware_memory(bytes: &[u8]) -> io::Result<PathBuf> {
    use sha2::{Digest,Sha256};
    Ok(session_folder()?.join(format!("{:x}.bin",Sha256::digest(bytes))))
}
pub fn archive_memory(path: &str) -> io::Result<()> {
    let p=std::path::Path::new(path);
    if p.exists(){let stamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(io::Error::other)?.as_nanos();
        std::fs::rename(p,p.with_extension(format!("before-reset-{stamp}.bin")))?;}
    Ok(())
}

#[cfg(test)] mod session_tests {
    #[test] fn reset_archive_preserves_original_bytes() {
        let path=std::env::temp_dir().join(format!("fm1-reset-test-{}-{}.bin",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::write(&path,b"saved memory").unwrap();
        super::archive_memory(path.to_str().unwrap()).unwrap();
        assert!(!path.exists());
        let prefix=path.file_stem().unwrap().to_string_lossy().to_string()+".before-reset-";
        let archive=std::fs::read_dir(path.parent().unwrap()).unwrap().filter_map(Result::ok).find(|e|e.file_name().to_string_lossy().starts_with(&prefix)).unwrap().path();
        assert_eq!(std::fs::read(&archive).unwrap(),b"saved memory");
        std::fs::remove_file(archive).unwrap();
    }
}
