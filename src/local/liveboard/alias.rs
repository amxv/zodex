use std::sync::Arc;

use anyhow::{Context, Result};

use crate::invocation::InvocationContext;
use crate::local::LocalHistoryRuntime;

use super::prefs::{
    LiveboardAgentPreferencePatch, LiveboardPreferencesPatch, LiveboardPreferencesStore,
    validate_alias,
};

/// Local-only control-plane operation: no shell work and no timeline entry.
#[derive(Clone)]
pub(crate) struct LocalAgentAliasService {
    history: Arc<LocalHistoryRuntime>,
    preferences: LiveboardPreferencesStore,
}

pub(crate) struct LocalAliasUpdate {
    pub(crate) context: InvocationContext,
    pub(crate) agent_id: String,
    pub(crate) alias: Option<String>,
}

impl LocalAgentAliasService {
    pub(crate) fn new(
        history: Arc<LocalHistoryRuntime>,
        preferences: LiveboardPreferencesStore,
    ) -> Self {
        Self {
            history,
            preferences,
        }
    }

    pub(crate) fn set_alias(
        &self,
        context: InvocationContext,
        alias: String,
    ) -> Result<LocalAliasUpdate> {
        validate_alias(&alias)?;
        // The session identity is taken exclusively from trusted MCP request
        // metadata, never from a model-supplied Agent ID.
        let context = self.history.identify_agent(context)?;
        let agent_id = context
            .agent_id
            .as_ref()
            .context("Local Agent identity is unavailable")?
            .to_string();
        let patch = LiveboardPreferencesPatch {
            agents: [(
                agent_id.clone(),
                LiveboardAgentPreferencePatch {
                    alias: Some(alias),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let saved = self.preferences.mutate(&patch)?;
        let alias = saved
            .agents
            .get(&agent_id)
            .and_then(|pref| pref.alias.clone());
        Ok(LocalAliasUpdate {
            context,
            agent_id,
            alias,
        })
    }
}
