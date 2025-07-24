use std::{env, fs, path::Path};

const NODE_VERSION: &str = "22.17.1";

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let platform = get_platform();

    // Write platform info for runtime use
    fs::write(Path::new(&out_dir).join("platform"), platform.to_string()).unwrap();

    println!("cargo:rustc-env=NODE_VERSION={}", NODE_VERSION);
    println!("cargo:rerun-if-changed=build.rs");
}

#[derive(Clone, Copy)]
enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosX64,
    MacosArm64,
    WindowsX64,
    WindowsArm64,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinuxX64 => write!(f, "linux-x64"),
            Self::LinuxArm64 => write!(f, "linux-arm64"),
            Self::MacosX64 => write!(f, "darwin-x64"),
            Self::MacosArm64 => write!(f, "darwin-arm64"),
            Self::WindowsX64 => write!(f, "win32-x64"),
            Self::WindowsArm64 => write!(f, "win32-arm64"),
        }
    }
}

fn get_platform() -> Platform {
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64") => Platform::LinuxX64,
        ("linux", "aarch64") => Platform::LinuxArm64,
        ("macos", "x86_64") => Platform::MacosX64,
        ("macos", "aarch64") => Platform::MacosArm64,
        ("windows", "x86_64") => Platform::WindowsX64,
        ("windows", "aarch64") => Platform::WindowsArm64,
        _ => panic!("Unsupported platform: {}-{}", os, arch),
    }
}
