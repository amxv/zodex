use std::fs::{File, OpenOptions};

use fs2::FileExt as _;
use semver::Version;
use sha2::{Digest as _, Sha256};

const OPERATOR_UPGRADE_SCHEMA_VERSION: u32 = 1;
const OPERATOR_LATEST_API: &str = "https://api.github.com/repos/amxv/zodex/releases/latest";
const OPERATOR_RELEASE_BASE: &str = "https://github.com/amxv/zodex/releases/download";
#[cfg(not(target_os = "windows"))]
const EMBEDDED_OPERATOR_INSTALLER: &str = include_str!("../../../scripts/install.sh");
const UPGRADE_CHECK_CACHE_SECONDS: i64 = 5 * 60;
const UPGRADE_DOWNLOAD_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy)]
struct OperatorUpgradeOptions<'a> {
    version: &'a str,
    check: bool,
    format: UpgradeFormat,
    stop_local: bool,
    refresh: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UpgradeLatestCache {
    schema_version: u32,
    checked_at_unix_seconds: i64,
    latest_version: String,
}

#[derive(Debug, Deserialize)]
struct GithubLatestRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeDirection {
    UpToDate,
    Upgrade,
    Downgrade,
    Ahead,
}

impl UpgradeDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Upgrade => "upgrade",
            Self::Downgrade => "downgrade",
            Self::Ahead => "ahead",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeLocalState {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Unconfigured,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Stopped,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Running,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Stale,
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Unsupported,
}

impl UpgradeLocalState {
    fn as_str(self) -> &'static str {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Unconfigured => "unconfigured",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Stopped => "stopped",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Running => "running",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::Stale => "stale",
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            Self::Unsupported => "unsupported",
        }
    }

    fn blocks_upgrade(self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            matches!(self, Self::Running | Self::Stale)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }
}

#[derive(Debug, Serialize)]
struct UpgradeEvent<'a> {
    schema_version: u32,
    event: &'a str,
    current_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_state: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_blocks_upgrade: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    message: &'a str,
}

struct UpgradeEmitter {
    format: UpgradeFormat,
    current_version: String,
}

impl UpgradeEmitter {
    fn new(format: UpgradeFormat) -> Self {
        Self {
            format,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn progress(&self, event: &'static str, target: Option<&str>, message: &str) {
        self.emit(UpgradeEvent {
            schema_version: OPERATOR_UPGRADE_SCHEMA_VERSION,
            event,
            current_version: &self.current_version,
            target_version: target,
            update_available: None,
            direction: None,
            local_state: None,
            local_blocks_upgrade: None,
            code: None,
            message,
        });
    }

    fn check_complete(
        &self,
        target: &str,
        direction: UpgradeDirection,
        local_state: UpgradeLocalState,
        message: &str,
    ) {
        self.emit(UpgradeEvent {
            schema_version: OPERATOR_UPGRADE_SCHEMA_VERSION,
            event: "check_complete",
            current_version: &self.current_version,
            target_version: Some(target),
            update_available: Some(direction == UpgradeDirection::Upgrade),
            direction: Some(direction.as_str()),
            local_state: Some(local_state.as_str()),
            local_blocks_upgrade: Some(local_state.blocks_upgrade()),
            code: None,
            message,
        });
    }

    fn failure(&self, code: &'static str, target: Option<&str>, message: &str) {
        self.emit(UpgradeEvent {
            schema_version: OPERATOR_UPGRADE_SCHEMA_VERSION,
            event: "failed",
            current_version: &self.current_version,
            target_version: target,
            update_available: None,
            direction: None,
            local_state: None,
            local_blocks_upgrade: None,
            code: Some(code),
            message,
        });
    }

    fn emit(&self, event: UpgradeEvent<'_>) {
        let line = match self.format {
            UpgradeFormat::Human => event.message.to_string(),
            UpgradeFormat::Json => match serde_json::to_string(&event) {
                Ok(value) => value,
                Err(_) => return,
            },
        };
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

#[derive(Debug)]
struct UpgradeFailure {
    code: &'static str,
    message: String,
}

impl UpgradeFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

struct OperatorUpgradeLock {
    _file: File,
}

impl OperatorUpgradeLock {
    fn acquire() -> std::result::Result<Self, UpgradeFailure> {
        let path = upgrade_state_root()?.join("upgrade.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|error| {
            UpgradeFailure::new(
                "upgrade_lock_failed",
                format!("Could not open the upgrade lock {}: {error}", path.display()),
            )
        })?;
        file.try_lock_exclusive().map_err(|error| {
            UpgradeFailure::new(
                "upgrade_in_progress",
                format!("Another Zodex upgrade is already in progress: {error}"),
            )
        })?;
        Ok(Self { _file: file })
    }
}

async fn upgrade_operator(options: OperatorUpgradeOptions<'_>) -> Result<()> {
    let emitter = UpgradeEmitter::new(options.format);
    match run_operator_upgrade(options, &emitter).await {
        Ok(()) => Ok(()),
        Err(failure) => {
            if options.format == UpgradeFormat::Json {
                emitter.failure(failure.code, None, &failure.message);
            }
            bail!(failure.message)
        }
    }
}

async fn run_operator_upgrade(
    options: OperatorUpgradeOptions<'_>,
    emitter: &UpgradeEmitter,
) -> std::result::Result<(), UpgradeFailure> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
        UpgradeFailure::new("invalid_current_version", format!("Invalid installed Zodex version: {error}"))
    })?;
    let selector = normalize_operator_version_selector(options.version)?;

    if selector != "latest" {
        let target = parse_release_version(&selector)?;
        if target == current {
            let local_state = operator_upgrade_local_state()?;
            emitter.check_complete(
                &target.to_string(),
                UpgradeDirection::UpToDate,
                local_state,
                &format!("Zodex v{current} is already installed."),
            );
            return Ok(());
        }
        return continue_operator_upgrade(options, emitter, current, target, false).await;
    }

    emitter.progress("checking", None, "Checking for updates…");
    let client = operator_upgrade_http_client()?;
    let target = resolve_latest_operator_version(
        &client,
        options.check && !options.refresh,
    )
    .await?;

    let direction = latest_direction(&current, &target);
    let local_state = operator_upgrade_local_state()?;
    let message = match direction {
        UpgradeDirection::UpToDate => format!("Zodex v{current} is up to date."),
        UpgradeDirection::Ahead => format!(
            "Zodex v{current} is newer than the latest published release v{target}."
        ),
        UpgradeDirection::Upgrade => format!("Update available: v{current} → v{target}"),
        UpgradeDirection::Downgrade => unreachable!("latest releases never request a downgrade"),
    };
    emitter.check_complete(&target.to_string(), direction, local_state, &message);
    if options.check || direction != UpgradeDirection::Upgrade {
        return Ok(());
    }

    continue_operator_upgrade(options, emitter, current, target, true).await
}

async fn continue_operator_upgrade(
    options: OperatorUpgradeOptions<'_>,
    emitter: &UpgradeEmitter,
    current: Version,
    target: Version,
    latest: bool,
) -> std::result::Result<(), UpgradeFailure> {
    let direction = requested_direction(&current, &target, latest);
    let local_state = operator_upgrade_local_state()?;
    if options.check {
        if !latest {
            let client = operator_upgrade_http_client()?;
            ensure_operator_release_exists(&client, &target).await?;
        }
        let message = match direction {
            UpgradeDirection::Upgrade => format!("Update available: v{current} → v{target}"),
            UpgradeDirection::Downgrade => format!("Requested version differs: v{current} → v{target}"),
            UpgradeDirection::UpToDate => format!("Zodex v{current} is already installed."),
            UpgradeDirection::Ahead => format!("Zodex v{current} is newer than v{target}."),
        };
        emitter.check_complete(&target.to_string(), direction, local_state, &message);
        return Ok(());
    }

    let _lock = OperatorUpgradeLock::acquire()?;
    let mut local_state = operator_upgrade_local_state()?;
    if local_state.blocks_upgrade() {
        if !options.stop_local {
            return Err(UpgradeFailure::new(
                "local_running",
                "Zodex Local must be stopped before upgrading. Run `zodex local stop`, or retry with `zodex upgrade --stop-local`.",
            ));
        }
        let client = operator_upgrade_http_client()?;
        ensure_operator_release_exists(&client, &target).await?;
        emitter.progress(
            "stopping_local",
            Some(&target.to_string()),
            "Stopping Zodex Local…",
        );
        stop_local_for_operator_upgrade().await?;
        local_state = operator_upgrade_local_state()?;
        if local_state.blocks_upgrade() {
            return Err(UpgradeFailure::new(
                "local_stop_failed",
                "Zodex Local still has blocking runtime state after the stop request.",
            ));
        }
    }

    let target_text = target.to_string();
    emitter.progress(
        "downloading",
        Some(&target_text),
        &format!("Downloading Zodex v{target}…"),
    );
    let client = operator_upgrade_http_client()?;
    let temp = tempfile::tempdir().map_err(|error| {
        UpgradeFailure::new("temporary_directory_failed", format!("Could not create upgrade workspace: {error}"))
    })?;
    let target_triple = operator_target_triple()?;
    let archive_name = format!("zodex-{target_triple}.tar.gz");
    let tag = format!("v{target}");
    let base = operator_release_asset_url(&tag, &archive_name);
    let archive_path = temp.path().join(&archive_name);
    let checksum_path = temp.path().join(format!("{archive_name}.sha256"));
    let installer_path = temp.path().join("install.sh");

    download_upgrade_file(&client, &base, &archive_path, emitter, &target_text).await?;
    download_upgrade_file(
        &client,
        &format!("{base}.sha256"),
        &checksum_path,
        emitter,
        &target_text,
    )
    .await?;
    #[cfg(not(target_os = "windows"))]
    fs::write(&installer_path, EMBEDDED_OPERATOR_INSTALLER).map_err(|error| {
        UpgradeFailure::new(
            "install_failed",
            format!("Could not materialize the embedded Zodex installer: {error}"),
        )
    })?;

    emitter.progress("verifying", Some(&target_text), "Verifying release checksum…");
    verify_upgrade_checksum(&archive_path, &checksum_path)?;

    emitter.progress("installing", Some(&target_text), "Installing update…");
    let extracted_dir = temp.path().join(format!("zodex-{target_triple}"));
    extract_upgrade_archive(&archive_path, temp.path())?;
    if !extracted_dir.join(operator_binary_name()).is_file() {
        return Err(UpgradeFailure::new(
            "release_archive_invalid",
            format!(
                "Release archive did not contain {}/{}",
                extracted_dir.display(),
                operator_binary_name()
            ),
        ));
    }
    install_extracted_operator(&installer_path, &extracted_dir)?;

    emitter.progress(
        "complete",
        Some(&target_text),
        &format!("Updated Zodex to v{target}."),
    );
    Ok(())
}

fn operator_release_asset_url(tag: &str, archive_name: &str) -> String {
    let base = env::var("ZODEX_UPGRADE_RELEASE_BASE")
        .unwrap_or_else(|_| OPERATOR_RELEASE_BASE.to_string());
    format!("{}/{tag}/{archive_name}", base.trim_end_matches('/'))
}

fn operator_latest_api() -> String {
    env::var("ZODEX_UPGRADE_LATEST_API").unwrap_or_else(|_| OPERATOR_LATEST_API.to_string())
}

async fn ensure_operator_release_exists(
    client: &reqwest::Client,
    target: &Version,
) -> std::result::Result<(), UpgradeFailure> {
    let target_triple = operator_target_triple()?;
    let archive_name = format!("zodex-{target_triple}.tar.gz");
    let url = operator_release_asset_url(&format!("v{target}"), &archive_name);
    let response = client.head(&url).send().await.map_err(|error| {
        UpgradeFailure::new(
            "network",
            format!("Could not verify Zodex v{target} release availability: {error}"),
        )
    })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(UpgradeFailure::new(
            "release_not_found",
            format!("Zodex release v{target} does not provide {archive_name}."),
        ));
    }
    if !response.status().is_success() {
        return Err(UpgradeFailure::new(
            "release_metadata",
            format!(
                "Could not verify Zodex v{target} release availability: HTTP {}",
                response.status()
            ),
        ));
    }
    Ok(())
}

fn normalize_operator_version_selector(value: &str) -> std::result::Result<String, UpgradeFailure> {
    let trimmed = value.trim();
    if trimmed == "latest" {
        return Ok("latest".to_string());
    }
    let bare = trimmed.strip_prefix('v').unwrap_or(trimmed);
    parse_release_version(bare)?;
    Ok(bare.to_string())
}

fn parse_release_version(value: &str) -> std::result::Result<Version, UpgradeFailure> {
    Version::parse(value.strip_prefix('v').unwrap_or(value)).map_err(|error| {
        UpgradeFailure::new(
            "invalid_version",
            format!("Invalid Zodex release version `{value}`: {error}"),
        )
    })
}

fn latest_direction(current: &Version, target: &Version) -> UpgradeDirection {
    match current.cmp(target) {
        std::cmp::Ordering::Less => UpgradeDirection::Upgrade,
        std::cmp::Ordering::Equal => UpgradeDirection::UpToDate,
        std::cmp::Ordering::Greater => UpgradeDirection::Ahead,
    }
}

fn requested_direction(current: &Version, target: &Version, latest: bool) -> UpgradeDirection {
    if latest {
        return latest_direction(current, target);
    }
    match current.cmp(target) {
        std::cmp::Ordering::Less => UpgradeDirection::Upgrade,
        std::cmp::Ordering::Equal => UpgradeDirection::UpToDate,
        std::cmp::Ordering::Greater => UpgradeDirection::Downgrade,
    }
}

fn operator_upgrade_http_client() -> std::result::Result<reqwest::Client, UpgradeFailure> {
    reqwest::Client::builder()
        .user_agent(format!("zodex/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|error| UpgradeFailure::new("network", format!("Could not create HTTP client: {error}")))
}

async fn resolve_latest_operator_version(
    client: &reqwest::Client,
    allow_cache: bool,
) -> std::result::Result<Version, UpgradeFailure> {
    if allow_cache
        && let Ok(Some(version)) = load_fresh_upgrade_cache()
    {
        return Ok(version);
    }
    let response = client
        .get(operator_latest_api())
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .send()
        .await
        .map_err(|error| UpgradeFailure::new("network", format!("Could not check the latest Zodex release: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(UpgradeFailure::new(
            "release_metadata",
            format!("Latest Zodex release lookup failed with HTTP {status}."),
        ));
    }
    let release: GithubLatestRelease = response.json().await.map_err(|error| {
        UpgradeFailure::new("release_metadata", format!("Could not parse latest Zodex release metadata: {error}"))
    })?;
    if release.draft || release.prerelease {
        return Err(UpgradeFailure::new(
            "release_metadata",
            format!("Latest Zodex release unexpectedly resolved to draft/prerelease `{}`.", release.tag_name),
        ));
    }
    let version = parse_release_version(&release.tag_name)?;
    let _ = save_upgrade_cache(&version);
    Ok(version)
}

fn load_fresh_upgrade_cache() -> std::result::Result<Option<Version>, UpgradeFailure> {
    let path = upgrade_cache_file()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    let cache: UpgradeLatestCache = match serde_json::from_slice(&bytes) {
        Ok(cache) => cache,
        Err(_) => return Ok(None),
    };
    if cache.schema_version != OPERATOR_UPGRADE_SCHEMA_VERSION {
        return Ok(None);
    }
    let age = OffsetDateTime::now_utc()
        .unix_timestamp()
        .saturating_sub(cache.checked_at_unix_seconds);
    if !(0..=UPGRADE_CHECK_CACHE_SECONDS).contains(&age) {
        return Ok(None);
    }
    Ok(parse_release_version(&cache.latest_version).ok())
}

fn save_upgrade_cache(version: &Version) -> std::result::Result<(), UpgradeFailure> {
    let path = upgrade_cache_file()?;
    let cache = UpgradeLatestCache {
        schema_version: OPERATOR_UPGRADE_SCHEMA_VERSION,
        checked_at_unix_seconds: OffsetDateTime::now_utc().unix_timestamp(),
        latest_version: version.to_string(),
    };
    let bytes = serde_json::to_vec(&cache).map_err(|error| {
        UpgradeFailure::new("upgrade_cache", format!("Could not serialize update cache: {error}"))
    })?;
    fs::write(&path, bytes).map_err(|error| {
        UpgradeFailure::new("upgrade_cache", format!("Could not write update cache {}: {error}", path.display()))
    })
}

fn upgrade_cache_file() -> std::result::Result<PathBuf, UpgradeFailure> {
    #[cfg(target_os = "windows")]
    {
        let root = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join("AppData/Local"))
            })
            .ok_or_else(|| {
                UpgradeFailure::new(
                    "upgrade_cache",
                    "LOCALAPPDATA and USERPROFILE are unavailable",
                )
            })?
            .join("zodex/cache");
        create_private_upgrade_dir(&root)?;
        return Ok(root.join("upgrade-check.json"));
    }

    #[cfg(not(target_os = "windows"))]
    {
    let root = env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| UpgradeFailure::new("upgrade_cache", "HOME is not set and XDG_CACHE_HOME is unavailable"))?
        .join("zodex");
    create_private_upgrade_dir(&root)?;
    Ok(root.join("upgrade-check.json"))
    }
}

fn upgrade_state_root() -> std::result::Result<PathBuf, UpgradeFailure> {
    #[cfg(target_os = "windows")]
    {
        let root = env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .filter(|value| !value.is_empty())
                    .map(|home| PathBuf::from(home).join("AppData/Local"))
            })
            .ok_or_else(|| {
                UpgradeFailure::new(
                    "upgrade_lock_failed",
                    "LOCALAPPDATA and USERPROFILE are unavailable",
                )
            })?
            .join("zodex/state");
        create_private_upgrade_dir(&root)?;
        return Ok(root);
    }

    #[cfg(not(target_os = "windows"))]
    {
    let root = env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or_else(|| UpgradeFailure::new("upgrade_lock_failed", "HOME is not set and XDG_STATE_HOME is unavailable"))?
        .join("zodex");
    create_private_upgrade_dir(&root)?;
    Ok(root)
    }
}

fn create_private_upgrade_dir(path: &Path) -> std::result::Result<(), UpgradeFailure> {
    fs::create_dir_all(path).map_err(|error| {
        UpgradeFailure::new("upgrade_state", format!("Could not create {}: {error}", path.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

async fn download_upgrade_file(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    emitter: &UpgradeEmitter,
    target: &str,
) -> std::result::Result<(), UpgradeFailure> {
    let mut last_error = String::new();
    for attempt in 1..=UPGRADE_DOWNLOAD_ATTEMPTS {
        match client.get(url).send().await {
            Ok(response) if response.status().is_success() => {
                match response.bytes().await {
                    Ok(bytes) => {
                        fs::write(destination, bytes).map_err(|error| {
                            UpgradeFailure::new(
                                "download_write_failed",
                                format!("Could not write {}: {error}", destination.display()),
                            )
                        })?;
                        return Ok(());
                    }
                    Err(error) => {
                        last_error = format!("download interrupted: {error}");
                    }
                }
            }
            Ok(response) => {
                last_error = format!("HTTP {}", response.status());
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    break;
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < UPGRADE_DOWNLOAD_ATTEMPTS {
            emitter.progress(
                "retrying_download",
                Some(target),
                &format!("Download failed; retrying ({attempt}/{UPGRADE_DOWNLOAD_ATTEMPTS})…"),
            );
            tokio::time::sleep(Duration::from_secs(attempt as u64 * 2)).await;
        }
    }
    let code = if last_error.starts_with("HTTP 404") {
        "release_not_found"
    } else {
        "network"
    };
    Err(UpgradeFailure::new(
        code,
        format!("Could not download {url}: {last_error}"),
    ))
}

fn verify_upgrade_checksum(
    archive: &Path,
    checksum: &Path,
) -> std::result::Result<(), UpgradeFailure> {
    let expected_text = fs::read_to_string(checksum).map_err(|error| {
        UpgradeFailure::new("checksum_failed", format!("Could not read checksum: {error}"))
    })?;
    let expected = expected_text.split_whitespace().next().ok_or_else(|| {
        UpgradeFailure::new("checksum_failed", "Release checksum file was empty")
    })?;
    let mut file = File::open(archive).map_err(|error| {
        UpgradeFailure::new("checksum_failed", format!("Could not open release archive: {error}"))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            UpgradeFailure::new("checksum_failed", format!("Could not read release archive: {error}"))
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(UpgradeFailure::new(
            "checksum_failed",
            format!("Release checksum mismatch: expected {expected}, got {actual}"),
        ));
    }
    Ok(())
}

fn extract_upgrade_archive(
    archive: &Path,
    destination: &Path,
) -> std::result::Result<(), UpgradeFailure> {
    let output = Command::new("tar")
        .args(["-xzf"])
        .arg(archive)
        .arg("-C")
        .arg(destination)
        .output()
        .map_err(|error| UpgradeFailure::new("extract_failed", format!("Could not run tar: {error}")))?;
    if !output.status.success() {
        return Err(UpgradeFailure::new(
            "extract_failed",
            format!("Could not extract release archive: {}", String::from_utf8_lossy(&output.stderr).trim()),
        ));
    }
    Ok(())
}

fn install_extracted_operator(
    installer: &Path,
    extracted_dir: &Path,
) -> std::result::Result<(), UpgradeFailure> {
    #[cfg(target_os = "windows")]
    {
        let _ = installer;
        return install_extracted_operator_windows(extracted_dir);
    }

    #[cfg(not(target_os = "windows"))]
    {
    let current = env::current_exe().map_err(|error| {
        UpgradeFailure::new("install_failed", format!("Could not resolve current zodex executable: {error}"))
    })?;
    let install_dir = current.parent().ok_or_else(|| {
        UpgradeFailure::new("install_failed", "Current zodex executable has no parent directory")
    })?;
    let output = Command::new("/bin/bash")
        .arg(installer)
        .env("ZODEX_INSTALL_MODE", "operator")
        .env("ZODEX_BINARY_SOURCE_DIR", extracted_dir)
        .env("ZODEX_INSTALL_DIR", install_dir)
        .env("ZODEX_UPGRADE_MODE", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| UpgradeFailure::new("install_failed", format!("Could not run Zodex installer: {error}")))?;
    if !output.status.success() {
        let stderr = redact_api_key_query_params(&String::from_utf8_lossy(&output.stderr));
        return Err(UpgradeFailure::new(
            "install_failed",
            if stderr.trim().is_empty() {
                format!("Zodex installer exited with status {}", output.status)
            } else {
                stderr.trim().to_string()
            },
        ));
    }
    Ok(())
    }
}

#[cfg(target_os = "windows")]
fn install_extracted_operator_windows(
    extracted_dir: &Path,
) -> std::result::Result<(), UpgradeFailure> {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    let current = env::current_exe().map_err(|error| {
        UpgradeFailure::new(
            "install_failed",
            format!("Could not resolve current zodex executable: {error}"),
        )
    })?;
    let source = extracted_dir.join(operator_binary_name());
    let state = upgrade_state_root()?;
    let pending = state.join("pending-zodex.exe");
    let helper = state.join("finish-upgrade.ps1");
    fs::copy(&source, &pending).map_err(|error| {
        UpgradeFailure::new(
            "install_failed",
            format!("Could not stage Windows update at {}: {error}", pending.display()),
        )
    })?;
    let script = r#"param(
    [Parameter(Mandatory=$true)][string]$Source,
    [Parameter(Mandatory=$true)][string]$Destination,
    [Parameter(Mandatory=$true)][int]$ParentPid,
    [Parameter(Mandatory=$true)][string]$ScriptPath
)
$ErrorActionPreference = 'Stop'
try {
    Wait-Process -Id $ParentPid -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
    Remove-Item -LiteralPath $Source -Force -ErrorAction SilentlyContinue
} finally {
    Remove-Item -LiteralPath $ScriptPath -Force -ErrorAction SilentlyContinue
}
"#;
    fs::write(&helper, script).map_err(|error| {
        UpgradeFailure::new(
            "install_failed",
            format!("Could not write Windows upgrade helper {}: {error}", helper.display()),
        )
    })?;
    Command::new(windows_powershell_path()?)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&helper)
        .arg("-Source")
        .arg(&pending)
        .arg("-Destination")
        .arg(&current)
        .arg("-ParentPid")
        .arg(std::process::id().to_string())
        .arg("-ScriptPath")
        .arg(&helper)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| {
            UpgradeFailure::new(
                "install_failed",
                format!("Could not launch Windows upgrade helper: {error}"),
            )
        })?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_powershell_path() -> std::result::Result<PathBuf, UpgradeFailure> {
    let system_root = env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            UpgradeFailure::new(
                "install_failed",
                "SystemRoot is unavailable; cannot locate built-in Windows PowerShell",
            )
        })?;
    let powershell = PathBuf::from(system_root)
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !powershell.is_file() {
        return Err(UpgradeFailure::new(
            "install_failed",
            format!(
                "Built-in Windows PowerShell was not found at {}",
                powershell.display()
            ),
        ));
    }
    Ok(powershell)
}

fn operator_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "zodex.exe"
    } else {
        "zodex"
    }
}

fn operator_target_triple() -> std::result::Result<&'static str, UpgradeFailure> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(UpgradeFailure::new(
            "unsupported_platform",
            format!("Zodex operator upgrades do not support {os}/{arch}"),
        )),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn operator_upgrade_local_state() -> std::result::Result<UpgradeLocalState, UpgradeFailure> {
    use zodex::local::{LocalPaths, LocalStatusDocument, LocalStatusState};

    let paths = LocalPaths::discover().map_err(|error| {
        UpgradeFailure::new("local_state", format!("Could not inspect Zodex Local state: {error:#}"))
    })?;
    let status = LocalStatusDocument::inspect(&paths).map_err(|error| {
        UpgradeFailure::new("local_state", format!("Could not inspect Zodex Local state: {error:#}"))
    })?;
    Ok(match status.state {
        LocalStatusState::Unconfigured => UpgradeLocalState::Unconfigured,
        LocalStatusState::Stopped => UpgradeLocalState::Stopped,
        LocalStatusState::Running => UpgradeLocalState::Running,
        LocalStatusState::Stale => UpgradeLocalState::Stale,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn operator_upgrade_local_state() -> std::result::Result<UpgradeLocalState, UpgradeFailure> {
    Ok(UpgradeLocalState::Unsupported)
}

#[cfg(target_os = "macos")]
async fn stop_local_for_operator_upgrade() -> std::result::Result<(), UpgradeFailure> {
    use zodex::local::{LocalPaths, SystemLaunchdController, stop_via_launchd};

    let paths = LocalPaths::discover().map_err(|error| {
        UpgradeFailure::new("local_stop_failed", format!("Could not resolve Zodex Local state: {error:#}"))
    })?;
    let launchd = SystemLaunchdController::for_current_user();
    stop_via_launchd(&paths, &launchd).await.map_err(|error| {
        UpgradeFailure::new("local_stop_failed", format!("Could not stop Zodex Local: {error:#}"))
    })?;
    Ok(())
}

#[cfg(target_os = "windows")]
async fn stop_local_for_operator_upgrade() -> std::result::Result<(), UpgradeFailure> {
    use zodex::local::{LocalPaths, stop_via_windows_process};

    let paths = LocalPaths::discover().map_err(|error| {
        UpgradeFailure::new(
            "local_stop_failed",
            format!("Could not resolve Zodex Local state: {error:#}"),
        )
    })?;
    stop_via_windows_process(&paths).await.map_err(|error| {
        UpgradeFailure::new(
            "local_stop_failed",
            format!("Could not stop Zodex Local: {error:#}"),
        )
    })?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn stop_local_for_operator_upgrade() -> std::result::Result<(), UpgradeFailure> {
    Ok(())
}

#[cfg(test)]
mod operator_upgrade_tests {
    use super::*;

    #[test]
    fn version_selectors_normalize_and_validate_semver() {
        assert_eq!(normalize_operator_version_selector("latest").unwrap(), "latest");
        assert_eq!(normalize_operator_version_selector("0.3.4").unwrap(), "0.3.4");
        assert_eq!(normalize_operator_version_selector("v0.3.4").unwrap(), "0.3.4");
        assert!(normalize_operator_version_selector("banana").is_err());
    }

    #[test]
    fn latest_direction_never_treats_newer_local_build_as_an_update() {
        let old = Version::parse("0.3.4").unwrap();
        let new = Version::parse("0.3.5").unwrap();
        assert_eq!(latest_direction(&old, &new), UpgradeDirection::Upgrade);
        assert_eq!(latest_direction(&new, &old), UpgradeDirection::Ahead);
        assert_eq!(latest_direction(&old, &old), UpgradeDirection::UpToDate);
    }

    #[test]
    fn explicit_versions_can_request_a_downgrade() {
        let current = Version::parse("0.3.5").unwrap();
        let target = Version::parse("0.3.4").unwrap();
        assert_eq!(
            requested_direction(&current, &target, false),
            UpgradeDirection::Downgrade
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_operator_target_and_binary_name_match_release_contract() {
        assert_eq!(operator_target_triple().unwrap(), "x86_64-pc-windows-msvc");
        assert_eq!(operator_binary_name(), "zodex.exe");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_upgrade_uses_builtin_powershell() {
        let system_root = env::var_os("SystemRoot").expect("Windows runner has SystemRoot");
        let expected = PathBuf::from(system_root)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        assert_eq!(windows_powershell_path().unwrap(), expected);
    }

    #[test]
    fn checksum_mismatch_has_a_structured_failure_code() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.tar.gz");
        let checksum = dir.path().join("release.tar.gz.sha256");
        fs::write(&archive, b"fixture release").unwrap();
        fs::write(&checksum, format!("{}  release.tar.gz\n", "0".repeat(64))).unwrap();

        let failure = verify_upgrade_checksum(&archive, &checksum).unwrap_err();
        assert_eq!(failure.code, "checksum_failed");
        assert!(failure.message.contains("checksum mismatch"));
    }
}
