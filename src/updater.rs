use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptHash, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};
use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

const GITHUB_API_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";
const RELEASE_ASSET_NAME: &str = "claude-code-usage-monitor.exe";
const HELPER_EXE_NAME: &str = "updater-helper.exe";
const DOWNLOAD_EXE_NAME: &str = "update-download.exe";
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CREATE_NEW_CONSOLE: u32 = 0x00000010;
// Keep this aligned with the package identifier used in winget-pkgs.
const WINGET_PACKAGE_ID: &str = "hadufer.ClaudeCodeUsageMonitor";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallChannel {
    Portable,
    Winget,
}

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

pub fn current_install_channel() -> InstallChannel {
    match std::env::current_exe() {
        Ok(path) if is_winget_install_path(&path) => InstallChannel::Winget,
        _ => InstallChannel::Portable,
    }
}

pub fn check_for_updates() -> Result<UpdateCheckResult, String> {
    match fetch_latest_release()? {
        Some(release) => Ok(UpdateCheckResult::Available(release)),
        None => Ok(UpdateCheckResult::UpToDate),
    }
}

pub fn begin_winget_update() -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Unable to locate current executable: {e}"))?;
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| "Unable to determine the app directory for restart.".to_string())?;
    let command = winget_upgrade_command(
        std::process::id(),
        &current_exe.to_string_lossy(),
        &current_dir.to_string_lossy(),
    );

    Command::new("powershell.exe")
        .arg("-NoLogo")
        .arg("-Command")
        .arg(&command)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|e| format!("Unable to launch WinGet update command: {e}"))?;

    Ok(())
}

pub fn begin_self_update(release: &ReleaseDescriptor) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Unable to locate current executable: {e}"))?;
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
        .map(|dir| dir.join("ClaudeCodeUsageMonitor").join("updates"))
        .or_else(|| {
            Some(
                std::env::temp_dir()
                    .join("ClaudeCodeUsageMonitor")
                    .join("updates"),
            )
        })
        .ok_or_else(|| "Unable to resolve a writable local updates directory.".to_string())
}

fn winget_upgrade_command(pid: u32, target: &str, working_dir: &str) -> String {
    let target = powershell_single_quoted(target);
    let working_dir = powershell_single_quoted(working_dir);
    let package_id = WINGET_PACKAGE_ID;

    format!(
        concat!(
            "$ErrorActionPreference = 'Stop'; ",
            "$pidToWait = {pid}; ",
            "$target = '{target}'; ",
            "$workingDir = '{working_dir}'; ",
            "try {{ Wait-Process -Id $pidToWait -Timeout 30 -ErrorAction Stop }} catch {{ }}; ",
            "winget upgrade --id {package_id} --exact; ",
            "$exitCode = $LASTEXITCODE; ",
            "if ($exitCode -eq 0) {{ ",
            "Start-Sleep -Seconds 2; ",
            "Start-Process -FilePath $target -WorkingDirectory $workingDir; ",
            "exit 0 ",
            "}}; ",
            "Write-Host ''; ",
            "Write-Host 'WinGet update failed with exit code' $exitCode; ",
            "Read-Host 'Press Enter to close'; ",
            "exit $exitCode"
        ),
        pid = pid,
        target = target,
        working_dir = working_dir,
        package_id = package_id,
    )
}

fn powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
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

    let probe_path = parent.join(".__ccum_update_probe");
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

fn is_winget_install_path(path: &Path) -> bool {
    let normalized_path = normalize_path(path);
    winget_install_roots()
        .into_iter()
        .map(|root| normalize_path(&root))
        .any(|root| normalized_path.starts_with(&root))
}

fn winget_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        roots.push(
            PathBuf::from(local_app_data)
                .join("Microsoft")
                .join("WinGet")
                .join("Packages"),
        );
    }

    if let Ok(program_files) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("WinGet").join("Packages"));
    } else {
        roots.push(PathBuf::from(r"C:\Program Files\WinGet\Packages"));
    }

    if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
        roots.push(
            PathBuf::from(program_files_x86)
                .join("WinGet")
                .join("Packages"),
        );
    } else {
        roots.push(PathBuf::from(r"C:\Program Files (x86)\WinGet\Packages"));
    }

    roots
}

fn normalize_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();

    normalized
        .strip_prefix("\\\\?\\unc\\")
        .map(|rest| format!("\\\\{rest}"))
        .or_else(|| normalized.strip_prefix("\\\\?\\").map(str::to_owned))
        .unwrap_or(normalized)
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
    fn the_winget_package_id_belongs_to_the_release_owner() {
        let (owner, _) = github_repo().expect("repository should be configured");
        assert!(
            WINGET_PACKAGE_ID.starts_with(&format!("{owner}.")),
            "winget id {WINGET_PACKAGE_ID} does not belong to {owner}"
        );
    }

    #[test]
    fn a_download_url_outside_this_repository_is_refused() {
        assert!(is_own_release_asset(
            "https://github.com/hadufer/ai-usage-monitor/releases/download/v1.5.4/claude-code-usage-monitor.exe",
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
513c211ac265b5a6770724093d6895dd8012dc68f4f9619af93d2d8ecddcad33 *claude-code-usage-monitor.exe
";
        assert_eq!(
            expected_digest(listing, "claude-code-usage-monitor.exe").as_deref(),
            Some("513c211ac265b5a6770724093d6895dd8012dc68f4f9619af93d2d8ecddcad33")
        );
        assert!(expected_digest(listing, "not-listed.exe").is_none());
        assert!(expected_digest("deadbeef  claude-code-usage-monitor.exe", "claude-code-usage-monitor.exe").is_none());
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
    fn a_prerelease_suffix_would_never_read_as_newer() {
        assert!(!is_version_newer("1.5.0-fork.1", "1.5.0"));
        assert!(is_version_newer("1.5.1", "1.5.0"));
    }
}
