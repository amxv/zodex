use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tracing::warn;

use crate::invocation::{InvocationContext, McpResultContextProvider};

use super::history::normalize_declared_workdir;
use super::{LocalConfig, LocalHistoryRuntime};

const MAX_SKILL_DEPTH: usize = 6;
const MAX_SKILL_DIRS_PER_ROOT: usize = 2000;
const MAX_SKILL_FRONTMATTER_BYTES: usize = 64 * 1024;
const MAX_SKILL_NAME_CHARS: usize = 64;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 1024;
const ZSH_SHELL_GUIDANCE: &str = "Zsh: Never use lowercase `path` as a variable (it is tied to `PATH` and can make installed commands disappear); use `route` or `file_path` instead. Use arrays, not scalar command strings, to execute a command with arguments.";

pub(crate) struct LocalMcpContextProvider {
    config_path: PathBuf,
    history: Arc<LocalHistoryRuntime>,
    home: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    uses_zsh: bool,
}

impl LocalMcpContextProvider {
    pub(crate) fn new(
        config_path: PathBuf,
        environment: &[(OsString, OsString)],
        uses_zsh: bool,
        history: Arc<LocalHistoryRuntime>,
    ) -> Self {
        let home = environment_value(environment, "HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let codex_home = environment_value(environment, "CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".codex")));
        Self {
            config_path,
            history,
            home,
            codex_home,
            uses_zsh,
        }
    }

    fn global_context(&self, config: &LocalConfig) -> String {
        let mut sections = Vec::new();
        if self.uses_zsh {
            sections.push(ZSH_SHELL_GUIDANCE.to_string());
        }
        // The Zsh safety hint is intrinsic to the selected shell, not an
        // optional user AGENTS.md/skills context setting.
        if !config.context.enabled {
            return sections.join("\n\n");
        }
        if config.context.global_agents
            && let Some(section) = self.global_agents_section()
        {
            sections.push(section);
        }
        if config.context.skills.enabled {
            let skills = self.skill_catalog(config);
            if let Some(section) = render_skill_catalog("Global skills on this machine:", &skills) {
                sections.push(section);
            }
        }
        sections.join("\n\n")
    }

    fn global_agents_section(&self) -> Option<String> {
        let codex_home = self.codex_home.as_deref()?;
        for filename in ["AGENTS.override.md", "AGENTS.md"] {
            let path = codex_home.join(filename);
            if !path.is_file() {
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    if contents.trim().is_empty() {
                        return None;
                    }
                    return Some(format!("Global {filename}:\n{contents}"));
                }
                Err(error) => {
                    warn!(
                        event = "local_mcp_global_agents_read_failed",
                        path = %path.display(),
                        error = %error,
                    );
                    return None;
                }
            }
        }
        None
    }

    fn skill_catalog(&self, config: &LocalConfig) -> Vec<SkillCatalogEntry> {
        let mut roots = Vec::new();
        if config.context.skills.agents
            && let Some(home) = self.home.as_deref()
        {
            roots.push((home.join(".agents/skills"), true));
        }
        if config.context.skills.codex
            && let Some(codex_home) = self.codex_home.as_deref()
        {
            roots.push((codex_home.join("skills"), true));
            roots.push((codex_home.join("skills/.system"), false));
        }
        for configured in &config.context.skills.paths {
            let configured = expand_home(configured, self.home.as_deref());
            if configured.is_absolute() {
                roots.push((configured, true));
            } else {
                warn!(
                    event = "local_mcp_skill_root_not_absolute",
                    path = %configured.display(),
                );
            }
        }

        scan_skill_roots(roots)
    }

    fn repo_skill_catalog(&self, workdir: &Path) -> Vec<SkillCatalogEntry> {
        let mut entries = scan_skill_roots([(workdir.join(".agents/skills"), true)]);
        let mut seen_metadata = HashSet::new();
        entries
            .retain(|entry| seen_metadata.insert((entry.name.clone(), entry.description.clone())));
        entries
    }
}

impl McpResultContextProvider for LocalMcpContextProvider {
    fn appended_context(
        &self,
        context: &InvocationContext,
        workdir: Option<&str>,
        tool_succeeded: bool,
    ) -> Result<Option<String>> {
        let Some(provider) = context.provider.as_ref() else {
            return Ok(None);
        };
        if !context.global_context_pending
            && !(tool_succeeded
                && (context.repo_agents_context_pending || context.repo_skills_context_pending)
                && workdir.is_some())
        {
            return Ok(None);
        }
        let config = LocalConfig::load(&self.config_path)?;
        let mut sections = Vec::new();
        if context.global_context_pending
            && (self.uses_zsh
                || (config.context.enabled
                    && (config.context.global_agents || config.context.skills.enabled)))
        {
            let global = self.global_context(&config);
            if !global.is_empty() && self.history.claim_global_context_injection(provider)? {
                sections.push(global);
            }
        }

        if !config.context.enabled {
            return Ok((!sections.is_empty()).then(|| sections.join("\n\n")));
        }

        if tool_succeeded
            && ((context.repo_agents_context_pending && config.context.repo_agents)
                || (context.repo_skills_context_pending && config.context.repo_skills))
            && let Some(workdir) = workdir
            && let Some(normalized_workdir) = normalize_declared_workdir(workdir)
        {
            let workdir_path = Path::new(workdir);

            if context.repo_agents_context_pending
                && config.context.repo_agents
                && self
                    .history
                    .claim_repo_agents_check(provider, &normalized_workdir)?
                && let Some(filename) = repo_instruction_filename(workdir_path)
            {
                sections.push(format!("{workdir} contains an {filename}."));
            }

            if context.repo_skills_context_pending && config.context.repo_skills {
                let repo_skills = self.repo_skill_catalog(workdir_path);
                if self
                    .history
                    .claim_repo_skills_check(provider, &normalized_workdir)?
                    && let Some(section) = render_skill_catalog(
                        &format!("Repo-local skills in {workdir}:"),
                        &repo_skills,
                    )
                {
                    sections.push(section);
                }
            }
        }

        Ok((!sections.is_empty()).then(|| sections.join("\n\n")))
    }
}

#[derive(Debug)]
struct SkillCatalogEntry {
    name: String,
    description: String,
    path: PathBuf,
}

fn scan_skill_roots(roots: impl IntoIterator<Item = (PathBuf, bool)>) -> Vec<SkillCatalogEntry> {
    let mut entries = Vec::new();
    let mut seen_skills = HashSet::new();
    let mut seen_dirs = HashSet::new();
    for (root, follow_symlinks) in roots {
        let mut scanned_dirs = 0usize;
        scan_skill_root(
            &root,
            0,
            follow_symlinks,
            &mut scanned_dirs,
            &mut seen_dirs,
            &mut seen_skills,
            &mut entries,
        );
    }
    entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries
}

fn render_skill_catalog(heading: &str, skills: &[SkillCatalogEntry]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut section = String::from(heading);
    section.push('\n');
    for skill in skills {
        section.push_str("- ");
        section.push_str(&skill.name);
        section.push_str(" — ");
        section.push_str(&skill.description);
        section.push_str(" — ");
        section.push_str(&skill.path.display().to_string());
        section.push('\n');
    }
    section.pop();
    Some(section)
}

fn scan_skill_root(
    dir: &Path,
    depth: usize,
    follow_symlinks: bool,
    scanned_dirs: &mut usize,
    seen_dirs: &mut HashSet<PathBuf>,
    seen_skills: &mut HashSet<PathBuf>,
    entries: &mut Vec<SkillCatalogEntry>,
) {
    if depth > MAX_SKILL_DEPTH || *scanned_dirs >= MAX_SKILL_DIRS_PER_ROOT {
        return;
    }
    let Ok(metadata) = fs::metadata(dir) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let canonical_dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !seen_dirs.insert(canonical_dir) {
        return;
    }
    *scanned_dirs += 1;

    let skill_md = dir.join("SKILL.md");
    let skill_md_is_file = fs::symlink_metadata(&skill_md)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file());
    if skill_md_is_file {
        let canonical_skill = fs::canonicalize(&skill_md).unwrap_or_else(|_| skill_md.clone());
        if seen_skills.insert(canonical_skill)
            && let Some((name, description)) = parse_skill_frontmatter(&skill_md)
        {
            entries.push(SkillCatalogEntry {
                name,
                description,
                path: skill_md,
            });
        }
    }

    let children = match fs::read_dir(dir) {
        Ok(children) => children,
        Err(error) => {
            warn!(
                event = "local_mcp_skill_root_read_failed",
                path = %dir.display(),
                error = %error,
            );
            return;
        }
    };
    for child in children.flatten() {
        let child_path = child.path();
        let name = child.file_name();
        if is_hidden_name(&name) {
            continue;
        }
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        if file_type.is_symlink() && !follow_symlinks {
            continue;
        }
        let is_directory = if file_type.is_symlink() {
            fs::metadata(&child_path).is_ok_and(|metadata| metadata.is_dir())
        } else {
            file_type.is_dir()
        };
        if is_directory {
            scan_skill_root(
                &child_path,
                depth + 1,
                follow_symlinks,
                scanned_dirs,
                seen_dirs,
                seen_skills,
                entries,
            );
        }
    }
}

fn parse_skill_frontmatter(path: &Path) -> Option<(String, String)> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            warn!(
                event = "local_mcp_skill_read_failed",
                path = %path.display(),
                error = %error,
            );
            return None;
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return None,
        Ok(_) if line.trim() == "---" => {}
        Ok(_) => return None,
        Err(error) => {
            warn!(
                event = "local_mcp_skill_read_failed",
                path = %path.display(),
                error = %error,
            );
            return None;
        }
    }
    let mut frontmatter = String::new();
    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(read) => read,
            Err(error) => {
                warn!(
                    event = "local_mcp_skill_read_failed",
                    path = %path.display(),
                    error = %error,
                );
                return None;
            }
        };
        if read == 0 {
            return None;
        }
        if line.trim() == "---" {
            break;
        }
        if frontmatter.len().saturating_add(line.len()) > MAX_SKILL_FRONTMATTER_BYTES {
            warn!(
                event = "local_mcp_skill_frontmatter_too_large",
                path = %path.display(),
            );
            return None;
        }
        frontmatter.push_str(&line);
    }
    let lines = frontmatter.lines().collect::<Vec<_>>();
    let frontmatter = lines.as_slice();
    let name = yaml_frontmatter_scalar(frontmatter, "name").unwrap_or_else(|| {
        path.parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string())
    });
    let description = yaml_frontmatter_scalar(frontmatter, "description")?;
    let name = normalize_inline_text(&name);
    let description = normalize_inline_text(&description);
    if name.is_empty()
        || description.is_empty()
        || name.chars().count() > MAX_SKILL_NAME_CHARS
        || description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS
    {
        return None;
    }
    Some((name, description))
}

fn yaml_frontmatter_scalar(lines: &[&str], key: &str) -> Option<String> {
    for (index, line) in lines.iter().enumerate() {
        if line.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let Some((candidate, raw_value)) = line.split_once(':') else {
            continue;
        };
        if candidate.trim() != key {
            continue;
        }
        let raw_value = raw_value.trim();
        if raw_value.starts_with('|') || raw_value.starts_with('>') {
            let folded = raw_value.starts_with('>');
            let mut values = Vec::new();
            for continuation in lines.iter().skip(index + 1) {
                if continuation.is_empty() {
                    values.push(String::new());
                    continue;
                }
                if !continuation.chars().next().is_some_and(char::is_whitespace) {
                    break;
                }
                values.push(continuation.trim().to_string());
            }
            return Some(if folded {
                values.join(" ")
            } else {
                values.join("\n")
            });
        }
        return Some(unquote_yaml_scalar(raw_value));
    }
    None
}

fn unquote_yaml_scalar(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str::<String>(value)
            .unwrap_or_else(|_| value[1..value.len() - 1].to_string());
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_string()
}

fn normalize_inline_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn repo_instruction_filename(workdir: &Path) -> Option<&'static str> {
    if workdir.join("AGENTS.override.md").is_file() {
        Some("AGENTS.override.md")
    } else if workdir.join("AGENTS.md").is_file() {
        Some("AGENTS.md")
    } else {
        None
    }
}

fn environment_value<'a>(environment: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
    environment.iter().rev().find_map(|(candidate, value)| {
        (candidate.as_os_str() == OsStr::new(key)).then_some(value.as_os_str())
    })
}

fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    if path == Path::new("~") {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Ok(rest) = path.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::invocation::{
        InvocationContext, InvocationEvidenceRecorder, InvocationOutcome, InvocationStart,
        McpResultContextProvider, ProviderCallMetadata,
    };
    use crate::local::{LocalConfig, LocalHistoryRuntime, LocalHistoryRuntimeConfig};

    use super::{
        LocalMcpContextProvider, ZSH_SHELL_GUIDANCE, parse_skill_frontmatter,
        repo_instruction_filename,
    };

    #[test]
    fn parses_plain_quoted_and_folded_skill_frontmatter() {
        let dir = tempdir().unwrap();
        let plain = dir.path().join("plain.md");
        fs::write(
            &plain,
            "---\nname: plain\ndescription: A plain description.\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(
            parse_skill_frontmatter(&plain),
            Some(("plain".to_string(), "A plain description.".to_string()))
        );

        let quoted = dir.path().join("quoted.md");
        fs::write(
            &quoted,
            "---\nname: \"quoted\"\ndescription: >-\n  A folded\n  description.\n---\n",
        )
        .unwrap();
        assert_eq!(
            parse_skill_frontmatter(&quoted),
            Some(("quoted".to_string(), "A folded description.".to_string()))
        );

        let fallback_dir = dir.path().join("fallback-name");
        fs::create_dir_all(&fallback_dir).unwrap();
        let fallback = fallback_dir.join("SKILL.md");
        fs::write(
            &fallback,
            "---\ndescription: Uses the directory name.\n---\n",
        )
        .unwrap();
        assert_eq!(
            parse_skill_frontmatter(&fallback),
            Some((
                "fallback-name".to_string(),
                "Uses the directory name.".to_string()
            ))
        );
    }

    #[test]
    fn repo_instruction_prefers_override() {
        let dir = tempdir().unwrap();
        assert_eq!(repo_instruction_filename(dir.path()), None);
        fs::write(dir.path().join("AGENTS.md"), "base").unwrap();
        assert_eq!(repo_instruction_filename(dir.path()), Some("AGENTS.md"));
        fs::write(dir.path().join("AGENTS.override.md"), "override").unwrap();
        assert_eq!(
            repo_instruction_filename(dir.path()),
            Some("AGENTS.override.md")
        );
    }

    #[test]
    fn global_context_is_once_per_conversation_and_repo_hint_waits_for_success() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        let codex_home = dir.path().join("codex");
        let repo = dir.path().join("repo");
        let skill_root = dir.path().join("team-skills");
        fs::create_dir_all(codex_home.join("skills/.system/system-skill")).unwrap();
        fs::create_dir_all(skill_root.join("demo-skill")).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(
            codex_home.join("AGENTS.md"),
            "base global instructions must lose to override",
        )
        .unwrap();
        fs::write(
            codex_home.join("AGENTS.override.md"),
            "global override instructions",
        )
        .unwrap();
        fs::write(
            codex_home.join("skills/.system/system-skill/SKILL.md"),
            "---\nname: system-skill\ndescription: System skill.\n---\n",
        )
        .unwrap();
        fs::write(
            skill_root.join("demo-skill/SKILL.md"),
            "---\nname: demo-skill\ndescription: Demo skill.\n---\n",
        )
        .unwrap();
        fs::write(repo.join("AGENTS.md"), "repo instructions").unwrap();

        let config_path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        config.context.skills.agents = false;
        config.context.skills.paths = vec![skill_root];
        config.save(&config_path).unwrap();

        let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            dir.path().join("history.sqlite3"),
            "context-test-runtime",
            365 * 24 * 60 * 60,
            1024 * 1024 * 1024,
        ))
        .unwrap();
        let invocation = InvocationEvidenceRecorder::begin(
            history.as_ref(),
            InvocationContext::default().with_provider(ProviderCallMetadata::new(
                "openai/session",
                "conversation-a",
            )),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"false","workdir":repo.display().to_string()}),
            ),
        )
        .unwrap();
        let environment = vec![
            (OsString::from("HOME"), home.into_os_string()),
            (
                OsString::from("CODEX_HOME"),
                codex_home.clone().into_os_string(),
            ),
        ];
        let provider =
            LocalMcpContextProvider::new(config_path, &environment, true, history.clone());

        let failed = provider
            .appended_context(&invocation, repo.to_str(), false)
            .unwrap()
            .unwrap();
        assert!(failed.contains("Global AGENTS.override.md:\nglobal override instructions"));
        assert!(!failed.contains("base global instructions"));
        assert!(failed.contains("Global skills on this machine:"));
        assert!(failed.contains(ZSH_SHELL_GUIDANCE));
        assert!(failed.contains("demo-skill — Demo skill."));
        assert!(failed.contains("system-skill — System skill."));
        assert!(!failed.contains("contains an AGENTS.md"));

        let successful = provider
            .appended_context(&invocation, repo.to_str(), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            successful,
            format!("{} contains an AGENTS.md.", repo.display())
        );
        assert!(
            provider
                .appended_context(&invocation, repo.to_str(), true)
                .unwrap()
                .is_none()
        );

        history.shutdown_blocking().unwrap();
    }

    #[test]
    fn repo_hint_is_once_per_agent_workdir_and_prefers_override() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("AGENTS.md"), "base").unwrap();
        fs::write(repo.join("AGENTS.override.md"), "override").unwrap();
        let config_path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        config.context.global_agents = false;
        config.context.skills.enabled = false;
        config.save(&config_path).unwrap();
        let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            dir.path().join("history.sqlite3"),
            "context-test-runtime",
            365 * 24 * 60 * 60,
            1024 * 1024 * 1024,
        ))
        .unwrap();

        let make_invocation = |session: &'static str| {
            InvocationEvidenceRecorder::begin(
                history.as_ref(),
                InvocationContext::default()
                    .with_provider(ProviderCallMetadata::new("openai/session", session)),
                InvocationStart::new(
                    "apply_patch",
                    json!({"patch":"noop","workdir":repo.display().to_string()}),
                ),
            )
            .unwrap()
        };
        let first = make_invocation("conversation-a");
        let second = make_invocation("conversation-b");
        let provider = LocalMcpContextProvider::new(config_path, &[], false, history.clone());

        let first_hint = provider
            .appended_context(&first, repo.to_str(), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            first_hint,
            format!("{} contains an AGENTS.override.md.", repo.display())
        );
        assert!(
            provider
                .appended_context(&first, repo.to_str(), true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            provider
                .appended_context(&second, repo.to_str(), true)
                .unwrap()
                .unwrap(),
            format!("{} contains an AGENTS.override.md.", repo.display())
        );

        history.shutdown_blocking().unwrap();
    }

    #[test]
    fn repo_skills_are_exact_workdir_deduplicated_and_independently_toggleable() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let child = repo.join("child");
        let skills = repo.join(".agents/skills");
        fs::create_dir_all(skills.join("demo")).unwrap();
        fs::create_dir_all(skills.join("duplicate")).unwrap();
        fs::create_dir_all(skills.join("variant")).unwrap();
        fs::create_dir_all(&child).unwrap();
        let skill = "---\nname: demo\ndescription: Demo skill.\n---\n";
        fs::write(skills.join("demo/SKILL.md"), skill).unwrap();
        fs::write(skills.join("duplicate/SKILL.md"), skill).unwrap();
        fs::write(
            skills.join("variant/SKILL.md"),
            "---\nname: demo\ndescription: Different description.\n---\n",
        )
        .unwrap();

        let config_path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        config.context.global_agents = false;
        config.context.repo_agents = false;
        config.context.repo_skills = false;
        config.context.skills.enabled = false;
        config.save(&config_path).unwrap();
        let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            dir.path().join("history.sqlite3"),
            "context-test-runtime",
            365 * 24 * 60 * 60,
            1024 * 1024 * 1024,
        ))
        .unwrap();
        let invocation_for = |workdir: &std::path::Path| {
            InvocationEvidenceRecorder::begin(
                history.as_ref(),
                InvocationContext::default().with_provider(ProviderCallMetadata::new(
                    "openai/session",
                    "conversation-repo-skills",
                )),
                InvocationStart::new(
                    "exec_command",
                    json!({"cmd":"true","workdir":workdir.display().to_string()}),
                ),
            )
            .unwrap()
        };
        let repo_invocation = invocation_for(&repo);
        let provider =
            LocalMcpContextProvider::new(config_path.clone(), &[], false, history.clone());

        assert!(
            provider
                .appended_context(&repo_invocation, repo.to_str(), true)
                .unwrap()
                .is_none(),
            "disabled repo skills must not inject or consume the workdir skill check"
        );

        config.context.repo_skills = true;
        config.save(&config_path).unwrap();
        assert!(
            provider
                .appended_context(&repo_invocation, repo.to_str(), false)
                .unwrap()
                .is_none(),
            "failed tool invocations must not consume repo-local skill context"
        );
        let context = provider
            .appended_context(&repo_invocation, repo.to_str(), true)
            .unwrap()
            .unwrap();
        assert!(context.contains(&format!("Repo-local skills in {}:", repo.display())));
        assert_eq!(context.matches("demo — Demo skill.").count(), 1);
        assert_eq!(context.matches("demo — Different description.").count(), 1);
        assert!(context.contains("/.agents/skills/demo/SKILL.md"));
        assert!(
            provider
                .appended_context(&repo_invocation, repo.to_str(), true)
                .unwrap()
                .is_none(),
            "repo skills must be injected at most once per conversation and normalized workdir"
        );
        let repo_again = invocation_for(&repo);
        assert!(!repo_again.repo_skills_context_pending);

        let child_invocation = invocation_for(&child);
        assert!(
            provider
                .appended_context(&child_invocation, child.to_str(), true)
                .unwrap()
                .is_none(),
            "repo skill discovery must not walk up from the exact requested workdir"
        );

        history.shutdown_blocking().unwrap();
    }

    #[test]
    fn delivered_context_survives_local_runtime_restart() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(codex_home.join("AGENTS.md"), "global instructions").unwrap();
        fs::write(repo.join("AGENTS.md"), "repo instructions").unwrap();
        let config_path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        config.context.skills.enabled = false;
        config.save(&config_path).unwrap();
        let database = dir.path().join("history.sqlite3");
        let environment = vec![(OsString::from("CODEX_HOME"), codex_home.into_os_string())];

        let first_runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            database.clone(),
            "runtime-a",
            365 * 24 * 60 * 60,
            1024 * 1024 * 1024,
        ))
        .unwrap();
        let first_invocation = InvocationEvidenceRecorder::begin(
            first_runtime.as_ref(),
            InvocationContext::default().with_provider(ProviderCallMetadata::new(
                "openai/session",
                "conversation-restart",
            )),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"true","workdir":repo.display().to_string()}),
            ),
        )
        .unwrap();
        let first_provider = LocalMcpContextProvider::new(
            config_path.clone(),
            &environment,
            true,
            first_runtime.clone(),
        );
        let first_context = first_provider
            .appended_context(&first_invocation, repo.to_str(), true)
            .unwrap()
            .unwrap();
        assert!(first_context.contains("Global AGENTS.md:\nglobal instructions"));
        assert_eq!(first_context.matches(ZSH_SHELL_GUIDANCE).count(), 1);
        assert!(first_context.contains("contains an AGENTS.md."));
        InvocationEvidenceRecorder::complete(
            first_runtime.as_ref(),
            &first_invocation,
            InvocationOutcome::Success(json!({"status":"exited","exit_code":0})),
        )
        .unwrap();
        first_runtime.flush_for_test().unwrap();
        first_runtime.shutdown_blocking().unwrap();
        drop(first_provider);
        drop(first_runtime);

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE invocations SET started_at_ms = 1, completed_at_ms = 2",
                [],
            )
            .unwrap();
        drop(connection);

        let second_runtime = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            database,
            "runtime-b",
            1,
            1024 * 1024 * 1024,
        ))
        .unwrap();
        let second_invocation = InvocationEvidenceRecorder::begin(
            second_runtime.as_ref(),
            InvocationContext::default().with_provider(ProviderCallMetadata::new(
                "openai/session",
                "conversation-restart",
            )),
            InvocationStart::new(
                "exec_command",
                json!({"cmd":"true","workdir":repo.display().to_string()}),
            ),
        )
        .unwrap();
        assert!(!second_invocation.global_context_pending);
        assert!(!second_invocation.repo_agents_context_pending);
        assert!(!second_invocation.repo_skills_context_pending);
        let second_provider =
            LocalMcpContextProvider::new(config_path, &environment, true, second_runtime.clone());
        assert!(
            second_provider
                .appended_context(&second_invocation, repo.to_str(), true)
                .unwrap()
                .is_none()
        );
        second_runtime.shutdown_blocking().unwrap();
    }

    #[test]
    fn zsh_shell_guidance_is_once_per_conversation_even_when_context_is_disabled() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("local.toml");
        let mut config = LocalConfig::default();
        config.context.enabled = false;
        config.save(&config_path).unwrap();
        let history = LocalHistoryRuntime::open(LocalHistoryRuntimeConfig::new(
            dir.path().join("history.sqlite3"),
            "zsh-guidance-runtime",
            365 * 24 * 60 * 60,
            1024 * 1024 * 1024,
        ))
        .unwrap();
        let invocation_for = |session: &'static str| {
            InvocationEvidenceRecorder::begin(
                history.as_ref(),
                InvocationContext::default()
                    .with_provider(ProviderCallMetadata::new("openai/session", session)),
                InvocationStart::new("exec_command", json!({"cmd":"true","workdir":"/tmp"})),
            )
            .unwrap()
        };

        let zsh_provider =
            LocalMcpContextProvider::new(config_path.clone(), &[], true, history.clone());
        let first = invocation_for("zsh-session");
        assert_eq!(
            zsh_provider
                .appended_context(&first, Some("/tmp"), true)
                .unwrap(),
            Some(ZSH_SHELL_GUIDANCE.to_string())
        );
        assert!(
            zsh_provider
                .appended_context(&first, Some("/tmp"), true)
                .unwrap()
                .is_none()
        );
        assert!(!invocation_for("zsh-session").global_context_pending);

        // Neither disabling AGENTS.md/skills nor the top-level context setting
        // can suppress the shell-specific guidance for a new session.
        config.context.enabled = true;
        config.context.global_agents = false;
        config.context.skills.enabled = false;
        config.save(&config_path).unwrap();
        let second = invocation_for("second-zsh-session");
        assert_eq!(
            zsh_provider
                .appended_context(&second, Some("/tmp"), true)
                .unwrap(),
            Some(ZSH_SHELL_GUIDANCE.to_string())
        );

        let non_zsh_provider =
            LocalMcpContextProvider::new(config_path, &[], false, history.clone());
        let sh_session = invocation_for("sh-session");
        assert!(
            non_zsh_provider
                .appended_context(&sh_session, Some("/tmp"), true)
                .unwrap()
                .is_none()
        );
        history.shutdown_blocking().unwrap();
    }
}
