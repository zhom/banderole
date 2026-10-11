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

    /// The platform whose Node.js build to use for `version`.
    ///
    /// Official arm64 builds start with Node 20 on Windows and Node 16 on macOS. For older
    /// versions this gives the x64 build, which Windows on ARM and Rosetta 2 run under
    /// emulation. Without it, such a version fails with a download error.
    pub fn node_build_platform(&self, version: &str) -> Platform {
        let major = version
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|m| m.parse::<u32>().ok());
        match (self, major) {
            (Platform::WindowsArm64, Some(major)) if major < 20 => Platform::WindowsX64,
            (Platform::MacosArm64, Some(major)) if major < 16 => Platform::MacosX64,
            _ => *self,
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

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn test_node_build_platform_falls_back_to_x64() {
        assert_eq!(
            Platform::WindowsArm64.node_build_platform("18.17.0"),
            Platform::WindowsX64
        );
        assert_eq!(
            Platform::WindowsArm64.node_build_platform("22.17.1"),
            Platform::WindowsArm64
        );
        assert_eq!(
            Platform::MacosArm64.node_build_platform("14.21.3"),
            Platform::MacosX64
        );
        assert_eq!(
            Platform::MacosArm64.node_build_platform("18.17.0"),
            Platform::MacosArm64
        );
        assert_eq!(
            Platform::LinuxArm64.node_build_platform("14.21.3"),
            Platform::LinuxArm64
        );
    }
}
