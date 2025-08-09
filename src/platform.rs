use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosX64,
    MacosArm64,
    WindowsX64,
    WindowsArm64,
}

impl Platform {
    pub fn current() -> Self {
        let os = env::consts::OS;
        let arch = env::consts::ARCH;

        match (os, arch) {
            ("linux", "x86_64") => Platform::LinuxX64,
            ("linux", "aarch64") => Platform::LinuxArm64,
            ("macos", "x86_64") => Platform::MacosX64,
            ("macos", "aarch64") => Platform::MacosArm64,
            ("windows", "x86_64") => Platform::WindowsX64,
            ("windows", "aarch64") => Platform::WindowsArm64,
            _ => panic!("Unsupported platform: {os}-{arch}"),
        }
    }

    pub fn node_archive_name(&self, version: &str) -> String {
        match self {
            Platform::LinuxX64 => format!("node-v{version}-linux-x64.tar.xz"),
            Platform::LinuxArm64 => format!("node-v{version}-linux-arm64.tar.xz"),
            Platform::MacosX64 => format!("node-v{version}-darwin-x64.tar.xz"),
            Platform::MacosArm64 => format!("node-v{version}-darwin-arm64.tar.xz"),
            Platform::WindowsX64 => format!("node-v{version}-win-x64.7z"),
            Platform::WindowsArm64 => format!("node-v{version}-win-arm64.7z"),
        }
    }

    pub fn node_executable_path(&self) -> PathBuf {
        match self {
            Platform::WindowsX64 | Platform::WindowsArm64 => PathBuf::from("node.exe"),
            _ => PathBuf::from("bin").join("node"),
        }
    }

    pub fn is_windows(&self) -> bool {
        matches!(self, Platform::WindowsX64 | Platform::WindowsArm64)
    }
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
