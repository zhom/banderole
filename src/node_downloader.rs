use crate::platform::Platform;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use std::path::{Path, PathBuf};
use lazy_static::lazy_static;

lazy_static! {
    static ref NODE_VERSION_CACHE: Mutex<HashMap<String, PathBuf>> = Mutex::new(HashMap::new());
}

pub struct NodeDownloader {
    platform: Platform,
    cache_dir: PathBuf,
    node_version: String,
}

impl NodeDownloader {
    #[allow(dead_code)]
    pub fn new(cache_dir: PathBuf, node_version: String) -> Self {
        Self {
            platform: Platform::current(),
            cache_dir,
            node_version,
        }
    }

    pub fn new_with_persistent_cache(node_version: String) -> Result<Self> {
        let cache_dir = Self::get_persistent_cache_dir()?;
        Ok(Self {
            platform: Platform::current(),
            cache_dir,
            node_version,
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

    pub async fn ensure_node_binary(&self) -> Result<PathBuf> {
        // Create cache key for this version and platform
        let cache_key = format!("{}:{}", self.node_version, self.platform);
        
        // Check in-memory cache first
        {
            let cache = NODE_VERSION_CACHE.lock().map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
            if let Some(cached_path) = cache.get(&cache_key) {
                if cached_path.exists() {
                    return Ok(cached_path.clone());
                }
            }
        }
        
        // Check disk cache
        let node_dir = self.cache_dir
            .join("node")
            .join(&self.node_version)
            .join(self.platform.to_string());
            
        let node_executable = node_dir.join(self.platform.node_executable_path());

        if node_executable.exists() {
            // Update in-memory cache
            let mut cache = NODE_VERSION_CACHE.lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
            cache.insert(cache_key, node_executable.clone());
            return Ok(node_executable);
        }

        println!(
            "Downloading Node.js {} for {}...",
            self.node_version, self.platform
        );

        // Create cache directory
        fs::create_dir_all(&node_dir)
            .await
            .context("Failed to create node cache directory")?;

        // Download and extract Node.js
        self.download_and_extract_node(&node_dir).await?;

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

    async fn download_and_extract_node(&self, target_dir: &Path) -> Result<()> {
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

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Failed to read download chunk")?;
            file.write_all(&chunk)
                .await
                .context("Failed to write archive chunk")?;
        }

        file.flush().await.context("Failed to flush archive file")?;
        drop(file);

        // Extract the archive
        if self.platform.is_windows() {
            self.extract_zip(&archive_path, target_dir).await?;
        } else {
            self.extract_tar_gz(&archive_path, target_dir).await?;
        }

        // Clean up archive
        fs::remove_file(&archive_path)
            .await
            .context("Failed to remove archive file")?;
            
        // Update in-memory cache with the path to the node executable
        let node_executable_path = target_dir.join(self.platform.node_executable_path());
        let mut cache = NODE_VERSION_CACHE.lock()
            .map_err(|e| anyhow::anyhow!("Failed to acquire cache lock: {}", e))?;
        cache.insert(
            format!("{}:{}", self.node_version, self.platform),
            node_executable_path.clone()
        );

        Ok(())
    }

    async fn extract_zip(&self, archive_path: &Path, target_dir: &Path) -> Result<()> {
        let file = std::fs::File::open(archive_path).context("Failed to open zip archive")?;

        let mut archive = zip::ZipArchive::new(file).context("Failed to read zip archive")?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).context("Failed to read zip entry")?;

            let outpath = match file.enclosed_name() {
                Some(path) => {
                    // Remove the top-level directory from the path
                    let components: Vec<_> = path.components().collect();
                    if components.len() > 1 {
                        target_dir.join(components[1..].iter().collect::<PathBuf>())
                    } else {
                        continue;
                    }
                }
                None => continue,
            };

            if file.is_dir() {
                fs::create_dir_all(&outpath)
                    .await
                    .context("Failed to create directory")?;
            } else {
                if let Some(p) = outpath.parent() {
                    fs::create_dir_all(p)
                        .await
                        .context("Failed to create parent directory")?;
                }

                let mut outfile = fs::File::create(&outpath)
                    .await
                    .context("Failed to create output file")?;

                let mut buffer = Vec::new();
                std::io::copy(&mut file, &mut buffer).context("Failed to read zip entry")?;

                outfile
                    .write_all(&buffer)
                    .await
                    .context("Failed to write output file")?;
            }
        }

        Ok(())
    }

    async fn extract_tar_gz(&self, archive_path: &Path, target_dir: &Path) -> Result<()> {
        let output = tokio::process::Command::new("tar")
            .args(&[
                "-xzf",
                archive_path.to_str().unwrap(),
                "-C",
                target_dir.to_str().unwrap(),
                "--strip-components=1",
            ])
            .output()
            .await
            .context("Failed to execute tar command")?;

        if !output.status.success() {
            anyhow::bail!(
                "tar extraction failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }
}
