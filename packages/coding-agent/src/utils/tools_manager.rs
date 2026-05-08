//! Resolve external CLI tools (`fd`, `rg`) by checking $PATH first and then
//! downloading platform-specific binaries from upstream GitHub releases.
//!
//! Behaviour parity with `pi-mono/.../tools-manager.ts`:
//! - Cached binaries live next to the runtime in a per-user directory
//!   (`~/.hand/tools/` here; the TS uses the same convention via
//!   `getBinDir`).
//! - Already-installed system binaries (including `fdfind` on Debian) win
//!   over the cache and skip downloads entirely.
//! - Offline mode (`HAND_OFFLINE=1`) suppresses downloads and returns
//!   `Ok(None)`, matching the TS `PI_OFFLINE` switch.
//! - Download failures become `Ok(None)` rather than errors so callers can
//!   degrade gracefully when GitHub is unreachable.
//!
//! Architecture notes:
//! - HTTP, archive extraction, and filesystem work all sit behind the
//!   [`ToolFetcher`] trait so unit tests can drive the manager without
//!   touching the network.
//! - The default implementation [`ReqwestFetcher`] uses `reqwest` for
//!   HTTP, `flate2`+`tar` for `.tar.gz`, and `zip` for `.zip`.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use thiserror::Error;

/// External tool managed by this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `fd` — sharkdp/fd file finder.
    Fd,
    /// `rg` — BurntSushi/ripgrep grep.
    Ripgrep,
}

impl Tool {
    /// Human-readable display name (matches the upstream project).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Ripgrep => "ripgrep",
        }
    }

    /// On-disk binary name (no extension).
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Ripgrep => "rg",
        }
    }

    /// GitHub `owner/repo` slug for releases.
    pub fn repo(self) -> &'static str {
        match self {
            Self::Fd => "sharkdp/fd",
            Self::Ripgrep => "BurntSushi/ripgrep",
        }
    }

    /// Names to probe in `$PATH` before falling back to the cache.
    /// Debian renames `fd` to `fdfind` for namespace conflicts.
    pub fn system_binary_names(self) -> &'static [&'static str] {
        match self {
            Self::Fd => &["fd", "fdfind"],
            Self::Ripgrep => &["rg"],
        }
    }

    /// Termux package name; consulted when running on Android.
    pub fn termux_package(self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Ripgrep => "ripgrep",
        }
    }

    /// Build the upstream asset filename for a given platform/arch combo.
    ///
    /// Returns `None` when no prebuilt asset exists (e.g. odd arches).
    /// Uses the same naming the TS implementation does so cached binaries
    /// from a Pi install would be reusable in principle.
    pub fn asset_name(self, version: &str, plat: Platform, arch: Arch) -> Option<String> {
        let arch_str = match arch {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
        };
        match (self, plat) {
            (Self::Fd, Platform::Darwin) => {
                Some(format!("fd-v{version}-{arch_str}-apple-darwin.tar.gz"))
            }
            (Self::Fd, Platform::Linux) => {
                Some(format!("fd-v{version}-{arch_str}-unknown-linux-gnu.tar.gz"))
            }
            (Self::Fd, Platform::Windows) => {
                Some(format!("fd-v{version}-{arch_str}-pc-windows-msvc.zip"))
            }
            (Self::Ripgrep, Platform::Darwin) => {
                Some(format!("ripgrep-{version}-{arch_str}-apple-darwin.tar.gz"))
            }
            (Self::Ripgrep, Platform::Linux) => {
                if matches!(arch, Arch::Aarch64) {
                    Some(format!(
                        "ripgrep-{version}-aarch64-unknown-linux-gnu.tar.gz"
                    ))
                } else {
                    Some(format!(
                        "ripgrep-{version}-x86_64-unknown-linux-musl.tar.gz"
                    ))
                }
            }
            (Self::Ripgrep, Platform::Windows) => {
                Some(format!("ripgrep-{version}-{arch_str}-pc-windows-msvc.zip"))
            }
            (_, Platform::Android) => None,
            (_, Platform::Other) => None,
        }
    }

    /// Tag prefix for the upstream release (`v` for fd, empty for ripgrep).
    pub fn tag_prefix(self) -> &'static str {
        match self {
            Self::Fd => "v",
            Self::Ripgrep => "",
        }
    }
}

/// Coarse platform classification used for asset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Darwin,
    Linux,
    Windows,
    /// Android (Termux). Linux binaries don't run because of Bionic libc.
    Android,
    /// Anything else; no prebuilt asset.
    Other,
}

impl Platform {
    /// Detect the current host platform.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    /// Executable extension (`.exe` on Windows, empty elsewhere).
    pub fn binary_extension(self) -> &'static str {
        match self {
            Self::Windows => ".exe",
            _ => "",
        }
    }
}

/// Coarse CPU arch classification used for asset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Aarch64,
    X86_64,
}

impl Arch {
    /// Detect the current host arch. Defaults to `X86_64` on unknown
    /// architectures so we still attempt a download (it may still work
    /// under emulation; failures are non-fatal).
    pub fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else {
            Self::X86_64
        }
    }
}

/// Errors from [`ensure_tool`].
///
/// Most "soft" failures (offline, missing asset, download error) surface as
/// `Ok(None)` to mirror the TS `console.log` + return-null pattern. The
/// errors here are reserved for unexpected I/O problems where the caller
/// genuinely cannot proceed — directory creation, permission denials, etc.
#[derive(Debug, Error)]
pub enum ToolsError {
    #[error("tools directory I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("home directory could not be resolved")]
    NoHomeDir,
}

/// Filesystem-and-network operations the manager needs, behind a trait so
/// tests can stub the network and offline-mode logic in isolation.
#[async_trait]
pub trait ToolFetcher: Send + Sync {
    /// Resolve the latest release version for a `owner/repo` slug, with
    /// any `tag_prefix` (e.g. `v`) stripped.
    async fn latest_version(&self, repo: &str) -> Result<String, FetchError>;

    /// Download `url` into `dest`. The full body is materialised on disk.
    async fn download_to_file(&self, url: &str, dest: &Path) -> Result<(), FetchError>;
}

/// Errors from [`ToolFetcher`] operations. Treated as soft failures by
/// [`ensure_tool`] (returns `Ok(None)`).
#[derive(Debug, Error)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("response body could not be parsed: {0}")]
    Parse(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Real-network implementation of [`ToolFetcher`] using `reqwest`.
pub struct ReqwestFetcher {
    client: reqwest::Client,
    user_agent: String,
}

impl ReqwestFetcher {
    /// Construct a fetcher with the given user agent. GitHub rejects
    /// requests without one.
    pub fn new(user_agent: impl Into<String>) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| FetchError::Http(e.to_string()))?;
        Ok(Self {
            client,
            user_agent: user_agent.into(),
        })
    }
}

#[async_trait]
impl ToolFetcher for ReqwestFetcher {
    async fn latest_version(&self, repo: &str) -> Result<String, FetchError> {
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let resp = self
            .client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("status {}", resp.status())));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| FetchError::Parse(e.to_string()))?;
        let tag = body
            .get("tag_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FetchError::Parse("missing tag_name".to_string()))?;
        Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
    }

    async fn download_to_file(&self, url: &str, dest: &Path) -> Result<(), FetchError> {
        let resp = self
            .client
            .get(url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!("status {}", resp.status())));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        fs::write(dest, &bytes)?;
        Ok(())
    }
}

/// Outcome of [`ensure_tool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPath {
    /// Tool is on `$PATH`; spawn it by name.
    System(String),
    /// Tool is cached locally; use this absolute path.
    Cached(PathBuf),
}

impl ToolPath {
    /// Spawnable command/path for `Command::new`.
    pub fn invocation(&self) -> &std::ffi::OsStr {
        match self {
            Self::System(name) => std::ffi::OsStr::new(name),
            Self::Cached(path) => path.as_os_str(),
        }
    }
}

/// Resolve a tool, downloading and extracting if necessary.
///
/// Returns:
/// - `Ok(Some(ToolPath::System(name)))` when the tool is on `$PATH`.
/// - `Ok(Some(ToolPath::Cached(path)))` when a cached binary is present
///   (or has just been downloaded).
/// - `Ok(None)` when the tool is not available and cannot be installed
///   (offline mode, Android without `pkg`, unsupported platform, network
///   failure, archive lacks the expected binary). Callers should fall back
///   to alternative implementations.
///
/// `tools_dir` is the directory cached binaries live in (typically
/// [`default_tools_dir`]). The directory is created on demand.
pub async fn ensure_tool(
    tool: Tool,
    tools_dir: &Path,
    fetcher: &dyn ToolFetcher,
    options: EnsureOptions,
) -> Result<Option<ToolPath>, ToolsError> {
    if let Some(p) = locate_existing(tool, tools_dir, &options) {
        return Ok(Some(p));
    }

    if options.offline {
        return Ok(None);
    }
    if options.platform == Platform::Android {
        return Ok(None);
    }
    // Probe early — if the platform/arch combo has no prebuilt asset we
    // shouldn't even hit the network.
    if tool
        .asset_name("0", options.platform, options.arch)
        .is_none()
    {
        return Ok(None);
    }

    fs::create_dir_all(tools_dir)?;

    let version = match fetcher.latest_version(tool.repo()).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let asset = match tool.asset_name(&version, options.platform, options.arch) {
        Some(name) => name,
        None => return Ok(None),
    };

    let url = format!(
        "https://github.com/{}/releases/download/{}{version}/{asset}",
        tool.repo(),
        tool.tag_prefix()
    );

    let archive_path = tools_dir.join(&asset);
    if let Err(_e) = fetcher.download_to_file(&url, &archive_path).await {
        let _ = fs::remove_file(&archive_path);
        return Ok(None);
    }

    // Extract into a unique temp dir; concurrent ensure_tool calls (fd + rg
    // at startup) must not share a directory.
    let extract_dir = tempfile::tempdir_in(tools_dir)?;
    let extracted = match extract_archive(&archive_path, extract_dir.path(), &asset) {
        Ok(()) => find_binary_in(extract_dir.path(), tool, options.platform),
        Err(_) => None,
    };

    let _ = fs::remove_file(&archive_path);

    let extracted = match extracted {
        Some(p) => p,
        None => return Ok(None),
    };

    let binary_dest = tools_dir.join(format!(
        "{}{}",
        tool.binary_name(),
        options.platform.binary_extension()
    ));
    fs::rename(&extracted, &binary_dest)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&binary_dest)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_dest, perms)?;
    }

    Ok(Some(ToolPath::Cached(binary_dest)))
}

/// Knobs controlling [`ensure_tool`] without bloating its signature.
#[derive(Debug, Clone)]
pub struct EnsureOptions {
    /// Skip downloads even when a binary is missing. Mirrors `HAND_OFFLINE=1`
    /// / `PI_OFFLINE=1` in the TS implementation.
    pub offline: bool,
    /// Target platform. Defaults to the host via [`Platform::current`].
    pub platform: Platform,
    /// Target arch. Defaults to the host via [`Arch::current`].
    pub arch: Arch,
}

impl EnsureOptions {
    /// Probe environment + host to derive defaults.
    pub fn from_env() -> Self {
        Self {
            offline: is_offline_env(),
            platform: Platform::current(),
            arch: Arch::current(),
        }
    }
}

impl Default for EnsureOptions {
    fn default() -> Self {
        Self::from_env()
    }
}

/// `~/.hand/tools/` — the canonical cache directory for managed binaries.
pub fn default_tools_dir() -> Result<PathBuf, ToolsError> {
    let home = dirs::home_dir().ok_or(ToolsError::NoHomeDir)?;
    Ok(home.join(".hand").join("tools"))
}

fn is_offline_env() -> bool {
    match std::env::var("HAND_OFFLINE")
        .or_else(|_| std::env::var("PI_OFFLINE"))
        .ok()
    {
        Some(v) => {
            let lc = v.to_ascii_lowercase();
            v == "1" || lc == "true" || lc == "yes"
        }
        None => false,
    }
}

/// Look for an already-installed binary, in priority order:
/// 1. The cache directory (`tools_dir`).
/// 2. `$PATH`, trying every alias in `system_binary_names`.
fn locate_existing(tool: Tool, tools_dir: &Path, options: &EnsureOptions) -> Option<ToolPath> {
    let cached = tools_dir.join(format!(
        "{}{}",
        tool.binary_name(),
        options.platform.binary_extension()
    ));
    if cached.exists() {
        return Some(ToolPath::Cached(cached));
    }

    for name in tool.system_binary_names() {
        if command_exists(name) {
            return Some(ToolPath::System((*name).to_string()));
        }
    }
    None
}

/// Run `<cmd> --version` to detect availability. Used in lieu of `which`
/// so we don't need a second crate.
fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Recursively look for `binary_name` under `root` (depth-first). Some
/// archives place the binary at the top level, others nest it under a
/// version-named subdirectory.
fn find_binary_in(root: &Path, tool: Tool, plat: Platform) -> Option<PathBuf> {
    let target = format!("{}{}", tool.binary_name(), plat.binary_extension());
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_file() && path.file_name().and_then(|s| s.to_str()) == Some(&target) {
                return Some(path);
            }
            if file_type.is_dir() {
                stack.push(path);
            }
        }
    }
    None
}

/// Pick an extractor based on the file extension and run it.
fn extract_archive(archive: &Path, dest: &Path, asset_name: &str) -> Result<(), io::Error> {
    if asset_name.ends_with(".tar.gz") {
        extract_tar_gz(archive, dest)
    } else if asset_name.ends_with(".zip") {
        extract_zip(archive, dest)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported archive format: {asset_name}"),
        ))
    }
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), io::Error> {
    let file = fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(dest)?;
    Ok(())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), io::Error> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let entry_path = match entry.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let out_path = dest.join(entry_path);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&out_path)?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        io::Write::write_all(&mut out, &buf)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mock fetcher: serves a fixed version + a recorded archive bytes for
    /// the expected URL. Other URLs fail with an HTTP error so accidental
    /// network calls during tests are obvious.
    struct MockFetcher {
        version: String,
        archives: Mutex<Vec<(String, Vec<u8>)>>,
        version_calls: Mutex<usize>,
        download_calls: Mutex<Vec<String>>,
    }

    impl MockFetcher {
        fn new(version: &str) -> Self {
            Self {
                version: version.to_string(),
                archives: Mutex::new(Vec::new()),
                version_calls: Mutex::new(0),
                download_calls: Mutex::new(Vec::new()),
            }
        }

        fn with_archive(self, url: &str, bytes: Vec<u8>) -> Self {
            self.archives.lock().unwrap().push((url.to_string(), bytes));
            self
        }
    }

    #[async_trait]
    impl ToolFetcher for MockFetcher {
        async fn latest_version(&self, _repo: &str) -> Result<String, FetchError> {
            *self.version_calls.lock().unwrap() += 1;
            Ok(self.version.clone())
        }

        async fn download_to_file(&self, url: &str, dest: &Path) -> Result<(), FetchError> {
            self.download_calls.lock().unwrap().push(url.to_string());
            let archives = self.archives.lock().unwrap();
            let bytes = archives
                .iter()
                .find(|(u, _)| u == url)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| FetchError::Http(format!("unexpected URL: {url}")))?;
            fs::write(dest, &bytes).map_err(FetchError::Io)?;
            Ok(())
        }
    }

    /// Build a synthetic .tar.gz containing a single file `<binary_name>` so
    /// the manager finds it during extraction.
    fn build_tar_gz(binary_name: &str, contents: &[u8]) -> Vec<u8> {
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut header = tar::Header::new_gnu();
            header.set_path(binary_name).expect("tar set_path");
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, contents).expect("append entry");
            builder.finish().expect("finalize tar");
        }
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut encoder =
                flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
            encoder.write_all(&tar_buf).expect("gz write");
            encoder.finish().expect("gz finish");
        }
        gz
    }

    #[tokio::test]
    async fn cached_binary_is_returned_without_network() {
        let dir = TempDir::new().expect("tmp");
        let tools_dir = dir.path().join("tools");
        fs::create_dir_all(&tools_dir).expect("mkdir");
        let cached = tools_dir.join("rg");
        fs::write(&cached, b"#!/bin/sh\n").expect("seed");

        let fetcher = MockFetcher::new("13.0.0");
        let options = EnsureOptions {
            offline: false,
            platform: Platform::Linux,
            arch: Arch::X86_64,
        };

        let result = ensure_tool(Tool::Ripgrep, &tools_dir, &fetcher, options)
            .await
            .expect("ensure ok");
        assert_eq!(result, Some(ToolPath::Cached(cached)));
        assert_eq!(*fetcher.version_calls.lock().unwrap(), 0);
        assert_eq!(fetcher.download_calls.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn offline_mode_skips_download() {
        let dir = TempDir::new().expect("tmp");
        // No cached binary, no system fd; offline mode should yield None.
        let fetcher = MockFetcher::new("9.0.0");
        let options = EnsureOptions {
            offline: true,
            platform: Platform::Linux,
            arch: Arch::X86_64,
        };
        let result = ensure_tool(Tool::Fd, dir.path(), &fetcher, options)
            .await
            .expect("ensure ok");
        // Result is either None (no fd anywhere) or System("fd"/"fdfind") if
        // the test host happens to have it installed. Both are valid; what
        // matters is the fetcher was untouched.
        assert!(matches!(result, None | Some(ToolPath::System(_))));
        assert_eq!(*fetcher.version_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn android_skips_download() {
        let dir = TempDir::new().expect("tmp");
        let fetcher = MockFetcher::new("9.0.0");
        let options = EnsureOptions {
            offline: false,
            platform: Platform::Android,
            arch: Arch::Aarch64,
        };
        // Cache empty; Android cannot use Linux binaries → None.
        let result = ensure_tool(Tool::Fd, dir.path(), &fetcher, options)
            .await
            .expect("ensure ok");
        assert!(matches!(result, None | Some(ToolPath::System(_))));
        assert_eq!(*fetcher.version_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn download_extracts_binary_into_cache() {
        let dir = TempDir::new().expect("tmp");
        let tools_dir = dir.path().join("tools");

        let archive = build_tar_gz("rg", b"\x7fELF binary stub");
        let url = "https://github.com/BurntSushi/ripgrep/releases/download/13.0.0/ripgrep-13.0.0-x86_64-unknown-linux-musl.tar.gz";
        let fetcher = MockFetcher::new("13.0.0").with_archive(url, archive);

        let options = EnsureOptions {
            offline: false,
            platform: Platform::Linux,
            arch: Arch::X86_64,
        };
        let result = ensure_tool(Tool::Ripgrep, &tools_dir, &fetcher, options)
            .await
            .expect("ensure ok");

        let path = match result {
            Some(ToolPath::Cached(p)) => p,
            other => panic!("expected Cached path, got {other:?}"),
        };
        assert!(path.exists(), "binary should be on disk");
        assert_eq!(fs::read(&path).expect("read"), b"\x7fELF binary stub");
        assert_eq!(*fetcher.version_calls.lock().unwrap(), 1);
        assert_eq!(fetcher.download_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn download_failure_is_soft_none() {
        let dir = TempDir::new().expect("tmp");
        let tools_dir = dir.path().join("tools");
        // Mock has no archive for the expected URL → download returns Err.
        let fetcher = MockFetcher::new("13.0.0");
        let options = EnsureOptions {
            offline: false,
            platform: Platform::Linux,
            arch: Arch::X86_64,
        };
        let result = ensure_tool(Tool::Ripgrep, &tools_dir, &fetcher, options)
            .await
            .expect("ensure ok");
        // No system rg expected in test env (test crate doesn't ship it
        // reliably); but if the host has rg, that's fine — System path is
        // still a valid outcome. Network failure must NOT propagate as Err.
        assert!(matches!(result, None | Some(ToolPath::System(_))));
    }

    #[test]
    fn asset_name_matches_typescript_format() {
        assert_eq!(
            Tool::Fd.asset_name("9.0.0", Platform::Darwin, Arch::Aarch64),
            Some("fd-v9.0.0-aarch64-apple-darwin.tar.gz".to_string())
        );
        assert_eq!(
            Tool::Ripgrep.asset_name("14.1.0", Platform::Linux, Arch::Aarch64),
            Some("ripgrep-14.1.0-aarch64-unknown-linux-gnu.tar.gz".to_string())
        );
        assert_eq!(
            Tool::Ripgrep.asset_name("14.1.0", Platform::Linux, Arch::X86_64),
            Some("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz".to_string())
        );
        assert_eq!(
            Tool::Fd.asset_name("9.0.0", Platform::Windows, Arch::X86_64),
            Some("fd-v9.0.0-x86_64-pc-windows-msvc.zip".to_string())
        );
        assert_eq!(
            Tool::Fd.asset_name("9.0.0", Platform::Android, Arch::Aarch64),
            None
        );
    }

    #[test]
    fn tool_path_invocation_matches_variant() {
        assert_eq!(
            ToolPath::System("fd".to_string()).invocation(),
            std::ffi::OsStr::new("fd")
        );
        let cached = ToolPath::Cached(PathBuf::from("/tmp/rg"));
        assert_eq!(cached.invocation(), std::ffi::OsStr::new("/tmp/rg"));
    }
}
