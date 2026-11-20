fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icons.res");
        println!("cargo:rerun-if-changed={}", icons.display());
        println!(
            "cargo:rustc-link-arg-bin=ClaudeCodePanel={}",
            icons.display()
        );
        println!("cargo:rustc-link-arg-bin=ClaudeCodePanel=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=ClaudeCodePanel=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
