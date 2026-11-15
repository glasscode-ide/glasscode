//! On Windows the program file carries Glasscode's icon and version, which File Explorer, the
//! taskbar, the window and "Installed apps" show. GPUI embeds the manifest (`vendor/gpui`, feature
//! `windows-manifest`), so this resource has none: a program has one manifest.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources/glasscode.rc");
    println!("cargo:rerun-if-changed=resources/Glasscode.ico");
    println!("cargo:rerun-if-env-changed=GLASSCODE_BUILD");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        windows_resource();
    }
}

/// Compile resources/glasscode.rc into glasscode.exe, with the version of this build.
#[cfg(windows)]
fn windows_resource() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let part = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|n| n.parse::<u16>().ok())
            .unwrap_or(0)
    };
    // The build number as glasscode_core::update::BUILD reads it, and its text as update::describe
    // writes it. Each of the four numbers of a Windows version has 16 bits, too few for a build
    // number, so only the text of the product version has it.
    let build = std::env::var("GLASSCODE_BUILD")
        .ok()
        .and_then(|b| b.parse::<u64>().ok())
        .unwrap_or(0);
    let described = if build == 0 {
        format!("{version} (development build)")
    } else {
        format!("{version} ({build})")
    };
    let header = format!(
        "#define GLASSCODE_VERSION {},{},{},0\n#define GLASSCODE_VERSION_TEXT \"{version}\"\n#define GLASSCODE_PRODUCT_VERSION_TEXT \"{described}\"\n",
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH"),
    );
    std::fs::write(out.join("glasscode_version.h"), header).expect("write glasscode_version.h");
    // OUT_DIR is on the include path of the resource compiler already; resources/ has the icon.
    let parameters = embed_resource::ParamsIncludeDirs(["resources"]);
    if let Err(error) =
        embed_resource::compile_for("resources/glasscode.rc", ["glasscode"], parameters)
            .manifest_required()
    {
        panic!("could not put the icon and the version into glasscode.exe: {error}");
    }
}
