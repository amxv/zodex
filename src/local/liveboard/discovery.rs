use std::fs;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::super::LocalPaths;
use super::super::status::write_user_only_json_atomic;

pub(crate) const LOCAL_LIVEBOARD_DISCOVERY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct LocalLiveboardDiscovery {
    pub(crate) schema_version: u32,
    pub(crate) runtime_id: String,
    pub(crate) base_url: String,
}

impl LocalLiveboardDiscovery {
    pub(crate) fn new(runtime_id: impl Into<String>, base_url: impl Into<String>) -> Result<Self> {
        let value = Self {
            schema_version: LOCAL_LIVEBOARD_DISCOVERY_SCHEMA_VERSION,
            runtime_id: runtime_id.into(),
            base_url: base_url.into(),
        };
        value.validated_base_url()?;
        Ok(value)
    }

    pub(crate) fn focused_url(&self, agent_id: &str) -> Result<String> {
        validate_agent_id(agent_id)?;
        let mut url = self.validated_base_url()?;
        url.set_query(None);
        url.query_pairs_mut().append_pair("agent", agent_id);
        Ok(url.into())
    }

    fn validated_base_url(&self) -> Result<Url> {
        validate_base_url(&self.base_url)
    }
}

pub(crate) fn write_liveboard_discovery(
    paths: &LocalPaths,
    discovery: &LocalLiveboardDiscovery,
) -> Result<()> {
    if discovery.schema_version != LOCAL_LIVEBOARD_DISCOVERY_SCHEMA_VERSION {
        bail!("refusing to write unsupported Local Liveboard discovery schema")
    }
    discovery.validated_base_url()?;
    write_user_only_json_atomic(&paths.liveboard_discovery_file(), discovery)
}

pub(crate) fn load_liveboard_discovery(
    paths: &LocalPaths,
    expected_runtime_id: &str,
) -> Result<LocalLiveboardDiscovery> {
    let path = paths.liveboard_discovery_file();
    let raw = fs::read(&path).with_context(|| {
        format!(
            "Local Liveboard is unavailable: private runtime discovery is missing at {}",
            path.display()
        )
    })?;
    let discovery: LocalLiveboardDiscovery =
        serde_json::from_slice(&raw).context("Local Liveboard private discovery is malformed")?;
    if discovery.schema_version != LOCAL_LIVEBOARD_DISCOVERY_SCHEMA_VERSION {
        bail!("Local Liveboard private discovery has an unsupported schema version")
    }
    if discovery.runtime_id != expected_runtime_id {
        bail!("Local Liveboard private discovery belongs to a different runtime")
    }
    discovery.validated_base_url()?;
    Ok(discovery)
}

pub(crate) fn remove_liveboard_discovery(paths: &LocalPaths) {
    let _ = fs::remove_file(paths.liveboard_discovery_file());
}

pub(crate) fn validate_agent_id(agent_id: &str) -> Result<()> {
    if agent_id.len() != 4
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        bail!("Agent ID must be exactly four lowercase letters or digits")
    }
    Ok(())
}

fn validate_base_url(value: &str) -> Result<Url> {
    let url =
        Url::parse(value).context("Local Liveboard private discovery contains an invalid URL")?;
    let path_segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with('/')
        || path_segments.len() != 1
        || path_segments[0].len() < 24
        || !path_segments[0]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("Local Liveboard URL must be a credential-free loopback capability URL")
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::*;

    fn test_paths() -> (tempfile::TempDir, LocalPaths) {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn discovery_is_private_runtime_scoped_and_builds_canonical_focus_url() {
        let (_dir, paths) = test_paths();
        let discovery = LocalLiveboardDiscovery::new(
            "runtime-a",
            "http://127.0.0.1:43123/abcdefghijklmnopqrstuvwxyz012345/",
        )
        .unwrap();
        write_liveboard_discovery(&paths, &discovery).unwrap();

        #[cfg(unix)]
        {
            let mode = fs::metadata(paths.liveboard_discovery_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let loaded = load_liveboard_discovery(&paths, "runtime-a").unwrap();
        assert_eq!(
            loaded.focused_url("k7m2").unwrap(),
            "http://127.0.0.1:43123/abcdefghijklmnopqrstuvwxyz012345/?agent=k7m2"
        );
        assert!(load_liveboard_discovery(&paths, "runtime-b").is_err());

        remove_liveboard_discovery(&paths);
        assert!(!paths.liveboard_discovery_file().exists());
    }

    #[test]
    fn discovery_rejects_non_loopback_or_query_bearing_capabilities() {
        assert!(
            LocalLiveboardDiscovery::new(
                "runtime-a",
                "http://example.com:43123/abcdefghijklmnopqrstuvwxyz012345/"
            )
            .is_err()
        );
        assert!(
            LocalLiveboardDiscovery::new(
                "runtime-a",
                "http://127.0.0.1:43123/abcdefghijklmnopqrstuvwxyz012345/?agent=k7m2"
            )
            .is_err()
        );
        assert!(validate_agent_id("K7M2").is_err());
        assert!(validate_agent_id("abc").is_err());
    }

    #[test]
    fn discovery_rejects_unknown_schema_before_use() {
        let (_dir, paths) = test_paths();
        fs::create_dir_all(paths.runtime_dir()).unwrap();
        fs::write(
            paths.liveboard_discovery_file(),
            br#"{"schema_version":99,"runtime_id":"runtime-a","base_url":"http://127.0.0.1:43123/abcdefghijklmnopqrstuvwxyz012345/"}"#,
        )
        .unwrap();

        let error = load_liveboard_discovery(&paths, "runtime-a")
            .err()
            .expect("unknown schema must fail closed");
        assert!(error.to_string().contains("unsupported schema version"));
    }
}
