use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=assets");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Extra virtual reserve for legitimate Shell/COM callback nesting; pages are
    // committed on demand. Re-entry suppression remains the primary safeguard.
    println!("cargo:rustc-link-arg-bin=ultra-bar=/STACK:4194304");
    let sdk = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK required"))
        .join("Windows Kits/10/bin");
    let mut versions: Vec<_> = fs::read_dir(sdk)
        .expect("Install Windows 10/11 SDK")
        .flatten()
        .map(|entry| entry.path().join("x64/rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    versions.sort();
    let compiler = versions.last().expect("Windows SDK rc.exe not found");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = format!("{},0", version.split('-').next().unwrap().replace('.', ","));
    let icon = root
        .join("assets/ultra-bar.ico")
        .to_string_lossy()
        .replace('\\', "/");
    let manifest = root
        .join("assets/ultra-bar.manifest")
        .to_string_lossy()
        .replace('\\', "/");
    let rc = format!(
        r#"
#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0
FILEOS 0x40004L
FILETYPE 1
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904b0"
  BEGIN
   VALUE "CompanyName", "Emssion\0"
   VALUE "FileDescription", "Ultra Bar\0"
   VALUE "ProductName", "Ultra Bar\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "ProductVersion", "{version}\0"
   VALUE "OriginalFilename", "ultra-bar.exe\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#
    );
    let input = out.join("ultra-bar.rc");
    let resource = out.join("ultra-bar.res");
    fs::write(&input, rc).unwrap();
    assert!(
        Command::new(compiler)
            .arg("/nologo")
            .arg("/fo")
            .arg(&resource)
            .arg(input)
            .status()
            .unwrap()
            .success(),
        "Resource compilation failed"
    );
    println!("cargo:rustc-link-arg-bin=ultra-bar={}", resource.display());
}
