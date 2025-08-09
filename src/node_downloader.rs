use crate::node_version_manager::NodeVersionManager;
use crate::platform::Platform;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use lazy_static::lazy_static;
use log::info;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::fs;
use tokio::io::AsyncWriteExt;

lazy_static! {
    static ref NODE_VERSION_CACHE: Mutex<HashMap<String, PathBuf>> = Mutex::new(HashMap::new());
}

pub struct NodeDownloader {
    platform: Platform,
    cache_dir: PathBuf,
    node_version: String,
}

impl NodeDownloader {
    pub async fn new_with_persistent_cache(version_spec: &str) -> Result<Self> {
        let cache_dir = Self::get_persistent_cache_dir()?;
        let version_resolver = NodeVersionManager::new();

        // Resolve the version specification to a concrete version
        let resolved_version = match parse_full_version_spec(version_spec) {
            Some(full) => full,
            None => version_resolver
                .resolve_version(version_spec, false)
                .await
                .context(format!(
                    "Failed to resolve Node.js version '{version_spec}'"
                ))?,
        };

        info!("Resolved '{version_spec}' to Node.js version {resolved_version}");

        Ok(Self {
            platform: Platform::current(),
            cache_dir,
            node_version: resolved_version,
        })
    }

    fn get_persistent_cache_dir() -> Result<PathBuf> {
        let cache_dir = if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME") {
            PathBuf::from(cache_home).join("banderole")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".cache").join("banderole")
        } else if let Some(appdata) = std::env::var_os("APPDATA") {
            PathBuf::from(appdata).join("banderole").join("cache")
        } else {
            std::env::temp_dir().join("banderole-cache")
        };

        std::fs::create_dir_all(&cache_dir)
            .context("Failed to create persistent cache directory")?;

        Ok(cache_dir)
    }

    /// Same as ensure_node_binary but reports progress to the provided ProgressBar if any
    pub async fn ensure_node_binary_with_progress(
        &self,
        progress: Option<&ProgressBar>,
    ) -> Result<PathBuf> {
        self.ensure_node_binary_inner(progress).await
    }

    async fn ensure_node_binary_inner(&self, progress: Option<&ProgressBar>) -> Result<PathBuf> {
        // Create cache key for this version and platform
        let cache_key = format!("{}:{}", self.node_version, self.platform);

        // Check in-memory cache first
        {
            let cache = NODE_VERSION_CACHE
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
            if let Some(cached_path) = cache.get(&cache_key) {
                if cached_path.exists() {
                    return Ok(cached_path.clone());
                }
            }
        }

        // Check disk cache
        let node_dir = self
            .cache_dir
            .join("node")
            .join(&self.node_version)
            .join(self.platform.to_string());

        let node_executable = node_dir.join(self.platform.node_executable_path());

        if node_executable.exists() {
            // Update in-memory cache
            let mut cache = NODE_VERSION_CACHE
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
            cache.insert(cache_key, node_executable.clone());
            return Ok(node_executable);
        }

        info!(
            "Fetching Node.js {} for {}",
            self.node_version, self.platform
        );

        // Create cache directory
        fs::create_dir_all(&node_dir)
            .await
            .context("Failed to create node cache directory")?;

        // Download and extract Node.js
        self.download_and_extract_node(&node_dir, progress).await?;

        if !node_executable.exists() {
            anyhow::bail!(
                "Node executable not found after extraction: {}",
                node_executable.display()
            );
        }

        // Make executable on Unix systems
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&node_executable).await?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&node_executable, perms).await?;
        }

        Ok(node_executable)
    }

    async fn download_and_extract_node(
        &self,
        target_dir: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        let archive_name = self.platform.node_archive_name(&self.node_version);
        let url = format!(
            "https://nodejs.org/dist/v{}/{}",
            self.node_version, archive_name
        );

        // Download the archive
        let response = reqwest::get(&url)
            .await
            .context("Failed to download Node.js archive")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to download Node.js: HTTP {}", response.status());
        }

        let archive_path = target_dir.join(&archive_name);
        let mut file = fs::File::create(&archive_path)
            .await
            .context("Failed to create archive file")?;

        // Configure a download progress bar style like the indicatif example
        // Template inspired by download-speed.rs example
        if let (Some(pb), Some(total)) = (progress, response.content_length()) {
            pb.set_style(
                ProgressStyle::with_template(
                    "[ {wide_bar} ] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .unwrap()
                .progress_chars("#>-"),
            );
            pb.set_length(total);
        } else if let Some(pb) = progress {
            pb.set_style(
                ProgressStyle::with_template(
                    "[ {wide_bar} ] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .unwrap()
                .tick_chars("/|\\- "),
            );
        }

        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Failed to read download chunk")?;
            file.write_all(&chunk)
                .await
                .context("Failed to write archive chunk")?;
            if let Some(pb) = progress {
                if pb.length().is_some() {
                    pb.inc(chunk.len() as u64);
                } else {
                    pb.tick();
                }
            }
        }

        file.flush().await.context("Failed to flush archive file")?;
        drop(file);

        // Extract the archive with determinate progress
        if let Some(pb) = progress {
            pb.set_style(
                ProgressStyle::with_template("[ {wide_bar} ] {pos}/{len}")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            pb.set_length(0);
            pb.set_position(0);
        }
        if self.platform.is_windows() {
            self.extract_7z(&archive_path, target_dir, progress).await?;
        } else {
            self.extract_tar_xz(&archive_path, target_dir, progress)
                .await?;
        }

        // Clean up archive
        fs::remove_file(&archive_path)
            .await
            .context("Failed to remove archive file")?;

        // Update in-memory cache with the path to the node executable
        let node_executable_path = target_dir.join(self.platform.node_executable_path());
        let mut cache = NODE_VERSION_CACHE
            .lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
        cache.insert(
            format!("{}:{}", self.node_version, self.platform),
            node_executable_path.clone(),
        );

        // Let caller finish the progress bar for this step
        Ok(())
    }

    async fn extract_7z(
        &self,
        archive_path: &Path,
        target_dir: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        let archive_path = archive_path.to_path_buf();
        let target_dir = target_dir.to_path_buf();
        let progress = progress.cloned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(pb) = &progress {
                pb.set_message("Extracting 7z archive");
            }
            sevenz_rust::decompress_file(&archive_path, &target_dir)
                .context("Failed to extract 7z archive")?;

            // Post-process: many Node archives have a single top-level folder. Flatten it.
            let entries = std::fs::read_dir(&target_dir)
                .context("Failed to read extraction directory")?
                .filter_map(|e| e.ok())
                .collect::<Vec<_>>();
            let top_dirs: Vec<_> = entries
                .iter()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .collect();
            let top_files_exist = entries
                .iter()
                .any(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false));
            if top_dirs.len() == 1 && !top_files_exist {
                let inner = top_dirs[0].path();
                for inner_entry in std::fs::read_dir(&inner)? {
                    let inner_entry = inner_entry?;
                    let from = inner_entry.path();
                    let to = target_dir.join(inner_entry.file_name());
                    std::fs::rename(&from, &to)
                        .or_else(|_| {
                            if inner_entry.file_type()?.is_dir() {
                                std::fs::create_dir_all(&to)?;
                                for sub in walkdir::WalkDir::new(&from).into_iter().flatten() {
                                    let p = sub.path();
                                    let rel = p.strip_prefix(&from).unwrap();
                                    let dest = to.join(rel);
                                    if sub.file_type().is_dir() {
                                        std::fs::create_dir_all(&dest)?;
                                    } else if sub.file_type().is_file() {
                                        if let Some(parent) = dest.parent() {
                                            std::fs::create_dir_all(parent)?;
                                        }
                                        std::fs::copy(p, &dest).map(|_| ())?;
                                    }
                                }
                                Ok(())
                            } else {
                                std::fs::copy(&from, &to).map(|_| ())
                            }
                        })
                        .context("Failed to move extracted files")?;
                }
                let _ = std::fs::remove_dir_all(&inner);
            }
            if let Some(pb) = &progress {
                pb.finish_and_clear();
            }
            Ok(())
        })
        .await??;

        Ok(())
    }

    async fn extract_tar_xz(
        &self,
        archive_path: &Path,
        target_dir: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        let archive_path = archive_path.to_path_buf();
        let target_dir = target_dir.to_path_buf();
        let progress = progress.cloned();

        tokio::task::spawn_blocking(move || -> Result<()> {
            use std::io::Cursor;
            use tar::Archive;

            // Read entire .xz into memory (Node archives are moderate size) and decode
            let mut raw = Vec::new();
            std::fs::File::open(&archive_path)
                .and_then(|mut f| {
                    use std::io::Read;
                    f.read_to_end(&mut raw)
                })
                .context("Failed to read .xz archive")?;

            // Decompress xz -> tar bytes
            let mut tar_bytes: Vec<u8> = Vec::new();
            {
                let mut reader = Cursor::new(&raw);
                lzma_rs::xz_decompress(&mut reader, &mut tar_bytes)
                    .context("Failed to decompress .xz archive")?;
            }

            // First pass: count tar entries
            let mut archive_for_count = Archive::new(Cursor::new(&tar_bytes));
            let mut total_entries: u64 = 0;
            for _ in archive_for_count
                .entries()
                .context("Failed to iterate tar entries")?
            {
                total_entries += 1;
            }

            if let Some(pb) = &progress {
                pb.set_length(total_entries);
                pb.set_position(0);
            }

            // Second pass: extract
            let mut archive = Archive::new(Cursor::new(&tar_bytes));

            for entry in archive.entries().context("Failed to iterate tar entries")? {
                let mut entry = entry.context("Failed to read tar entry")?;
                let path = entry.path().context("Failed to get tar entry path")?;

                // Strip the first component from the path
                let mut components = path.components();
                // discard first component
                components.next();
                let stripped: PathBuf = components.collect();
                if stripped.as_os_str().is_empty() {
                    if let Some(pb) = &progress {
                        pb.inc(1);
                    }
                    continue;
                }
                let outpath = target_dir.join(stripped);
                if let Some(parent) = outpath.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                entry
                    .unpack(&outpath)
                    .context("Failed to unpack tar entry")?;

                if let Some(pb) = &progress {
                    pb.inc(1);
                }
            }

            Ok(())
        })
        .await??;

        Ok(())
    }
}

fn parse_full_version_spec(spec: &str) -> Option<String> {
    let cleaned = spec.trim().trim_start_matches('v');
    let parts: Vec<&str> = cleaned.split('.').collect();
    if parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
        Some(cleaned.to_string())
    } else {
        None
    }
}
