use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use std::os::windows::process::CommandExt;

use crate::diagnose;
use crate::localization::Strings;
use crate::models::{AppUsageData, ScopedUsage, UsageData, UsageSection};
use crate::providers::ProviderId;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTIGRAVITY_CREDENTIAL_TARGET: &str = "gemini:antigravity";
const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MODEL_FALLBACK_CHAIN: &[&str] = &["claude-3-haiku-20240307", "claude-haiku-4-5-20251001"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    AuthRequired,
    NoCredentials,
    TokenExpired,
    RequestFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialWatchMode {
    ActiveSource,
    AllSources,
    Antigravity,
}

pub type CredentialWatchSnapshot = Vec<String>;

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageBucket>,
    seven_day: Option<UsageBucket>,
    #[serde(default)]
    limits: Vec<UsageLimit>,
}

#[derive(Deserialize)]
struct UsageLimit {
    kind: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
    scope: Option<UsageLimitScope>,
}

#[derive(Deserialize)]
struct UsageLimitScope {
    model: Option<UsageLimitModel>,
}

#[derive(Deserialize)]
struct UsageLimitModel {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct UsageBucket {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexTokenData>,
}

#[derive(Clone, Deserialize)]
struct CodexTokenData {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct CodexUsageResponse {
    rate_limit: Option<Option<Box<CodexRateLimitDetails>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitDetails {
    primary_window: Option<Option<Box<CodexRateLimitWindow>>>,
    secondary_window: Option<Option<Box<CodexRateLimitWindow>>>,
}

#[derive(Deserialize)]
struct CodexRateLimitWindow {
    /// 0-100. Optional: a window the server reports without a usable figure is
    /// not worth failing the whole response over.
    #[serde(default)]
    used_percent: Option<f64>,
    /// Unix seconds. Zero means "no reset time", not "reset at the epoch".
    #[serde(default)]
    reset_at: Option<i64>,
    /// Length of the window in seconds. This, not the key it arrived under, is
    /// what tells a 5-hour window from a weekly one.
    #[serde(default)]
    limit_window_seconds: Option<i64>,
}

#[derive(Deserialize)]
struct AntigravityAuthFile {
    token: AntigravityTokenData,
}

#[derive(Deserialize)]
struct AntigravityTokenData {
    access_token: String,
}

#[derive(Deserialize)]
struct AntigravityLoadResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityModelsResponse {
    models: HashMap<String, AntigravityModelInfo>,
}

#[derive(Deserialize)]
struct AntigravityModelInfo {
    #[serde(rename = "quotaInfo")]
    quota_info: Option<AntigravityQuotaInfo>,
}

#[derive(Deserialize)]
struct AntigravityQuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryResponse {
    groups: Option<Vec<AntigravityQuotaSummaryGroup>>,
}

#[derive(Deserialize)]
struct AntigravityQuotaSummaryGroup {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    description: Option<String>,
    buckets: Option<Vec<AntigravityQuotaSummaryBucket>>,
}

#[derive(Clone, Deserialize)]
struct AntigravityQuotaSummaryBucket {
    #[serde(rename = "bucketId")]
    bucket_id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    window: Option<String>,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[repr(C)]
struct CredentialW {
    flags: u32,
    type_: u32,
    target_name: *mut u16,
    comment: *mut u16,
    last_written: u64,
    credential_blob_size: u32,
    credential_blob: *mut u8,
    persist: u32,
    attribute_count: u32,
    attributes: *mut c_void,
    target_alias: *mut u16,
    user_name: *mut u16,
}

#[link(name = "Advapi32")]
extern "system" {
    fn CredReadW(
        target_name: *const u16,
        type_: u32,
        reserved_flags: u32,
        credential: *mut *mut CredentialW,
    ) -> i32;
    fn CredFree(buffer: *mut c_void);
}

pub fn poll(active: &[ProviderId]) -> Result<AppUsageData, PollError> {
    poll_with(active, poll_provider)
}

fn poll_provider(provider: ProviderId) -> Result<UsageData, PollError> {
    match provider {
        ProviderId::ClaudeCode => poll_claude_code(),
        ProviderId::Codex => poll_codex(),
        ProviderId::Antigravity => poll_antigravity(),
    }
}

/// Polls every active provider in turn. One provider failing never blocks the
/// others: the failure is only surfaced when nothing at all came back.
fn poll_with(
    active: &[ProviderId],
    mut poll_one: impl FnMut(ProviderId) -> Result<UsageData, PollError>,
) -> Result<AppUsageData, PollError> {
    let mut data = AppUsageData::default();
    let mut first_error = None;

    for &provider in active {
        match poll_one(provider) {
            Ok(usage) => data.set(provider, usage),
            Err(error) => {
                // With a single provider on screen the widget already shows the
                // failure, so logging it would only be noise.
                if active.len() > 1 {
                    diagnose::log(format!("{provider:?} usage poll failed: {error:?}"));
                }
                first_error.get_or_insert(error);
            }
        }
    }

    if data.is_empty() {
        Err(first_error.unwrap_or(PollError::RequestFailed))
    } else {
        Ok(data)
    }
}

fn poll_claude_code() -> Result<UsageData, PollError> {
    let creds = match read_first_credentials() {
        Some(c) => c,
        None => {
            diagnose::log("poll failed: no Claude credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    let creds = refresh_or_fallback(creds)?;

    fetch_usage_with_fallback(&creds.access_token)
}

fn poll_codex() -> Result<UsageData, PollError> {
    let creds = match read_codex_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Codex usage poll failed: no Codex credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    match fetch_codex_usage(&creds.access_token, creds.account_id.as_deref()) {
        Ok(data) => Ok(data),
        Err(PollError::AuthRequired) => {
            cli_refresh_codex_token();
            let refreshed = read_codex_credentials().ok_or(PollError::TokenExpired)?;
            fetch_codex_usage(&refreshed.access_token, refreshed.account_id.as_deref())
        }
        Err(error) => Err(error),
    }
}

fn poll_antigravity() -> Result<UsageData, PollError> {
    let creds = match read_antigravity_credentials() {
        Some(creds) => creds,
        None => {
            diagnose::log("Antigravity usage poll failed: no Antigravity credentials found");
            return Err(PollError::NoCredentials);
        }
    };

    fetch_antigravity_usage(&creds.access_token)
}

fn refresh_or_fallback(mut creds: Credentials) -> Result<Credentials, PollError> {
    loop {
        if !is_token_expired(creds.expires_at) {
            return Ok(creds);
        }

        let source = creds.source.clone();
        cli_refresh_token(&source);

        match read_credentials_from_source(&source) {
            Some(refreshed) if !is_token_expired(refreshed.expires_at) => return Ok(refreshed),
            Some(_) => diagnose::log(format!(
                "credentials from {source:?} still expired after refresh attempt"
            )),
            None => diagnose::log(format!(
                "credentials from {source:?} unavailable after refresh attempt"
            )),
        }

        match read_next_credentials_after(&source) {
            Some(next) => creds = next,
            None => return Err(PollError::TokenExpired),
        }
    }
}

/// Invoke the Claude CLI with a minimal prompt to force its internal
/// OAuth token refresh.
fn cli_refresh_token(source: &CredentialSource) {
    match source {
        CredentialSource::Windows(_) => cli_refresh_windows_token(),
        CredentialSource::Wsl { distro } => cli_refresh_wsl_token(distro),
    }
}

fn cli_refresh_windows_token() {
    let claude_path = resolve_windows_claude_path();
    let is_cmd = claude_path.to_lowercase().ends_with(".cmd");
    diagnose::log(format!(
        "attempting Windows Claude token refresh via {claude_path}"
    ));

    let args: &[&str] = &["-p", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&claude_path).args(args);
        c
    } else {
        let mut c = Command::new(&claude_path);
        c.args(args);
        c
    };
    cmd.env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Claude token refresh", error);
            return;
        }
    };

    // Wait up to 30 seconds — don't block the poll thread forever
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

fn cli_refresh_wsl_token(distro: &str) {
    diagnose::log(format!(
        "attempting WSL Claude token refresh in distro {distro}"
    ));
    let mut cmd = Command::new("wsl.exe");
    cmd.arg("-d")
        .arg(distro)
        .arg("--")
        .arg("bash")
        .arg("-lic")
        .arg("if command -v claude >/dev/null 2>&1; then claude -p .; elif [ -x \"$HOME/.local/bin/claude\" ]; then \"$HOME/.local/bin/claude\" -p .; else exit 127; fi")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn WSL Claude token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

fn cli_refresh_codex_token() {
    let codex_path = resolve_windows_codex_path();
    let is_cmd = codex_path.to_lowercase().ends_with(".cmd");
    let is_ps1 = codex_path.to_lowercase().ends_with(".ps1");
    diagnose::log(format!(
        "attempting Windows Codex token refresh via {codex_path}"
    ));

    let args: &[&str] = &["exec", "."];

    let mut cmd = if is_cmd {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&codex_path).args(args);
        c
    } else if is_ps1 {
        let mut c = Command::new("powershell.exe");
        c.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&codex_path)
            .args(args);
        c
    } else {
        let mut c = Command::new(&codex_path);
        c.args(args);
        c
    };
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(error) => {
            diagnose::log_error("unable to spawn Windows Codex token refresh", error);
            return;
        }
    };

    wait_for_refresh(&mut child);
}

/// Spawn a command and wait up to `timeout` for it to finish.
/// Returns None if the process fails to start or exceeds the deadline.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = cmd.spawn().ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

fn wait_for_refresh(child: &mut std::process::Child) {
    // Wait up to 30 seconds; don't block the poll thread forever.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(_) => break,
        }
    }
}

/// Resolve the full path to the `claude` CLI executable.
fn resolve_windows_claude_path() -> String {
    for name in &["claude.cmd", "claude"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["claude.cmd", "claude"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "claude.cmd".to_string()
}

fn resolve_windows_codex_path() -> String {
    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if Command::new(name)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return name.to_string();
        }
    }

    for name in &["codex.cmd", "codex.ps1", "codex.exe", "codex"] {
        if let Ok(output) = Command::new("where.exe")
            .arg(name)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(first_line) = stdout.lines().next() {
                    let path = first_line.trim().to_string();
                    if !path.is_empty() {
                        return path;
                    }
                }
            }
        }
    }

    "codex.cmd".to_string()
}

fn build_agent() -> Result<ureq::Agent, PollError> {
    let tls = native_tls::TlsConnector::new().map_err(|_| PollError::RequestFailed)?;
    Ok(ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .tls_connector(std::sync::Arc::new(tls))
        .build())
}

pub fn credential_watch_snapshot(mode: CredentialWatchMode) -> CredentialWatchSnapshot {
    if mode == CredentialWatchMode::Antigravity {
        return vec![antigravity_credential_watch_signature()];
    }

    let sources = match mode {
        CredentialWatchMode::ActiveSource => read_first_credentials()
            .map(|creds| vec![creds.source])
            .unwrap_or_else(all_known_credential_sources),
        CredentialWatchMode::AllSources => all_known_credential_sources(),
        CredentialWatchMode::Antigravity => unreachable!(),
    };

    let mut snapshot: CredentialWatchSnapshot = sources
        .into_iter()
        .filter_map(|source| credential_watch_signature(&source))
        .collect();
    snapshot.sort();
    snapshot.dedup();
    snapshot
}

fn all_known_credential_sources() -> Vec<CredentialSource> {
    let mut sources = Vec::new();
    if let Some(source) = windows_credential_source() {
        sources.push(source);
    }
    for distro in list_wsl_distros() {
        sources.push(CredentialSource::Wsl { distro });
    }
    sources
}

fn windows_credential_source() -> Option<CredentialSource> {
    let home = dirs::home_dir()?;
    Some(CredentialSource::Windows(
        home.join(".claude").join(".credentials.json"),
    ))
}

fn credential_watch_signature(source: &CredentialSource) -> Option<String> {
    match source {
        CredentialSource::Windows(path) => Some(windows_credential_watch_signature(path)),
        CredentialSource::Wsl { distro } => wsl_credential_watch_signature(distro),
    }
}

fn windows_credential_watch_signature(path: &PathBuf) -> String {
    let key = format!("win:{}", path.display());
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                .map(|value| value.as_secs())
                .unwrap_or(0);
            format!("{key}|present|{}|{modified}", metadata.len())
        }
        Err(_) => format!("{key}|missing"),
    }
}

fn wsl_credential_watch_signature(distro: &str) -> Option<String> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg(
                "if [ -f ~/.claude/.credentials.json ]; then \
                 stat -c 'present|%s|%Y' ~/.claude/.credentials.json; \
                 else echo missing; fi",
            )
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    let state = if output.status.success() {
        decode_wsl_text(&output.stdout).trim().to_string()
    } else {
        format!("status-{}", output.status)
    };

    Some(format!("wsl:{distro}|{state}"))
}

fn fetch_usage_with_fallback(token: &str) -> Result<UsageData, PollError> {
    // Try the dedicated usage endpoint first
    match try_usage_endpoint(token)? {
        Some(data) => {
            // If reset timers are missing, fill them in from the Messages API
            if data.session.resets_at.is_none() || data.weekly.resets_at.is_none() {
                if let Ok(fallback) = fetch_usage_via_messages(token) {
                    let mut merged = data;
                    if merged.session.resets_at.is_none() {
                        merged.session.resets_at = fallback.session.resets_at;
                    }
                    if merged.weekly.resets_at.is_none() {
                        merged.weekly.resets_at = fallback.weekly.resets_at;
                    }
                    return Ok(merged);
                }
            }
            return Ok(data);
        }
        None => {}
    }

    // Fall back to Messages API with rate limit headers
    let result = fetch_usage_via_messages(token);
    if result.is_err() {
        diagnose::log("usage endpoint and Messages API fallback both failed");
    }
    result
}

fn try_usage_endpoint(token: &str) -> Result<Option<UsageData>, PollError> {
    let agent = build_agent()?;

    let resp = match agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .call()
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "usage endpoint returned auth error status {code}; re-login required"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(_) => return Ok(None),
    };

    let body = match resp.into_string() {
        Ok(body) => body,
        Err(_) => return Ok(None),
    };
    if diagnose::is_enabled() {
        diagnose::log(format!("usage endpoint raw body: {body}"));
    }

    let response: UsageResponse = match serde_json::from_str(&body) {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    let mut data = UsageData::default();

    if let Some(bucket) = &response.five_hour {
        data.session.percentage = bucket.utilization;
        data.session.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    if let Some(bucket) = &response.seven_day {
        data.weekly.percentage = bucket.utilization;
        data.weekly.resets_at = parse_iso8601(bucket.resets_at.as_deref());
    }

    data.scoped = scoped_weekly(&response.limits);

    Ok(Some(data))
}

/// The per-model weekly limit, which the API only reports inside `limits`
/// rather than as its own top-level bucket.
fn scoped_weekly(limits: &[UsageLimit]) -> Option<ScopedUsage> {
    // Every scoped entry is considered, not just the first: a scope can carry a
    // surface instead of a model, or a null percent, and letting such an entry
    // short-circuit would hide a perfectly good model limit sitting behind it.
    // When several models are reported, the most consumed one is the useful one.
    limits
        .iter()
        .filter(|limit| limit.kind.as_deref() == Some("weekly_scoped"))
        .filter_map(usable_scoped_limit)
        .max_by(|left, right| {
            left.section
                .percentage
                .partial_cmp(&right.section.percentage)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn usable_scoped_limit(limit: &UsageLimit) -> Option<ScopedUsage> {
    let label = limit
        .scope
        .as_ref()?
        .model
        .as_ref()?
        .display_name
        .as_ref()
        .filter(|label| !label.is_empty())?
        .clone();

    Some(ScopedUsage {
        label,
        section: UsageSection {
            percentage: limit.percent?,
            resets_at: parse_iso8601(limit.resets_at.as_deref()),
        },
    })
}

fn fetch_usage_via_messages(token: &str) -> Result<UsageData, PollError> {
    let agent = build_agent()?;

    for model in MODEL_FALLBACK_CHAIN {
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "."}]
        });

        let response = match agent
            .post(MESSAGES_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-version", "2023-06-01")
            .set("anthropic-beta", "oauth-2025-04-20")
            .send_json(&body)
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
                diagnose::log(format!(
                    "messages endpoint returned auth error status {code}; re-login required"
                ));
                return Err(PollError::AuthRequired);
            }
            Err(ureq::Error::Status(_code, resp)) => resp,
            Err(_) => continue,
        };

        let h5 = response.header("anthropic-ratelimit-unified-5h-utilization");
        let h7 = response.header("anthropic-ratelimit-unified-7d-utilization");
        let hs = response.header("anthropic-ratelimit-unified-status");

        if h5.is_some() || h7.is_some() || hs.is_some() {
            return Ok(parse_rate_limit_headers(&response));
        }
    }

    Err(PollError::RequestFailed)
}

fn parse_rate_limit_headers(response: &ureq::Response) -> UsageData {
    let mut data = UsageData::default();

    data.session.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-5h-utilization") * 100.0;
    data.session.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-5h-reset",
    ));

    data.weekly.percentage =
        get_header_f64(response, "anthropic-ratelimit-unified-7d-utilization") * 100.0;
    data.weekly.resets_at = unix_to_system_time(get_header_i64(
        response,
        "anthropic-ratelimit-unified-7d-reset",
    ));

    let overall_reset = get_header_i64(response, "anthropic-ratelimit-unified-reset");

    if data.session.percentage == 0.0 && data.weekly.percentage == 0.0 {
        let status = response.header("anthropic-ratelimit-unified-status");
        if status == Some("rejected") {
            let claim = response.header("anthropic-ratelimit-unified-representative-claim");
            match claim {
                Some("five_hour") => data.session.percentage = 100.0,
                Some("seven_day") => data.weekly.percentage = 100.0,
                _ => {}
            }
        }

        if data.session.resets_at.is_none() && overall_reset.is_some() {
            data.session.resets_at = unix_to_system_time(overall_reset);
        }
    }

    data
}

fn fetch_codex_usage(token: &str, account_id: Option<&str>) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let mut request = agent
        .get(CODEX_USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "codex-cli");

    if let Some(account_id) = account_id.filter(|value| !value.is_empty()) {
        request = request.set("ChatGPT-Account-Id", account_id);
    }

    let resp = match request.call() {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "Codex usage endpoint returned auth error status {code}; refresh required"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error) => {
            diagnose::log_error("Codex usage endpoint request failed", error);
            return Err(PollError::RequestFailed);
        }
    };

    let body = match resp.into_string() {
        Ok(body) => body,
        Err(error) => {
            diagnose::log_error("unable to read Codex usage response", error);
            return Err(PollError::RequestFailed);
        }
    };
    if diagnose::is_enabled() {
        // Same reason the Claude path logs its own body: this is how the shape
        // of the response gets checked against a real account instead of
        // against a guess. It holds usage percentages, reset timestamps and,
        // on a workspace plan, the account id — never a token.
        diagnose::log(format!("Codex usage endpoint raw body: {body}"));
    }

    let response: CodexUsageResponse = match serde_json::from_str(&body) {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Codex usage response", error);
            return Err(PollError::RequestFailed);
        }
    };

    match codex_usage_from_response(response) {
        Some(usage) => Ok(usage),
        None => {
            // Distinguishable in the log from a transport failure, which is
            // the whole point of not inventing a 0% for this case.
            diagnose::log("Codex usage response held no usable rate-limit window");
            Err(PollError::RequestFailed)
        }
    }
}

/// Windows are told apart by their declared length, never by which key they
/// arrived under: `primary_window` and `secondary_window` are positional names,
/// and the server has been observed putting a weekly window in `primary_window`
/// with `secondary_window` empty.
fn codex_usage_from_response(response: CodexUsageResponse) -> Option<UsageData> {
    let Some(details) = response.rate_limit.flatten() else {
        // No rate-limit block at all: nothing to show. Treated as a failed
        // read rather than as "zero used".
        return None;
    };

    let mut data = UsageData::default();
    let mut session_set = false;
    let mut weekly_set = false;

    let windows = [
        (details.primary_window.flatten(), CodexWindowFallback::Session),
        (details.secondary_window.flatten(), CodexWindowFallback::Weekly),
    ];

    for (window, fallback) in windows {
        let Some(window) = window else { continue };

        let is_session = codex_window_kind(&window, fallback) == CodexWindowKind::Session;
        if (is_session && session_set) || (!is_session && weekly_set) {
            continue;
        }

        // A window with no usable percentage carries no information: leave the
        // row alone instead of publishing it as a zero.
        let Some(section) = codex_section_from_window(&window) else {
            continue;
        };

        if is_session {
            data.session = section;
            session_set = true;
        } else {
            data.weekly = section;
            weekly_set = true;
        }
    }

    if session_set || weekly_set {
        Some(data)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexWindowKind {
    Session,
    Weekly,
}

/// Which row a window goes to when its length is missing or unfamiliar. The
/// positional key is the only hint left in that case, so it is used here and
/// only here.
#[derive(Clone, Copy)]
enum CodexWindowFallback {
    Session,
    Weekly,
}

/// A day in minutes. The two nominal lengths the server reports are 300
/// (five hours) and 10 080 (a week), so a day sits between them with room to
/// spare: anything shorter is the short window, anything at or above it is a
/// weekly-style allowance. Windows reported a few percent off their nominal
/// length therefore land in the right row without a tolerance of their own.
const DAY_MINUTES: f64 = 1_440.0;

fn codex_window_kind(
    window: &CodexRateLimitWindow,
    fallback: CodexWindowFallback,
) -> CodexWindowKind {
    let fallback_kind = match fallback {
        CodexWindowFallback::Session => CodexWindowKind::Session,
        CodexWindowFallback::Weekly => CodexWindowKind::Weekly,
    };

    let Some(seconds) = window.limit_window_seconds.filter(|seconds| *seconds > 0) else {
        return fallback_kind;
    };
    let minutes = (seconds as f64 + 59.0) / 60.0;

    if minutes >= DAY_MINUTES {
        CodexWindowKind::Weekly
    } else {
        CodexWindowKind::Session
    }
}

fn codex_section_from_window(window: &CodexRateLimitWindow) -> Option<UsageSection> {
    let percentage = window.used_percent?;
    // A zero timestamp means "no known reset", not "reset at the epoch".
    let resets_at = window
        .reset_at
        .filter(|secs| *secs > 0)
        .and_then(|secs| unix_to_system_time(Some(secs)));

    Some(UsageSection {
        percentage,
        resets_at,
    })
}

fn antigravity_credential_watch_signature() -> String {
    let Some(content) = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET) else {
        return format!("{ANTIGRAVITY_CREDENTIAL_TARGET}|missing");
    };

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!(
        "{ANTIGRAVITY_CREDENTIAL_TARGET}|present|{}|{}",
        content.len(),
        hasher.finish()
    )
}

fn fetch_antigravity_usage(token: &str) -> Result<UsageData, PollError> {
    let mut auth_error = false;
    let mut last_error = PollError::RequestFailed;

    for base_url in ANTIGRAVITY_ENDPOINTS {
        match fetch_antigravity_usage_from_endpoint(base_url, token) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => auth_error = true,
            Err(error) => last_error = error,
        }
    }

    if auth_error {
        Err(PollError::AuthRequired)
    } else {
        Err(last_error)
    }
}

fn fetch_antigravity_usage_from_endpoint(
    base_url: &str,
    token: &str,
) -> Result<UsageData, PollError> {
    let project = fetch_antigravity_project(base_url, token)?;
    if let Some(project) = project.as_deref() {
        match fetch_antigravity_quota_summary(base_url, token, project) {
            Ok(data) => return Ok(data),
            Err(PollError::AuthRequired) => return Err(PollError::AuthRequired),
            Err(error) => diagnose::log(format!(
                "Antigravity retrieveUserQuotaSummary failed, falling back to model quota: {error:?}"
            )),
        }
    }

    let session = fetch_antigravity_model_quota(base_url, token, project.as_deref())?;
    let weekly = UsageSection::default();

    Ok(UsageData {
        session,
        weekly,
        scoped: None,
    })
}

fn fetch_antigravity_project(base_url: &str, token: &str) -> Result<Option<String>, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:loadCodeAssist"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "Antigravity loadCodeAssist returned auth error status {code}"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error) => {
            diagnose::log_error("Antigravity loadCodeAssist request failed", error);
            return Err(PollError::RequestFailed);
        }
    };

    let response: AntigravityLoadResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error("unable to parse Antigravity loadCodeAssist response", error);
            return Err(PollError::RequestFailed);
        }
    };

    Ok(response.project.filter(|project| !project.is_empty()))
}

fn fetch_antigravity_model_quota(
    base_url: &str,
    token: &str,
    project: Option<&str>,
) -> Result<UsageSection, PollError> {
    let agent = build_agent()?;
    let body = match project {
        Some(project) => serde_json::json!({ "project": project }),
        None => serde_json::json!({}),
    };

    let resp = match agent
        .post(&format!("{base_url}/v1internal:fetchAvailableModels"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            diagnose::log(format!(
                "Antigravity fetchAvailableModels returned auth error status {code}"
            ));
            return Err(PollError::AuthRequired);
        }
        Err(error) => {
            diagnose::log_error("Antigravity fetchAvailableModels request failed", error);
            return Err(PollError::RequestFailed);
        }
    };

    let response: AntigravityModelsResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity fetchAvailableModels response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    best_antigravity_section(response.models.into_iter().filter_map(|(model, info)| {
        let quota = info.quota_info?;
        if !is_antigravity_display_model(&model) {
            return None;
        }
        antigravity_section_from_quota(quota)
    }))
    .ok_or(PollError::RequestFailed)
}

fn fetch_antigravity_quota_summary(
    base_url: &str,
    token: &str,
    project: &str,
) -> Result<UsageData, PollError> {
    let agent = build_agent()?;
    let body = serde_json::json!({ "project": project });

    let resp = match agent
        .post(&format!("{base_url}/v1internal:retrieveUserQuotaSummary"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .set("User-Agent", "antigravity")
        .send_json(&body)
    {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            return Err(PollError::AuthRequired);
        }
        Err(error) => {
            diagnose::log_error("Antigravity retrieveUserQuotaSummary request failed", error);
            return Err(PollError::RequestFailed);
        }
    };

    let response: AntigravityQuotaSummaryResponse = match resp.into_json() {
        Ok(response) => response,
        Err(error) => {
            diagnose::log_error(
                "unable to parse Antigravity retrieveUserQuotaSummary response",
                error,
            );
            return Err(PollError::RequestFailed);
        }
    };

    antigravity_usage_from_summary(response).ok_or(PollError::RequestFailed)
}

fn antigravity_section_from_quota(quota: AntigravityQuotaInfo) -> Option<UsageSection> {
    let remaining = quota.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(quota.reset_time.as_deref()),
    })
}

fn antigravity_section_from_summary_bucket(
    bucket: &AntigravityQuotaSummaryBucket,
) -> Option<UsageSection> {
    let remaining = bucket.remaining_fraction?.clamp(0.0, 1.0);
    Some(UsageSection {
        percentage: (1.0 - remaining) * 100.0,
        resets_at: parse_iso8601(bucket.reset_time.as_deref()),
    })
}

fn antigravity_usage_from_summary(response: AntigravityQuotaSummaryResponse) -> Option<UsageData> {
    let mut fallback = None;

    for group in response.groups.unwrap_or_default() {
        let is_gemini = is_antigravity_gemini_summary_group(&group);
        let usage = antigravity_usage_from_summary_group(group);

        if is_gemini && usage.is_some() {
            return usage;
        }

        if fallback.is_none() {
            fallback = usage;
        }
    }

    fallback
}

fn antigravity_usage_from_summary_group(group: AntigravityQuotaSummaryGroup) -> Option<UsageData> {
    let mut data = UsageData::default();
    let mut has_quota = false;

    for bucket in group.buckets.unwrap_or_default() {
        let Some(section) = antigravity_section_from_summary_bucket(&bucket) else {
            continue;
        };

        match bucket.window.as_deref() {
            Some(window) if window.eq_ignore_ascii_case("5h") => {
                data.session = section;
                has_quota = true;
            }
            Some(window) if window.eq_ignore_ascii_case("weekly") => {
                data.weekly = section;
                has_quota = true;
            }
            _ => {}
        }
    }

    has_quota.then_some(data)
}

fn is_antigravity_gemini_summary_group(group: &AntigravityQuotaSummaryGroup) -> bool {
    group
        .display_name
        .as_deref()
        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
        || group
            .description
            .as_deref()
            .is_some_and(|description| description.to_ascii_lowercase().contains("gemini"))
        || group.buckets.as_ref().is_some_and(|buckets| {
            buckets.iter().any(|bucket| {
                bucket
                    .bucket_id
                    .as_deref()
                    .is_some_and(|id| id.to_ascii_lowercase().starts_with("gemini-"))
                    || bucket
                        .display_name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("gemini"))
            })
        })
}

fn best_antigravity_section<I>(sections: I) -> Option<UsageSection>
where
    I: IntoIterator<Item = UsageSection>,
{
    sections.into_iter().max_by(|a, b| {
        a.percentage
            .partial_cmp(&b.percentage)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.resets_at.cmp(&b.resets_at))
    })
}

fn is_antigravity_display_model(model: &str) -> bool {
    model.starts_with("gemini")
        || model.starts_with("claude")
        || model.starts_with("gpt")
        || model.starts_with("image")
        || model.starts_with("imagen")
}

fn get_header_f64(response: &ureq::Response, name: &str) -> f64 {
    response
        .header(name)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn get_header_i64(response: &ureq::Response, name: &str) -> Option<i64> {
    response.header(name).and_then(|s| s.parse::<i64>().ok())
}

fn unix_to_system_time(unix_secs: Option<i64>) -> Option<SystemTime> {
    let secs = unix_secs?;
    if secs < 0 {
        return None;
    }
    // An out-of-range timestamp is treated as "no reset time" rather than
    // taking the process down: `panic = "abort"` is set, so adding blindly here
    // would end the widget instead of dropping one row.
    UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
}

struct Credentials {
    access_token: String,
    expires_at: Option<i64>,
    source: CredentialSource,
}

#[derive(Clone, Debug)]
enum CredentialSource {
    Windows(PathBuf),
    Wsl { distro: String },
}

fn read_first_credentials() -> Option<Credentials> {
    if let Some(creds) = read_windows_credentials() {
        return Some(creds);
    }

    for distro in list_wsl_distros() {
        if let Some(creds) = read_wsl_credentials(&distro) {
            return Some(creds);
        }
    }

    None
}

fn read_windows_credentials() -> Option<Credentials> {
    let CredentialSource::Windows(cred_path) = windows_credential_source()? else {
        return None;
    };
    let content = match std::fs::read_to_string(&cred_path) {
        Ok(content) => content,
        Err(error) => {
            if diagnose::is_enabled() {
                diagnose::log_error(
                    &format!(
                        "unable to read Windows credentials at {}",
                        cred_path.display()
                    ),
                    error,
                );
            }
            return None;
        }
    };
    parse_credentials(&content, CredentialSource::Windows(cred_path))
}

fn read_credentials_from_source(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(path) => {
            let content = std::fs::read_to_string(path).ok()?;
            parse_credentials(&content, source.clone())
        }
        CredentialSource::Wsl { distro } => read_wsl_credentials(distro),
    }
}

fn codex_auth_path() -> Option<PathBuf> {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        return Some(codex_home.join("auth.json"));
    }

    Some(dirs::home_dir()?.join(".codex").join("auth.json"))
}

fn read_codex_credentials() -> Option<CodexTokenData> {
    let auth_path = codex_auth_path()?;
    let content = match std::fs::read_to_string(&auth_path) {
        Ok(content) => content,
        Err(error) => {
            diagnose::log_error(
                &format!(
                    "unable to read Codex credentials at {}",
                    auth_path.display()
                ),
                error,
            );
            return None;
        }
    };

    let auth: CodexAuthFile = serde_json::from_str(&content).ok()?;
    auth.tokens.filter(|tokens| !tokens.access_token.is_empty())
}

fn read_antigravity_credentials() -> Option<AntigravityTokenData> {
    let content = read_windows_generic_credential(ANTIGRAVITY_CREDENTIAL_TARGET)?;
    let auth: AntigravityAuthFile = serde_json::from_str(&content).ok()?;
    if auth.token.access_token.is_empty() {
        None
    } else {
        Some(auth.token)
    }
}

fn read_windows_generic_credential(target: &str) -> Option<String> {
    const CRED_TYPE_GENERIC: u32 = 1;

    let mut target_wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut credential: *mut CredentialW = std::ptr::null_mut();

    let ok = unsafe {
        CredReadW(
            target_wide.as_mut_ptr(),
            CRED_TYPE_GENERIC,
            0,
            &mut credential,
        )
    };

    if ok == 0 || credential.is_null() {
        diagnose::log(format!(
            "unable to read Windows generic credential target {target}"
        ));
        return None;
    }

    let result = unsafe {
        let cred = &*credential;
        if cred.credential_blob_size == 0 || cred.credential_blob.is_null() {
            CredFree(credential as *mut c_void);
            return None;
        }
        let bytes =
            std::slice::from_raw_parts(cred.credential_blob, cred.credential_blob_size as usize);
        let text = String::from_utf8(bytes.to_vec()).ok();
        CredFree(credential as *mut c_void);
        text
    };

    result
}

fn read_wsl_credentials(distro: &str) -> Option<Credentials> {
    let output = run_with_timeout(
        Command::new("wsl.exe")
            .arg("-d")
            .arg(distro)
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg("cat ~/.claude/.credentials.json")
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    )?;

    if !output.status.success() {
        diagnose::log(format!(
            "WSL credentials probe failed for distro {distro} with status {}",
            output.status
        ));
        return None;
    }

    let content = String::from_utf8(output.stdout).ok()?;
    parse_credentials(
        &content,
        CredentialSource::Wsl {
            distro: distro.to_string(),
        },
    )
}

fn parse_credentials(content: &str, source: CredentialSource) -> Option<Credentials> {
    let json: serde_json::Value = serde_json::from_str(content).ok()?;

    let oauth = json.get("claudeAiOauth")?;
    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())?
        .to_string();
    let expires_at = oauth.get("expiresAt").and_then(|v| v.as_i64());

    Some(Credentials {
        access_token,
        expires_at,
        source,
    })
}

fn read_next_credentials_after(source: &CredentialSource) -> Option<Credentials> {
    match source {
        CredentialSource::Windows(_) => {
            for distro in list_wsl_distros() {
                if let Some(creds) = read_wsl_credentials(&distro) {
                    return Some(creds);
                }
            }
        }
        CredentialSource::Wsl { distro } => {
            let mut past_current = false;
            for candidate_distro in list_wsl_distros() {
                if !past_current {
                    past_current = candidate_distro == *distro;
                    continue;
                }
                if let Some(creds) = read_wsl_credentials(&candidate_distro) {
                    return Some(creds);
                }
            }
        }
    }

    None
}

fn list_wsl_distros() -> Vec<String> {
    let output = match run_with_timeout(
        Command::new("wsl.exe")
            .args(["-l", "-q"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
        Duration::from_secs(5),
    ) {
        Some(output) if output.status.success() => output,
        _ => {
            diagnose::log("unable to enumerate WSL distros");
            return Vec::new();
        }
    };

    let stdout = decode_wsl_text(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn decode_wsl_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    if let Some(decoded) = decode_utf16le(bytes) {
        return decoded;
    }

    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes.len() % 2 != 0 {
        return None;
    }

    let body = if bytes.starts_with(&[0xFF, 0xFE]) {
        &bytes[2..]
    } else if looks_like_utf16le(bytes) {
        bytes
    } else {
        return None;
    };

    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    Some(String::from_utf16_lossy(&units))
}

fn looks_like_utf16le(bytes: &[u8]) -> bool {
    let sample_len = bytes.len().min(128);
    let units = sample_len / 2;
    if units == 0 {
        return false;
    }

    let nul_high_bytes = bytes[..sample_len]
        .chunks_exact(2)
        .filter(|chunk| chunk[1] == 0)
        .count();

    nul_high_bytes * 2 >= units
}

fn is_token_expired(expires_at: Option<i64>) -> bool {
    let Some(exp) = expires_at else { return false };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now >= exp
}

/// Parse an ISO 8601 timestamp string into a SystemTime.
fn parse_iso8601(s: Option<&str>) -> Option<SystemTime> {
    let s = s?;
    // Strip timezone offset to get "YYYY-MM-DDTHH:MM:SS" or with fractional seconds
    // The API returns formats like "2026-03-05T08:00:00.321598+00:00"
    let datetime_part = s.split('+').next().unwrap_or(s);
    let datetime_part = datetime_part.split('Z').next().unwrap_or(datetime_part);

    // Try parsing with and without fractional seconds
    let formats = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    for fmt in &formats {
        if let Ok(secs) = parse_datetime_to_unix(datetime_part, fmt) {
            return Some(UNIX_EPOCH + Duration::from_secs(secs));
        }
    }
    None
}

/// Minimal datetime parser — avoids pulling in chrono/time crates.
fn parse_datetime_to_unix(s: &str, _fmt: &str) -> Result<u64, ()> {
    // Extract date and time parts from "YYYY-MM-DDTHH:MM:SS[.frac]"
    let (date_str, time_str) = s.split_once('T').ok_or(())?;
    let date_parts: Vec<&str> = date_str.split('-').collect();
    if date_parts.len() != 3 {
        return Err(());
    }

    let year: u64 = date_parts[0].parse().map_err(|_| ())?;
    let month: u64 = date_parts[1].parse().map_err(|_| ())?;
    let day: u64 = date_parts[2].parse().map_err(|_| ())?;

    // Strip fractional seconds
    let time_base = time_str.split('.').next().unwrap_or(time_str);
    let time_parts: Vec<&str> = time_base.split(':').collect();
    if time_parts.len() != 3 {
        return Err(());
    }

    let hour: u64 = time_parts[0].parse().map_err(|_| ())?;
    let min: u64 = time_parts[1].parse().map_err(|_| ())?;
    let sec: u64 = time_parts[2].parse().map_err(|_| ())?;

    // Days from year (using a simplified calculation for dates after 1970)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }

    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += day - 1;

    Ok(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Format a usage section as "X% · Yh" style text, or "X% · Yh Zm" when the
/// countdown is set to be detailed.
pub fn format_line(section: &UsageSection, strings: Strings, detailed: bool) -> String {
    let pct = format!("{:.0}%", section.percentage);
    let cd = format_countdown(section.resets_at, strings, detailed);
    if cd.is_empty() {
        pct
    } else {
        format!("{pct} \u{00b7} {cd}")
    }
}

/// The countdown alone, without the percentage: the widget's featured window
/// writes the two in different inks.
pub fn format_countdown(
    resets_at: Option<SystemTime>,
    strings: Strings,
    detailed: bool,
) -> String {
    let reset = match resets_at {
        Some(t) => t,
        None => return String::new(),
    };

    let remaining = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d,
        Err(_) => return strings.now.to_string(),
    };

    format_countdown_from_secs(remaining.as_secs(), strings, detailed)
}

/// Calculate how long until the display text would change
pub fn time_until_display_change(
    resets_at: Option<SystemTime>,
    detailed: bool,
) -> Option<Duration> {
    let reset = resets_at?;
    let remaining = reset.duration_since(SystemTime::now()).ok()?;
    Some(time_until_display_change_from_secs(
        remaining.as_secs(),
        detailed,
    ))
}

const MIN_SECS: u64 = 60;
const HOUR_SECS: u64 = 60 * MIN_SECS;
const DAY_SECS: u64 = 24 * HOUR_SECS;

fn round_div(n: u64, unit: u64) -> u64 {
    (n + unit / 2) / unit
}

fn format_countdown_from_secs(total_secs: u64, strings: Strings, detailed: bool) -> String {
    if detailed {
        return format_detailed_countdown(total_secs, strings);
    }

    // The unit is picked by magnitude, then rounded to the nearest whole unit, so
    // 3h59m reads "4h" rather than understating it as "3h". Rounding can fill the
    // unit, which promotes to the next one instead of printing "24h" or "60m".
    let (value, suffix) = if total_secs >= DAY_SECS {
        (round_div(total_secs, DAY_SECS), strings.day_suffix)
    } else if total_secs >= HOUR_SECS {
        match round_div(total_secs, HOUR_SECS) {
            24 => (1, strings.day_suffix),
            hours => (hours, strings.hour_suffix),
        }
    } else if total_secs >= MIN_SECS {
        match round_div(total_secs, MIN_SECS) {
            60 => (1, strings.hour_suffix),
            mins => (mins, strings.minute_suffix),
        }
    } else {
        (total_secs, strings.second_suffix)
    };

    format!("{value}{suffix}")
}

/// Hours *and* minutes ("3h59m") instead of one rounded unit ("4h"), for anyone who
/// would rather read the exact figure than a tidy one. The pair is truncated rather
/// than rounded: the finer unit already carries the precision the rounding hid.
fn format_detailed_countdown(total_secs: u64, strings: Strings) -> String {
    let (unit, finer, unit_suffix, finer_suffix) = if total_secs >= DAY_SECS {
        (DAY_SECS, HOUR_SECS, strings.day_suffix, strings.hour_suffix)
    } else if total_secs >= HOUR_SECS {
        (
            HOUR_SECS,
            MIN_SECS,
            strings.hour_suffix,
            strings.minute_suffix,
        )
    } else if total_secs >= MIN_SECS {
        // No coarser unit left to pair with, and these minutes are exact instead of
        // rounded, so one unit already says everything the detailed form would.
        return format!("{}{}", total_secs / MIN_SECS, strings.minute_suffix);
    } else {
        return format!("{total_secs}{}", strings.second_suffix);
    };

    let value = total_secs / unit;
    let finer_value = (total_secs % unit) / finer;
    // Padded so the column does not jitter between "3h05m" and "3h5m".
    format!("{value}{unit_suffix}{finer_value:02}{finer_suffix}")
}

fn time_until_display_change_from_secs(total_secs: u64, detailed: bool) -> Duration {
    if detailed {
        // The truncated finer unit of the pair is what ticks, so the text changes
        // when it does: hours under a day, minutes under an hour, else seconds.
        let step = if total_secs >= DAY_SECS {
            HOUR_SECS
        } else if total_secs >= MIN_SECS {
            MIN_SECS
        } else {
            1
        };
        return Duration::from_secs(total_secs % step + 1);
    }

    let (unit, finer) = if total_secs >= DAY_SECS {
        (DAY_SECS, HOUR_SECS)
    } else if total_secs >= HOUR_SECS {
        (HOUR_SECS, MIN_SECS)
    } else if total_secs >= MIN_SECS {
        (MIN_SECS, 1)
    } else {
        return Duration::from_secs(1);
    };

    // Rounding half up shows the same value down to `value * unit - unit / 2`. At value 1
    // the string also survives below the unit, because the finer bucket rounds back up
    // into it ("1h" covers 59m30s), so that edge sits half a finer unit lower.
    let value = round_div(total_secs, unit);
    let bucket_start = if value == 1 {
        unit - finer / 2
    } else {
        value * unit - unit / 2
    };

    Duration::from_secs(total_secs.saturating_sub(bucket_start) + 1)
}

/// Returns true if either section has reached "now" (reset time has passed).
pub fn is_past_reset(data: &UsageData) -> bool {
    let now = SystemTime::now();
    let past = |s: &UsageSection| matches!(s.resets_at, Some(t) if now.duration_since(t).is_ok());
    past(&data.session) || past(&data.weekly)
}

pub fn app_is_past_reset(data: &AppUsageData) -> bool {
    data.iter().any(|entry| is_past_reset(&entry.data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english() -> Strings {
        crate::localization::LanguageId::English.strings()
    }

    const COUNTDOWN_PROBES: [u64; 12] = [
        59, 60, 3599, 3600, 12599, 14340, 14380, 84599, 86399, 86400, 91800, 600_000,
    ];

    /// The bar has room for one unit, so it rounds to the nearest one instead of
    /// flooring, and the redraw timer has to wake up exactly when that string changes.
    #[test]
    fn rounds_the_countdown_to_the_nearest_unit() {
        let s = english();
        let fmt = |secs| format_countdown_from_secs(secs, s, false);

        assert_eq!(
            fmt(14340),
            "4h",
            "3h59m rounds up, it no longer floors to 3h"
        );
        assert_eq!(fmt(12599), "3h", "3h29m59s rounds down");
        assert_eq!(
            fmt(86399),
            "1d",
            "23h59m59s promotes instead of showing 24h"
        );
        assert_eq!(fmt(3599), "1h", "59m59s promotes instead of showing 60m");
        assert_eq!(fmt(59), "59s", "seconds stay exact");
    }

    /// The detailed setting trades the tidy figure for the exact one.
    #[test]
    fn the_detailed_countdown_shows_two_units_without_rounding() {
        let s = english();
        let fmt = |secs| format_countdown_from_secs(secs, s, true);

        assert_eq!(
            fmt(14340),
            "3h59m",
            "the minutes are shown, not rounded away"
        );
        assert_eq!(fmt(14380), "3h59m", "the seconds below them are dropped");
        assert_eq!(fmt(11100), "3h05m", "padded so the column cannot jitter");
        assert_eq!(fmt(600_000), "6d22h", "days pair with hours");
        assert_eq!(fmt(3599), "59m", "under an hour the minutes are exact");
        assert_eq!(fmt(59), "59s", "seconds stay exact");
    }

    /// Both modes have to keep the redraw timer honest: the widget must wake up when
    /// its text changes, and no earlier, or it burns repaints for nothing.
    #[test]
    fn the_countdown_wakes_up_exactly_when_its_text_changes() {
        let s = english();

        for detailed in [false, true] {
            let fmt = |secs| format_countdown_from_secs(secs, s, detailed);
            for secs in COUNTDOWN_PROBES {
                let delay = time_until_display_change_from_secs(secs, detailed).as_secs();
                assert!(
                    delay >= 1 && delay <= secs,
                    "{secs}s (detailed {detailed}): bogus delay {delay}s"
                );
                assert_eq!(
                    fmt(secs - delay + 1),
                    fmt(secs),
                    "{secs}s (detailed {detailed}): text changed before the {delay}s wake-up"
                );
                assert_ne!(
                    fmt(secs - delay),
                    fmt(secs),
                    "{secs}s (detailed {detailed}): text had not changed yet at the {delay}s wake-up"
                );
            }
        }
    }

    fn usage_with_session_percent(percentage: f64) -> UsageData {
        UsageData {
            session: UsageSection {
                percentage,
                resets_at: None,
            },
            weekly: UsageSection::default(),
            scoped: None,
        }
    }

    #[test]
    fn reads_the_scoped_weekly_limit_and_its_model_label() {
        let body = r#"{
            "five_hour": {"utilization": 36.0, "resets_at": "2026-07-27T17:19:59+00:00"},
            "seven_day": {"utilization": 46.0, "resets_at": "2026-07-31T04:59:59+00:00"},
            "seven_day_opus": null,
            "limits": [
                {"kind": "session", "percent": 36, "resets_at": "2026-07-27T17:19:59+00:00", "scope": null},
                {"kind": "weekly_all", "percent": 46, "resets_at": "2026-07-31T04:59:59+00:00", "scope": null},
                {"kind": "weekly_scoped", "percent": 29, "resets_at": "2026-07-31T05:00:00+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ]
        }"#;

        let response: UsageResponse = serde_json::from_str(body).expect("payload should parse");
        let scoped = scoped_weekly(&response.limits).expect("scoped limit should be found");

        assert_eq!(scoped.label, "Fable");
        assert_eq!(scoped.section.percentage, 29.0);
        assert!(scoped.section.resets_at.is_some());
    }

    /// Flagged in review: a surface-scoped entry, or one with a null percent,
    /// used to short-circuit and hide the model limit behind it.
    #[test]
    fn an_incomplete_scoped_entry_does_not_hide_a_usable_one() {
        let body = r#"{
            "limits": [
                {"kind": "weekly_scoped", "percent": 12, "resets_at": null,
                 "scope": {"model": null, "surface": "code"}},
                {"kind": "weekly_scoped", "percent": null, "resets_at": null,
                 "scope": {"model": {"display_name": "Sonnet"}}},
                {"kind": "weekly_scoped", "percent": 70, "resets_at": null,
                 "scope": {"model": {"display_name": "Opus"}}}
            ]
        }"#;

        let response: UsageResponse = serde_json::from_str(body).expect("payload should parse");
        let scoped = scoped_weekly(&response.limits).expect("the usable entry should be found");
        assert_eq!(scoped.label, "Opus");
        assert_eq!(scoped.section.percentage, 70.0);
    }

    #[test]
    fn the_most_consumed_model_wins_when_several_are_reported() {
        let body = r#"{
            "limits": [
                {"kind": "weekly_scoped", "percent": 20, "resets_at": null,
                 "scope": {"model": {"display_name": "Sonnet"}}},
                {"kind": "weekly_scoped", "percent": 85, "resets_at": null,
                 "scope": {"model": {"display_name": "Fable"}}}
            ]
        }"#;

        let response: UsageResponse = serde_json::from_str(body).expect("payload should parse");
        let scoped = scoped_weekly(&response.limits).expect("a limit should be found");
        assert_eq!(scoped.label, "Fable");
    }

    #[test]
    fn ignores_a_scoped_limit_without_a_model_label() {
        let body = r#"{
            "limits": [
                {"kind": "weekly_scoped", "percent": 29, "resets_at": null, "scope": {"model": null}}
            ]
        }"#;

        let response: UsageResponse = serde_json::from_str(body).expect("payload should parse");
        assert!(scoped_weekly(&response.limits).is_none());
    }

    #[test]
    fn tolerates_a_payload_without_any_limits_array() {
        let body = r#"{"five_hour": {"utilization": 1.0, "resets_at": null}}"#;
        let response: UsageResponse = serde_json::from_str(body).expect("payload should parse");
        assert!(scoped_weekly(&response.limits).is_none());
    }

    fn codex_window(used_percent: f64, reset_at: i64, window_seconds: i64) -> String {
        format!(
            r#"{{"used_percent": {used_percent}, "reset_at": {reset_at}, "limit_window_seconds": {window_seconds}}}"#
        )
    }

    fn codex_payload(primary: &str, secondary: &str) -> CodexUsageResponse {
        let body = format!(
            r#"{{"rate_limit": {{"primary_window": {primary}, "secondary_window": {secondary}}}}}"#
        );
        serde_json::from_str(&body).expect("codex payload should parse")
    }

    #[test]
    fn claude_failure_does_not_block_codex_when_both_are_enabled() {
        let data = poll_with(&[ProviderId::ClaudeCode, ProviderId::Codex], |provider| {
            match provider {
                ProviderId::ClaudeCode => Err(PollError::AuthRequired),
                ProviderId::Codex => Ok(usage_with_session_percent(42.0)),
                ProviderId::Antigravity => unreachable!("antigravity is disabled"),
            }
        })
        .expect("codex data should keep the poll successful");

        assert!(data.get(ProviderId::ClaudeCode).is_none());
        assert_eq!(
            data.get(ProviderId::Codex).unwrap().session.percentage,
            42.0
        );
    }

    #[test]
    fn codex_failure_does_not_block_claude_when_both_are_enabled() {
        let data = poll_with(&[ProviderId::ClaudeCode, ProviderId::Codex], |provider| {
            match provider {
                ProviderId::ClaudeCode => Ok(usage_with_session_percent(64.0)),
                ProviderId::Codex => Err(PollError::RequestFailed),
                ProviderId::Antigravity => unreachable!("antigravity is disabled"),
            }
        })
        .expect("claude data should keep the poll successful");

        assert_eq!(
            data.get(ProviderId::ClaudeCode).unwrap().session.percentage,
            64.0
        );
        assert!(data.get(ProviderId::Codex).is_none());
    }

    #[test]
    fn returns_first_error_when_no_enabled_provider_succeeds() {
        let error = poll_with(&crate::providers::PROVIDERS, |provider| match provider {
            ProviderId::ClaudeCode => Err(PollError::AuthRequired),
            ProviderId::Codex => Err(PollError::RequestFailed),
            ProviderId::Antigravity => Err(PollError::NoCredentials),
        })
        .expect_err("all-provider failure should return an error");

        assert_eq!(error, PollError::AuthRequired);
    }

    #[test]
    fn antigravity_failure_does_not_block_codex_when_both_are_enabled() {
        let data = poll_with(&[ProviderId::Codex, ProviderId::Antigravity], |provider| {
            match provider {
                ProviderId::ClaudeCode => unreachable!("claude code is disabled"),
                ProviderId::Codex => Ok(usage_with_session_percent(42.0)),
                ProviderId::Antigravity => Err(PollError::NoCredentials),
            }
        })
        .expect("codex data should keep the poll successful");

        assert!(data.get(ProviderId::Antigravity).is_none());
        assert_eq!(
            data.get(ProviderId::Codex).unwrap().session.percentage,
            42.0
        );
    }

    #[test]
    fn codex_maps_a_weekly_window_by_its_length_not_by_its_position() {
        // The server has been seen delivering the weekly window as
        // `primary_window` with `secondary_window` empty: position must not
        // decide which row the value lands on.
        let response = codex_payload(&codex_window(63.0, 1_800_000_000, 604_800), "null");
        let usage = codex_usage_from_response(response).expect("the weekly window should map");

        assert_eq!(usage.weekly.percentage, 63.0);
        assert!(usage.weekly.resets_at.is_some());
        assert!(usage.session.resets_at.is_none());
    }

    #[test]
    fn codex_maps_a_five_hour_window_to_the_session_row() {
        let response = codex_payload(&codex_window(20.0, 1_800_000_000, 18_000), "null");
        let usage = codex_usage_from_response(response).expect("the 5h window should map");

        assert_eq!(usage.session.percentage, 20.0);
        assert!(usage.session.resets_at.is_some());
        assert!(usage.weekly.resets_at.is_none());
    }

    #[test]
    fn codex_keeps_both_windows_when_the_lengths_are_nominal() {
        let response = codex_payload(
            &codex_window(20.0, 1_800_000_000, 18_000),
            &codex_window(80.0, 1_800_100_000, 604_800),
        );
        let usage = codex_usage_from_response(response).expect("both windows should map");

        assert_eq!(usage.session.percentage, 20.0);
        assert_eq!(usage.weekly.percentage, 80.0);
    }

    #[test]
    fn codex_tolerates_a_null_reset_at() {
        let body = r#"{"rate_limit": {"primary_window": {"used_percent": 5.0, "reset_at": null}}}"#;
        let response: CodexUsageResponse =
            serde_json::from_str(body).expect("a null reset_at should not fail the response");
        let usage = codex_usage_from_response(response).expect("the window should still map");

        assert_eq!(usage.session.percentage, 5.0);
        assert!(usage.session.resets_at.is_none());
    }

    #[test]
    fn codex_ignores_a_window_without_a_used_percent() {
        let body = r#"{"rate_limit": {"primary_window": {"reset_at": 1800000000}}}"#;
        let response: CodexUsageResponse =
            serde_json::from_str(body).expect("a missing used_percent should not fail the response");

        assert!(codex_usage_from_response(response).is_none());
    }

    #[test]
    fn codex_returns_nothing_when_rate_limit_is_null() {
        let response: CodexUsageResponse =
            serde_json::from_str(r#"{"rate_limit": null}"#).expect("payload should parse");

        assert!(codex_usage_from_response(response).is_none());
    }

    #[test]
    fn codex_returns_nothing_when_both_windows_are_absent() {
        assert!(codex_usage_from_response(codex_payload("null", "null")).is_none());
    }

    #[test]
    fn codex_treats_a_zero_reset_at_as_no_reset() {
        let response = codex_payload(&codex_window(7.0, 0, 18_000), "null");
        let usage = codex_usage_from_response(response).expect("the window should map");

        assert!(usage.session.resets_at.is_none());
    }

    #[test]
    fn codex_keeps_the_usable_window_when_the_other_is_unusable() {
        let body = format!(
            r#"{{"rate_limit": {{"primary_window": {{"reset_at": 1800000000}}, "secondary_window": {}}}}}"#,
            codex_window(80.0, 1_800_100_000, 604_800)
        );
        let response: CodexUsageResponse =
            serde_json::from_str(&body).expect("payload should parse");
        let usage = codex_usage_from_response(response).expect("the weekly window should map");

        assert_eq!(usage.weekly.percentage, 80.0);
        assert!(usage.session.resets_at.is_none());
    }

    #[test]
    fn codex_ignores_a_second_window_that_lands_on_the_same_row() {
        // Both keys carry a weekly window: the first one wins, and the second
        // must not overwrite it.
        let response = codex_payload(
            &codex_window(40.0, 1_800_000_000, 604_800),
            &codex_window(90.0, 1_800_100_000, 604_800),
        );
        let usage = codex_usage_from_response(response).expect("a weekly window should map");

        assert_eq!(usage.weekly.percentage, 40.0);
        assert!(usage.session.resets_at.is_none());
    }

    #[test]
    fn codex_falls_back_to_the_key_when_the_length_is_missing() {
        let body = r#"{"rate_limit": {"primary_window": {"used_percent": 9.0, "reset_at": 1800000000}}}"#;
        let response: CodexUsageResponse = serde_json::from_str(body).expect("payload should parse");
        let usage = codex_usage_from_response(response).expect("the legacy shape should still map");

        assert_eq!(usage.session.percentage, 9.0);
    }

    #[test]
    fn codex_falls_back_to_the_weekly_key_when_its_length_is_missing() {
        let body =
            r#"{"rate_limit": {"secondary_window": {"used_percent": 33.0, "reset_at": 1800000000}}}"#;
        let response: CodexUsageResponse =
            serde_json::from_str(body).expect("payload should parse");
        let usage = codex_usage_from_response(response).expect("the window should map");

        assert_eq!(usage.weekly.percentage, 33.0);
    }

    #[test]
    fn a_provider_reported_twice_keeps_the_latest_usage() {
        let mut data = AppUsageData::default();
        data.set(ProviderId::Codex, usage_with_session_percent(10.0));
        data.set(ProviderId::Codex, usage_with_session_percent(20.0));

        assert_eq!(data.iter().count(), 1, "one provider, one entry");
        assert_eq!(
            data.get(ProviderId::Codex).unwrap().session.percentage,
            20.0
        );
    }

    #[test]
    fn codex_treats_an_unfamiliar_long_window_as_weekly() {
        let response = codex_payload(&codex_window(30.0, 1_800_000_000, 2_592_000), "null");
        let usage = codex_usage_from_response(response).expect("the monthly window should map");

        assert_eq!(usage.weekly.percentage, 30.0);
    }

    #[test]
    fn antigravity_summary_prefers_gemini_group() {
        let response: AntigravityQuotaSummaryResponse = serde_json::from_str(
            r#"{
                "groups": [
                    {
                        "displayName": "Claude and GPT models",
                        "buckets": [
                            {
                                "bucketId": "3p-weekly",
                                "window": "weekly",
                                "resetTime": "2026-06-20T18:32:02Z",
                                "remainingFraction": 1
                            },
                            {
                                "bucketId": "3p-5h",
                                "window": "5h",
                                "resetTime": "2026-06-13T23:32:02Z",
                                "remainingFraction": 1
                            }
                        ]
                    },
                    {
                        "displayName": "Gemini Models",
                        "description": "Models within this group: Gemini Flash, Gemini Pro",
                        "buckets": [
                            {
                                "bucketId": "gemini-weekly",
                                "displayName": "Weekly Limit",
                                "window": "weekly",
                                "resetTime": "2026-06-20T17:08:54Z",
                                "remainingFraction": 0.99304295
                            },
                            {
                                "bucketId": "gemini-5h",
                                "displayName": "Five Hour Limit",
                                "window": "5h",
                                "resetTime": "2026-06-13T22:08:54Z",
                                "remainingFraction": 0.9582575
                            }
                        ]
                    }
                ]
            }"#,
        )
        .expect("summary response should deserialize");

        let usage =
            antigravity_usage_from_summary(response).expect("Gemini quota should be selected");

        assert!((usage.weekly.percentage - 0.695705).abs() < 0.000001);
        assert!((usage.session.percentage - 4.17425).abs() < 0.000001);
        assert!(usage.weekly.resets_at.is_some());
        assert!(usage.session.resets_at.is_some());
    }
}
