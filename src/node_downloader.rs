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
            self.extract_zip(&archive_path, target_dir, progress)
                .await?;
        } else {
            self.extract_tar_gz(&archive_path, target_dir, progress)
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

    async fn extract_zip(
        &self,
        archive_path: &Path,
        target_dir: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        let archive_path = archive_path.to_path_buf();
        let target_dir = target_dir.to_path_buf();
        let progress = progress.cloned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = std::fs::File::open(&archive_path).context("Failed to open zip archive")?;
            let mut archive = zip::ZipArchive::new(file).context("Failed to read zip archive")?;

            if let Some(pb) = &progress {
                pb.set_length(archive.len() as u64);
                pb.set_position(0);
            }

            for i in 0..archive.len() {
                let mut file = archive.by_index(i).context("Failed to read zip entry")?;

                let outpath = match file.enclosed_name() {
                    Some(path) => {
                        let components: Vec<_> = path.components().collect();
                        if components.len() > 1 {
                            target_dir.join(components[1..].iter().collect::<PathBuf>())
                        } else {
                            if let Some(pb) = &progress {
                                pb.inc(1);
                            }
                            continue;
                        }
                    }
                    None => {
                        if let Some(pb) = &progress {
                            pb.inc(1);
                        }
                        continue;
                    }
                };

                if file.is_dir() {
                    std::fs::create_dir_all(&outpath).context("Failed to create directory")?;
                } else {
                    if let Some(p) = outpath.parent() {
                        std::fs::create_dir_all(p).context("Failed to create parent directory")?;
                    }

                    let mut outfile =
                        std::fs::File::create(&outpath).context("Failed to create output file")?;

                    std::io::copy(&mut file, &mut outfile)
                        .context("Failed to extract zip entry")?;
                }

                if let Some(pb) = &progress {
                    pb.inc(1);
                }
            }

            Ok(())
        })
        .await??;

        Ok(())
    }

    async fn extract_tar_gz(
        &self,
        archive_path: &Path,
        target_dir: &Path,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        let archive_path = archive_path.to_path_buf();
        let target_dir = target_dir.to_path_buf();
        let progress = progress.cloned();

        tokio::task::spawn_blocking(move || -> Result<()> {
            use flate2::read::GzDecoder;
            use tar::Archive;

            // First pass: count entries
            let file_for_count =
                std::fs::File::open(&archive_path).context("Failed to open tar.gz for counting")?;
            let decoder_for_count = GzDecoder::new(file_for_count);
            let mut archive_for_count = Archive::new(decoder_for_count);
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
            let file = std::fs::File::open(&archive_path).context("Failed to open tar.gz")?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);

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
