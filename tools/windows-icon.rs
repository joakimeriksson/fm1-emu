// Shared build-script helper: embed Explorer artwork without extra Cargo dependencies.
pub fn embed() {
    use std::{env, path::PathBuf, process::Command};
    println!("cargo:rerun-if-changed=../../assets/icons/FM1.ico");
    println!("cargo:rerun-if-changed=../../tools/windows-icon.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") { return; }
    assert_eq!(env::var("CARGO_CFG_TARGET_ENV").unwrap(), "msvc", "Windows icon embedding requires the supported MSVC target");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("../../assets/icons/FM1.ico").canonicalize().unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let rc = out.join("fm1-icon.rc"); let res = out.join("fm1-icon.res");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "/"))).unwrap();
    let compiler = if Command::new("rc.exe").arg("/?").output().is_ok() { PathBuf::from("rc.exe") } else {
        let kits = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK required")).join("Windows Kits/10/bin");
        let mut versions: Vec<_> = std::fs::read_dir(kits).expect("Install Windows SDK").filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("x64/rc.exe").is_file()).collect();
        versions.sort(); versions.pop().expect("Windows SDK rc.exe required").join("x64/rc.exe")
    };
    assert!(Command::new(compiler).arg("/nologo").arg("/fo").arg(&res).arg(&rc).status().expect("run resource compiler").success());
    println!("cargo:rustc-link-arg={}",res.display());
}
