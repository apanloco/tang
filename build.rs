fn main() {
    // On macOS, embed Info.plist into the binary's __TEXT,__info_plist section
    // so LaunchServices reads LSUIElement=true at process launch and never
    // creates a dock icon for our TUI.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        println!("cargo:rerun-if-changed={}", plist.display());
        println!(
            "cargo:rustc-link-arg-bin=tang=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
