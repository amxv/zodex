use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};

use anyhow::{Context, Result, bail};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};

use super::super::LocalPaths;
use super::super::status::write_user_only_json_atomic;

pub(crate) const LIVEBOARD_PREFERENCES_SCHEMA_VERSION: u32 = 1;
const MIN_VISIBLE_AGENTS: u8 = 1;
const MAX_VISIBLE_AGENTS: u8 = 8;
const MAX_ALIAS_CHARS: usize = 80;
const MAX_EDITOR_COMMAND_CHARS: usize = 256;
const MIN_WIDTH_WEIGHT: f64 = 0.1;
const MAX_WIDTH_WEIGHT: f64 = 10.0;
const MAX_ORDER: u32 = 1_024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveboardTheme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct LiveboardPreferences {
    pub(crate) schema_version: u32,
    pub(crate) theme: LiveboardTheme,
    pub(crate) max_visible_agents: u8,
    pub(crate) command_outputs_expanded: bool,
    pub(crate) diffs_expanded: bool,
    #[serde(default)]
    pub(crate) show_raw_button: bool,
    #[serde(default = "default_editor_command")]
    pub(crate) editor_command: String,
    pub(crate) agents: BTreeMap<String, LiveboardAgentPreference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct LiveboardAgentPreference {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) order: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width_weight: Option<f64>,
}

impl Default for LiveboardPreferences {
    fn default() -> Self {
        Self {
            schema_version: LIVEBOARD_PREFERENCES_SCHEMA_VERSION,
            theme: LiveboardTheme::System,
            max_visible_agents: 4,
            command_outputs_expanded: false,
            diffs_expanded: true,
            show_raw_button: false,
            editor_command: default_editor_command(),
            agents: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveboardPreferencesPatch {
    pub(crate) schema_version: Option<u32>,
    pub(crate) theme: Option<LiveboardTheme>,
    pub(crate) max_visible_agents: Option<u8>,
    pub(crate) command_outputs_expanded: Option<bool>,
    pub(crate) diffs_expanded: Option<bool>,
    pub(crate) show_raw_button: Option<bool>,
    pub(crate) editor_command: Option<String>,
    #[serde(default)]
    pub(crate) agents: BTreeMap<String, LiveboardAgentPreferencePatch>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct LiveboardAgentPreferencePatch {
    pub(crate) alias: Option<String>,
    pub(crate) visible: Option<bool>,
    pub(crate) order: Option<u32>,
    pub(crate) width_weight: Option<f64>,
}

impl LiveboardPreferencesPatch {
    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .schema_version
            .is_some_and(|version| version != LIVEBOARD_PREFERENCES_SCHEMA_VERSION)
        {
            bail!(
                "Liveboard preference mutation schema must be {}",
                LIVEBOARD_PREFERENCES_SCHEMA_VERSION
            );
        }
        if let Some(max_visible_agents) = self.max_visible_agents
            && !(MIN_VISIBLE_AGENTS..=MAX_VISIBLE_AGENTS).contains(&max_visible_agents)
        {
            bail!(
                "max_visible_agents must be between {MIN_VISIBLE_AGENTS} and {MAX_VISIBLE_AGENTS}"
            );
        }
        if let Some(editor_command) = self.editor_command.as_deref() {
            validate_editor_command(editor_command)?;
        }
        for (agent_id, preference) in &self.agents {
            validate_agent_id(agent_id)?;
            preference.validate()?;
        }
        Ok(())
    }
}

impl LiveboardAgentPreferencePatch {
    fn validate(&self) -> Result<()> {
        if let Some(alias) = self.alias.as_deref() {
            validate_alias(alias)?;
        }
        if self.order.is_some_and(|order| order > MAX_ORDER) {
            bail!("Agent board order must be at most {MAX_ORDER}");
        }
        if let Some(weight) = self.width_weight
            && (!weight.is_finite() || !(MIN_WIDTH_WEIGHT..=MAX_WIDTH_WEIGHT).contains(&weight))
        {
            bail!(
                "Agent width weight must be finite and between {MIN_WIDTH_WEIGHT} and {MAX_WIDTH_WEIGHT}"
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct LiveboardPreferencesStore {
    paths: LocalPaths,
}

impl LiveboardPreferencesStore {
    pub(crate) fn new(paths: &LocalPaths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }

    pub(crate) fn load(&self) -> Result<LiveboardPreferences> {
        load_preferences(&self.paths)
    }

    pub(crate) fn mutate(&self, patch: &LiveboardPreferencesPatch) -> Result<LiveboardPreferences> {
        patch.validate()?;
        let _lock = LiveboardPreferenceLock::acquire(&self.paths)?;
        let mut current = load_preferences(&self.paths)?;
        merge_preferences(&mut current, patch);
        validate_preferences(&current)?;
        write_user_only_json_atomic(&self.paths.liveboard_preferences_file(), &current)?;
        Ok(current)
    }
}

fn load_preferences(paths: &LocalPaths) -> Result<LiveboardPreferences> {
    let path = paths.liveboard_preferences_file();
    if !path.exists() {
        return Ok(LiveboardPreferences::default());
    }
    let raw = fs::read(&path)
        .with_context(|| format!("failed to read Liveboard preferences at {}", path.display()))?;
    let preferences: LiveboardPreferences = serde_json::from_slice(&raw).with_context(|| {
        format!(
            "failed to parse Liveboard preferences at {}",
            path.display()
        )
    })?;
    if preferences.schema_version != LIVEBOARD_PREFERENCES_SCHEMA_VERSION {
        bail!(
            "unsupported Liveboard preference schema version {} at {}; expected {}",
            preferences.schema_version,
            path.display(),
            LIVEBOARD_PREFERENCES_SCHEMA_VERSION
        );
    }
    validate_preferences(&preferences)?;
    Ok(preferences)
}

fn merge_preferences(current: &mut LiveboardPreferences, patch: &LiveboardPreferencesPatch) {
    if let Some(theme) = patch.theme {
        current.theme = theme;
    }
    if let Some(max_visible_agents) = patch.max_visible_agents {
        current.max_visible_agents = max_visible_agents;
    }
    if let Some(expanded) = patch.command_outputs_expanded {
        current.command_outputs_expanded = expanded;
    }
    if let Some(expanded) = patch.diffs_expanded {
        current.diffs_expanded = expanded;
    }
    if let Some(show_raw_button) = patch.show_raw_button {
        current.show_raw_button = show_raw_button;
    }
    if let Some(editor_command) = patch.editor_command.as_deref() {
        current.editor_command = editor_command.trim().to_owned();
    }
    for (agent_id, patch) in &patch.agents {
        let preference = current.agents.entry(agent_id.clone()).or_default();
        if let Some(alias) = patch.alias.as_deref() {
            let alias = alias.trim();
            preference.alias = (!alias.is_empty()).then(|| alias.to_owned());
        }
        if let Some(visible) = patch.visible {
            preference.visible = Some(visible);
        }
        if let Some(order) = patch.order {
            preference.order = Some(order);
        }
        if let Some(width_weight) = patch.width_weight {
            preference.width_weight = Some(width_weight);
        }
    }
}

fn validate_preferences(preferences: &LiveboardPreferences) -> Result<()> {
    if !(MIN_VISIBLE_AGENTS..=MAX_VISIBLE_AGENTS).contains(&preferences.max_visible_agents) {
        bail!("stored Liveboard max_visible_agents is outside supported bounds");
    }
    validate_editor_command(&preferences.editor_command)?;
    for (agent_id, preference) in &preferences.agents {
        validate_agent_id(agent_id)?;
        if let Some(alias) = preference.alias.as_deref() {
            validate_alias(alias)?;
        }
        if preference.order.is_some_and(|order| order > MAX_ORDER) {
            bail!("stored Agent board order is outside supported bounds");
        }
        if let Some(weight) = preference.width_weight
            && (!weight.is_finite() || !(MIN_WIDTH_WEIGHT..=MAX_WIDTH_WEIGHT).contains(&weight))
        {
            bail!("stored Agent width weight is outside supported bounds");
        }
    }
    Ok(())
}

fn validate_agent_id(value: &str) -> Result<()> {
    if value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        Ok(())
    } else {
        bail!("Liveboard Agent preference key must match [a-z0-9]{{4}}")
    }
}

fn validate_alias(value: &str) -> Result<()> {
    if value.chars().count() > MAX_ALIAS_CHARS {
        bail!("Liveboard Agent alias must be at most {MAX_ALIAS_CHARS} characters");
    }
    if value.chars().any(|character| character.is_control()) {
        bail!("Liveboard Agent alias cannot contain control characters");
    }
    Ok(())
}

fn default_editor_command() -> String {
    "zed".to_string()
}

fn validate_editor_command(value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("Liveboard editor command must not be empty");
    }
    if value.chars().count() > MAX_EDITOR_COMMAND_CHARS {
        bail!("Liveboard editor command must be at most {MAX_EDITOR_COMMAND_CHARS} characters");
    }
    if value
        .chars()
        .any(|character| character == '\0' || character == '\n' || character == '\r')
    {
        bail!("Liveboard editor command must be one executable path or name");
    }
    Ok(())
}

struct LiveboardPreferenceLock {
    _file: File,
}

impl LiveboardPreferenceLock {
    fn acquire(paths: &LocalPaths) -> Result<Self> {
        let path = paths.liveboard_preferences_lock_file();
        let parent = path
            .parent()
            .context("Liveboard preference lock path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Liveboard state directory {}",
                parent.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
                format!("failed to set 0700 permissions on {}", parent.display())
            })?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).with_context(|| {
            format!(
                "failed to open Liveboard preference lock {}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set 0600 permissions on {}", path.display()))?;
        }
        file.lock_exclusive().with_context(|| {
            format!(
                "failed to acquire Liveboard preference lock {}",
                path.display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{
        LIVEBOARD_PREFERENCES_SCHEMA_VERSION, LiveboardAgentPreferencePatch,
        LiveboardPreferenceLock, LiveboardPreferencesPatch, LiveboardPreferencesStore,
        LiveboardTheme, load_preferences, merge_preferences, validate_preferences,
        write_user_only_json_atomic,
    };
    use crate::local::LocalPaths;

    fn test_store() -> (tempfile::TempDir, LocalPaths, LiveboardPreferencesStore) {
        let dir = tempdir().unwrap();
        let paths = LocalPaths::from_roots(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("state"),
        )
        .unwrap();
        let store = LiveboardPreferencesStore::new(&paths);
        (dir, paths, store)
    }

    #[test]
    fn defaults_and_partial_mutations_are_versioned_validated_and_user_only() {
        let (_dir, paths, store) = test_store();
        let defaults = store.load().unwrap();
        assert_eq!(
            defaults.schema_version,
            LIVEBOARD_PREFERENCES_SCHEMA_VERSION
        );
        assert_eq!(defaults.theme, LiveboardTheme::System);
        assert_eq!(defaults.max_visible_agents, 4);
        assert!(!defaults.command_outputs_expanded);
        assert!(defaults.diffs_expanded);
        assert!(!defaults.show_raw_button);
        assert_eq!(defaults.editor_command, "zed");

        let updated = store
            .mutate(&LiveboardPreferencesPatch {
                theme: Some(LiveboardTheme::Dark),
                max_visible_agents: Some(5),
                show_raw_button: Some(true),
                editor_command: Some("cursor".to_string()),
                agents: [(
                    "k7m2".to_string(),
                    LiveboardAgentPreferencePatch {
                        alias: Some("docs redesign".to_string()),
                        visible: Some(true),
                        order: Some(2),
                        width_weight: Some(1.25),
                    },
                )]
                .into_iter()
                .collect(),
                ..LiveboardPreferencesPatch::default()
            })
            .unwrap();
        assert_eq!(updated.theme, LiveboardTheme::Dark);
        assert_eq!(updated.max_visible_agents, 5);
        assert!(updated.show_raw_button);
        assert_eq!(updated.editor_command, "cursor");
        assert_eq!(
            updated.agents["k7m2"].alias.as_deref(),
            Some("docs redesign")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(paths.liveboard_preferences_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(paths.liveboard_preferences_lock_file())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(
            store
                .mutate(&LiveboardPreferencesPatch {
                    max_visible_agents: Some(0),
                    ..LiveboardPreferencesPatch::default()
                })
                .is_err()
        );
        assert!(
            store
                .mutate(&LiveboardPreferencesPatch {
                    editor_command: Some("  ".to_string()),
                    ..LiveboardPreferencesPatch::default()
                })
                .is_err()
        );
    }

    #[test]
    fn independent_stores_merge_concurrent_fields_under_the_preference_lock() {
        let (_dir, _paths, store) = test_store();
        let barrier = Arc::new(Barrier::new(3));
        let theme_store = store.clone();
        let theme_barrier = barrier.clone();
        let theme = std::thread::spawn(move || {
            theme_barrier.wait();
            theme_store
                .mutate(&LiveboardPreferencesPatch {
                    theme: Some(LiveboardTheme::Dark),
                    ..LiveboardPreferencesPatch::default()
                })
                .unwrap();
        });
        let max_store = store.clone();
        let max_barrier = barrier.clone();
        let maximum = std::thread::spawn(move || {
            max_barrier.wait();
            max_store
                .mutate(&LiveboardPreferencesPatch {
                    max_visible_agents: Some(6),
                    ..LiveboardPreferencesPatch::default()
                })
                .unwrap();
        });
        barrier.wait();
        theme.join().unwrap();
        maximum.join().unwrap();

        let merged = store.load().unwrap();
        assert_eq!(merged.theme, LiveboardTheme::Dark);
        assert_eq!(merged.max_visible_agents, 6);
    }

    #[test]
    fn cross_process_lock_serializes_read_merge_write_against_latest_document() {
        let (dir, paths, store) = test_store();
        let ready = dir.path().join("holder-ready");
        let helper = std::env::current_exe().unwrap();
        let helper_name = "local::liveboard::prefs::tests::cross_process_preference_helper";
        let roots = dir.path().to_string_lossy().into_owned();

        let mut holder = std::process::Command::new(&helper)
            .args([helper_name, "--exact", "--ignored", "--nocapture"])
            .env("ZODEX_LIVEBOARD_PREFS_TEST_ROOT", &roots)
            .env("ZODEX_LIVEBOARD_PREFS_TEST_ROLE", "hold_theme")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready.exists(),
            "cross-process preference holder never became ready"
        );

        let mut merger = std::process::Command::new(&helper)
            .args([helper_name, "--exact", "--ignored", "--nocapture"])
            .env("ZODEX_LIVEBOARD_PREFS_TEST_ROOT", &roots)
            .env("ZODEX_LIVEBOARD_PREFS_TEST_ROLE", "merge_max")
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            merger.try_wait().unwrap().is_none(),
            "independent preference writer bypassed the held cross-process lock"
        );

        assert!(holder.wait().unwrap().success());
        assert!(merger.wait().unwrap().success());
        let merged = store.load().unwrap();
        assert_eq!(merged.theme, LiveboardTheme::Dark);
        assert_eq!(merged.max_visible_agents, 6);
        assert!(
            std::fs::metadata(paths.liveboard_preferences_file())
                .unwrap()
                .len()
                > 0
        );
    }

    #[test]
    #[ignore = "spawned only by the cross-process preference test"]
    fn cross_process_preference_helper() {
        let root = std::path::PathBuf::from(
            std::env::var_os("ZODEX_LIVEBOARD_PREFS_TEST_ROOT").expect("preference helper root"),
        );
        let role =
            std::env::var("ZODEX_LIVEBOARD_PREFS_TEST_ROLE").expect("preference helper role");
        let paths =
            LocalPaths::from_roots(root.join("config"), root.join("data"), root.join("state"))
                .unwrap();
        let store = LiveboardPreferencesStore::new(&paths);
        match role.as_str() {
            "hold_theme" => {
                let patch = LiveboardPreferencesPatch {
                    theme: Some(LiveboardTheme::Dark),
                    ..LiveboardPreferencesPatch::default()
                };
                patch.validate().unwrap();
                let _lock = LiveboardPreferenceLock::acquire(&paths).unwrap();
                let mut current = load_preferences(&paths).unwrap();
                merge_preferences(&mut current, &patch);
                validate_preferences(&current).unwrap();
                std::fs::write(root.join("holder-ready"), b"ready").unwrap();
                std::thread::sleep(Duration::from_millis(250));
                write_user_only_json_atomic(&paths.liveboard_preferences_file(), &current).unwrap();
            }
            "merge_max" => {
                store
                    .mutate(&LiveboardPreferencesPatch {
                        max_visible_agents: Some(6),
                        ..LiveboardPreferencesPatch::default()
                    })
                    .unwrap();
            }
            other => panic!("unexpected preference helper role {other}"),
        }
    }

    #[test]
    fn newer_or_invalid_preference_documents_fail_clearly() {
        let (_dir, paths, store) = test_store();
        std::fs::create_dir_all(paths.liveboard_dir()).unwrap();
        std::fs::write(
            paths.liveboard_preferences_file(),
            r#"{"schema_version":99,"theme":"system","max_visible_agents":4,"command_outputs_expanded":false,"diffs_expanded":true,"agents":{}}"#,
        )
        .unwrap();
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("unsupported Liveboard preference schema version 99"));
    }
}
