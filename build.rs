fn main() {
    println!("cargo:rerun-if-changed=resources/app.manifest");
    println!("cargo:rerun-if-changed=experiments/gesture-cache-probe.rs");
    println!("cargo:rerun-if-changed=experiments/Test-BackgroundGestureSettings.ps1");
    let helper = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
        .join("gesture-cache-probe.exe");
    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap())
        .args([
            "--edition",
            "2021",
            "-D",
            "warnings",
            "-O",
            "experiments/gesture-cache-probe.rs",
            "-o",
        ])
        .arg(helper)
        .status()
        .expect("compile the embedded settings-cache helper");
    assert!(status.success(), "settings-cache helper build failed");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("resources/app.manifest");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
}
