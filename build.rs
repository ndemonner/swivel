//! Build script.
//!
//! The product is one binary, not an application bundle, so there is nowhere to
//! put an `Info.plist`. macOS refuses the microphone without one, and without
//! `LSUIElement` the process would take a Dock icon.
//!
//! The plist is therefore linked into the binary itself, in the
//! `__TEXT,__info_plist` section. macOS reads it from there exactly as it would
//! read it from a bundle. See `ARCHITECTURE.md` §9.1.

fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest directory");
    let plist = std::path::Path::new(&dir).join("Info.plist");

    if !plist.exists() {
        println!("cargo:warning=Info.plist is missing, so the microphone will be refused");
        return;
    }

    println!(
        "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}
