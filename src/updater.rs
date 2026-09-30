use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::diagnose;
use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

const GITHUB_API_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";
const RELEASE_ASSET_NAME: &str = "ai-usage-monitor.exe";
const HELPER_EXE_NAME: &str = "updater-helper.exe";
const DOWNLOAD_EXE_NAME: &str = "update-download.exe";
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Debug)]
pub struct ReleaseDescriptor {
    pub latest_version: String,
    asset_url: String,
    asset_name: String,
    checksum_url: Option<String>,
}

#[derive(Debug)]
pub enum UpdateCheckResult {
    UpToDate,
    Available(ReleaseDescriptor),
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// Name of the checksum asset the release workflow publishes.
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";

pub fn handle_cli_mode(args: &[String]) -> Option<i32> {
    if args.len() == 5 && args[1] == "--apply-update" {
        let target = PathBuf::from(&args[2]);
        let source = PathBuf::from(&args[3]);
        let pid = args[4].parse::<u32>().unwrap_or(0);

        return Some(match apply_update(target, source, pid) {
            Ok(()) => 0,
            Err(error) => {
                show_error_message("Update failed", &error);
                1
            }
        });
    }

    None
}

pub fn check_for_updates() -> Result<UpdateCheckResult, String> {
    match fetch_latest_release()? {
        Some(release) => Ok(UpdateCheckResult::Available(release)),
        None => Ok(UpdateCheckResult::UpToDate),
    }
}

/// The running exe with symlinks resolved: WinGet may start it through a link
/// in its `Links` folder, and an update has to replace the file behind it.
fn resolved_current_exe() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Unable to locate current executable: {e}"))?;
    let resolved = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(match resolved.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => resolved,
    })
}

/// A WinGet install updates itself like any other copy, so WinGet's own record
/// of it (version and file hash) is brought in step afterwards. Without that,
/// `winget list` keeps showing the old version, and `winget upgrade` and
/// `winget uninstall` refuse the file as modified.
/// ponytail: per-user installs only; a machine-wide one cannot update itself anyway.
pub fn sync_winget_record() {
    if let Ok(exe) = resolved_current_exe() {
        sync_winget_record_under(r"Software\Microsoft\Windows\CurrentVersion\Uninstall", &exe);
    }
}

fn sync_winget_record_under(uninstall_key: &str, exe: &Path) {
    // WinGet keeps each portable package in a folder named after its record.
    let Some(record) = exe
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
    else {
        return;
    };
    let key = format!(r"{uninstall_key}\{record}");
    let installed_here = registry_string(&key, "TargetFullPath")
        .is_some_and(|target| target.eq_ignore_ascii_case(&exe.to_string_lossy()));
    let version = env!("CARGO_PKG_VERSION");
    if !installed_here || registry_string(&key, "DisplayVersion").as_deref() == Some(version) {
        return;
    }
    let Ok(digest) = std::fs::read(&exe).map_err(|e| e.to_string()).and_then(|bytes| sha256_hex(&bytes)) else {
        return;
    };
    // The version last, so an interrupted sync is retried on the next start.
    if set_registry_string(&key, "SHA256", &digest) {
        set_registry_string(&key, "DisplayVersion", version);
        diagnose::log(format!("WinGet record {record} brought to {version}"));
    }
}

fn registry_string(key: &str, name: &str) -> Option<String> {
    let key = wide_str(key);
    let name = wide_str(name);
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn set_registry_string(key: &str, name: &str, value: &str) -> bool {
    let key = wide_str(key);
    let name = wide_str(name);
    let value = wide_str(value);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key.as_ptr()),
            PCWSTR::from_raw(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            (value.len() * 2) as u32,
        )
        .is_ok()
    }
}

/// False for a copy in a folder it cannot write to, such as a machine-wide
/// WinGet install, which unattended updates then leave alone.
pub fn can_update_in_place() -> bool {
    resolved_current_exe().is_ok_and(|exe| ensure_target_location_writable(&exe).is_ok())
}

pub fn begin_self_update(release: &ReleaseDescriptor) -> Result<(), String> {
    let current_exe = resolved_current_exe()?;
    ensure_target_location_writable(&current_exe)?;

    let stage_dir = updates_dir()?;
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("Unable to create updater working directory: {e}"))?;

    let helper_path = stage_dir.join(HELPER_EXE_NAME);
    let download_path = stage_dir.join(DOWNLOAD_EXE_NAME);
    let partial_download_path = stage_dir.join(format!("{DOWNLOAD_EXE_NAME}.part"));

    if helper_path.exists() {
        let _ = std::fs::remove_file(&helper_path);
    }
    if download_path.exists() {
        let _ = std::fs::remove_file(&download_path);
    }
    if partial_download_path.exists() {
        let _ = std::fs::remove_file(&partial_download_path);
    }

    download_release_asset(release, &partial_download_path, &download_path)?;
    std::fs::copy(&current_exe, &helper_path)
        .map_err(|e| format!("Unable to prepare updater helper: {e}"))?;

    let pid = std::process::id().to_string();
    let target = current_exe.to_string_lossy().to_string();
    let source = download_path.to_string_lossy().to_string();

    Command::new(&helper_path)
        .arg("--apply-update")
        .arg(target)
        .arg(source)
        .arg(pid)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Unable to launch updater helper: {e}"))?;

    Ok(())
}

fn apply_update(target: PathBuf, source: PathBuf, pid: u32) -> Result<(), String> {
    if !source.exists() {
        return Err(format!(
            "Downloaded update not found at {}",
            source.display()
        ));
    }

    let _ = wait_for_process_exit(pid, Duration::from_secs(30));
    replace_target_binary(&target, &source)?;
    relaunch_target(&target)?;
    let _ = std::fs::remove_file(&source);

    Ok(())
}

fn fetch_latest_release() -> Result<Option<ReleaseDescriptor>, String> {
    let (owner, repo) = github_repo()?;
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let agent = build_agent()?;

    let response = agent
        .get(&url)
        .set("Accept", GITHUB_API_ACCEPT)
        .set("User-Agent", user_agent())
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(|e| format!("Unable to check GitHub releases: {e}"))?;

    let release: GitHubRelease = response
        .into_json()
        .map_err(|e| format!("Unable to parse GitHub release data: {e}"))?;

    let latest_version = release.tag_name.trim_start_matches('v').to_string();
    if !is_version_newer(&latest_version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }

    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(RELEASE_ASSET_NAME))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.to_ascii_lowercase().ends_with(".exe"))
        })
        .ok_or_else(|| {
            "No Windows executable asset was found in the latest release.".to_string()
        })?;

    // An update replaces the running executable, so the download location is not
    // taken on trust from the response: it has to live under this repository's
    // own releases.
    if !is_own_release_asset(&asset.browser_download_url, owner, repo) {
        return Err(format!(
            "release asset is not hosted under {owner}/{repo}: {}",
            asset.browser_download_url
        ));
    }

    let checksum_url = release
        .assets
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(CHECKSUM_ASSET_NAME))
        .map(|candidate| candidate.browser_download_url.clone());

    Ok(Some(ReleaseDescriptor {
        latest_version,
        asset_url: asset.browser_download_url.clone(),
        asset_name: asset.name.clone(),
        checksum_url,
    }))
}

/// Whether a download URL belongs to this repository's release storage.
fn is_own_release_asset(url: &str, owner: &str, repo: &str) -> bool {
    let expected = format!("https://github.com/{owner}/{repo}/releases/download/");
    url.starts_with(&expected)
}

/// Pull the expected digest for `asset_name` out of a `sha256sum`-style listing.
fn expected_digest(listing: &str, asset_name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if name.eq_ignore_ascii_case(asset_name) && digest.len() == 64 {
            Some(digest.to_ascii_lowercase())
        } else {
            None
        }
    })
}

/// SHA-256 through the platform's own provider, to avoid pulling a crypto crate
/// into a dependency tree this small.
fn sha256_hex(bytes: &[u8]) -> Result<String, String> {
    unsafe {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        let status = BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_SHA256_ALGORITHM,
            PCWSTR::null(),
            BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0),
        );
        if status.is_err() {
            return Err(format!("unable to open the SHA-256 provider: {status:?}"));
        }

        let mut digest = [0u8; 32];
        let status = BCryptHash(algorithm, None, bytes, &mut digest);
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
        if status.is_err() {
            return Err(format!("unable to hash the download: {status:?}"));
        }

        Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

fn build_agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new()
        .map_err(|e| format!("Unable to initialize TLS support for update checks: {e}"))?;
    Ok(ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .tls_connector(std::sync::Arc::new(tls))
        .build())
}

fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let agent = build_agent()?;
    let response = agent
        .get(url)
        .set("User-Agent", user_agent())
        .call()
        .map_err(|e| format!("Unable to download {url}: {e}"))?;

    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Unable to read {url}: {e}"))?;
    Ok(bytes)
}

/// Download, verify, and only then put the file where the helper will find it.
/// Verification is mandatory: an update overwrites the running executable, so a
/// release without published checksums is refused rather than trusted.
fn download_release_asset(
    release: &ReleaseDescriptor,
    partial_path: &Path,
    final_path: &Path,
) -> Result<(), String> {
    let checksum_url = release.checksum_url.as_deref().ok_or_else(|| {
        format!(
            "This release publishes no {CHECKSUM_ASSET_NAME}, so the download cannot be verified."
        )
    })?;

    let listing = fetch_bytes(checksum_url, 64 * 1024)?;
    let listing = String::from_utf8(listing)
        .map_err(|_| format!("{CHECKSUM_ASSET_NAME} is not text"))?;
    let expected = expected_digest(&listing, &release.asset_name).ok_or_else(|| {
        format!(
            "{CHECKSUM_ASSET_NAME} lists no digest for {}",
            release.asset_name
        )
    })?;

    let payload = fetch_bytes(&release.asset_url, 128 * 1024 * 1024)?;
    let actual = sha256_hex(&payload)?;
    if actual != expected {
        return Err(format!(
            "The downloaded update does not match its published checksum (expected {expected}, got {actual}); it has not been installed."
        ));
    }

    let mut file = File::create(partial_path)
        .map_err(|e| format!("Unable to create temporary download file: {e}"))?;
    file.write_all(&payload)
        .map_err(|e| format!("Unable to write the downloaded update: {e}"))?;
    file.flush()
        .map_err(|e| format!("Unable to finalize the downloaded update: {e}"))?;
    drop(file);

    std::fs::rename(partial_path, final_path)
        .map_err(|e| format!("Unable to finalize the downloaded update file: {e}"))?;

    Ok(())
}

fn replace_target_binary(target: &Path, source: &Path) -> Result<(), String> {
    let backup_path = backup_path_for(target);
    let mut last_error = None;

    for _ in 0..60 {
        let _ = std::fs::remove_file(&backup_path);

        let renamed_existing = match std::fs::rename(target, &backup_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        match std::fs::copy(source, target) {
            Ok(_) => {
                let _ = std::fs::remove_file(&backup_path);
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error);
                let _ = std::fs::remove_file(target);
                if renamed_existing {
                    let _ = std::fs::rename(&backup_path, target);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }

    Err(format!(
        "Unable to replace {}. {}",
        target.display(),
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| {
                "The file may still be locked or the install directory may not be writable."
                    .to_string()
            })
    ))
}

fn relaunch_target(target: &Path) -> Result<(), String> {
    let mut command = Command::new(target);
    if let Some(parent) = target.parent() {
        command.current_dir(parent);
    }

    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            format!(
                "The update was installed, but the app could not be restarted automatically: {e}"
            )
        })?;

    Ok(())
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> Result<(), String> {
    if pid == 0 {
        return Ok(());
    }

    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)
            .map_err(|e| format!("Unable to monitor the running app process: {e}"))?;

        let result = WaitForSingleObject(handle, timeout.as_millis().min(u32::MAX as u128) as u32);
        let _ = windows::Win32::Foundation::CloseHandle(handle);

        if result == WAIT_OBJECT_0 {
            Ok(())
        } else if result == WAIT_TIMEOUT {
            Err("Timed out waiting for the running app to exit.".to_string())
        } else {
            Err("Unable to confirm that the running app has exited.".to_string())
        }
    }
}

fn updates_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|dir| dir.join("AIUsageMonitor").join("updates"))
        .or_else(|| {
            Some(
                std::env::temp_dir()
                    .join("AIUsageMonitor")
                    .join("updates"),
            )
        })
        .ok_or_else(|| "Unable to resolve a writable local updates directory.".to_string())
}

fn backup_path_for(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("app.exe");
    target.with_file_name(format!("{file_name}.old"))
}

fn ensure_target_location_writable(target: &Path) -> Result<(), String> {
    let parent = target.parent().ok_or_else(|| {
        "Unable to determine the install directory for the current executable.".to_string()
    })?;

    let probe_path = parent.join(".__aium_update_probe");
    match File::create(&probe_path) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe_path);
            Ok(())
        }
        Err(error) => Err(format!(
            "The current install location is not writable. Move the app to a user-writable folder or install it somewhere outside Program Files. {error}"
        )),
    }
}

fn github_repo() -> Result<(&'static str, &'static str), String> {
    let repository = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let parts: Vec<&str> = repository.split('/').collect();
    if parts.len() < 2 {
        return Err("Package repository URL is not configured for GitHub releases.".to_string());
    }

    let owner = parts[parts.len() - 2];
    let repo = parts[parts.len() - 1];
    if owner.is_empty() || repo.is_empty() {
        return Err("Package repository URL is not configured for GitHub releases.".to_string());
    }

    Ok((owner, repo))
}

fn user_agent() -> &'static str {
    concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"))
}

fn is_version_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(version: &str) -> (u32, u32, u32) {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.').map(|part| part.parse::<u32>().unwrap_or(0));

    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn show_error_message(title: &str, message: &str) {
    unsafe {
        let title_wide = wide_str(title);
        let message_wide = wide_str(message);
        let _ = MessageBoxW(
            HWND::default(),
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide_str(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_checks_target_this_repository() {
        let (owner, repo) = github_repo().expect("repository should be configured");
        assert_eq!(owner, "hadufer");
        assert_eq!(repo, "ai-usage-monitor");
    }

    #[test]
    fn a_download_url_outside_this_repository_is_refused() {
        assert!(is_own_release_asset(
            "https://github.com/hadufer/ai-usage-monitor/releases/download/v1.5.4/ai-usage-monitor.exe",
            "hadufer",
            "ai-usage-monitor"
        ));
        // What a tampered or redirected API response would look like.
        assert!(!is_own_release_asset(
            "https://example.com/hadufer/ai-usage-monitor/releases/download/v1/x.exe",
            "hadufer",
            "ai-usage-monitor"
        ));
        assert!(!is_own_release_asset(
            "https://github.com/someone-else/ai-usage-monitor/releases/download/v1/x.exe",
            "hadufer",
            "ai-usage-monitor"
        ));
        assert!(!is_own_release_asset(
            "http://github.com/hadufer/ai-usage-monitor/releases/download/v1/x.exe",
            "hadufer",
            "ai-usage-monitor"
        ));
    }

    #[test]
    fn the_digest_is_taken_from_the_matching_line_only() {
        let listing = "1111111111111111111111111111111111111111111111111111111111111111  other-file.zip
513c211ac265b5a6770724093d6895dd8012dc68f4f9619af93d2d8ecddcad33 *ai-usage-monitor.exe
";
        assert_eq!(
            expected_digest(listing, "ai-usage-monitor.exe").as_deref(),
            Some("513c211ac265b5a6770724093d6895dd8012dc68f4f9619af93d2d8ecddcad33")
        );
        assert!(expected_digest(listing, "not-listed.exe").is_none());
        assert!(expected_digest("deadbeef  ai-usage-monitor.exe", "ai-usage-monitor.exe").is_none());
    }

    #[test]
    fn the_platform_digest_matches_a_known_value() {
        // SHA-256 of "abc", the canonical test vector.
        assert_eq!(
            sha256_hex(b"abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_self_updated_winget_copy_brings_its_winget_record_in_step() {
        let root = format!(r"Software\AIUsageMonitorTest{}", std::process::id());
        let record = "hadufer.AIUsageMonitor_Test";
        let base = std::env::temp_dir().join(format!("aium-test-{}", std::process::id()));
        let dir = base.join(record);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("ai-usage-monitor.exe");
        std::fs::write(&exe, b"abc").unwrap();
        let key = format!(r"{root}\{record}");
        assert!(set_registry_string(&key, "TargetFullPath", &exe.to_string_lossy()));
        assert!(set_registry_string(&key, "DisplayVersion", "0.0.1"));
        assert!(set_registry_string(&key, "SHA256", "stale"));

        // A copy in a folder of the same name that the record does not point at
        // leaves it alone.
        let other = base.join("sub").join(record).join("ai-usage-monitor.exe");
        sync_winget_record_under(&root, &other);
        let untouched = registry_string(&key, "DisplayVersion");

        sync_winget_record_under(&root, &exe);
        let version = registry_string(&key, "DisplayVersion");
        let digest = registry_string(&key, "SHA256");

        let _ = Command::new("reg")
            .args(["delete", &format!(r"HKCU\{root}"), "/f"])
            .output();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(untouched.as_deref(), Some("0.0.1"));
        assert_eq!(version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            digest.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn a_prerelease_suffix_would_never_read_as_newer() {
        assert!(!is_version_newer("1.5.0-fork.1", "1.5.0"));
        assert!(is_version_newer("1.5.1", "1.5.0"));
    }
}
