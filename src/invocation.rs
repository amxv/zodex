use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationContext {
    pub invocation_id: Option<i64>,
    pub correlation_id: Option<Arc<str>>,
    pub provider: Option<ProviderCallMetadata>,
    pub agent_id: Option<Arc<str>>,
    pub global_context_pending: bool,
    pub repo_agents_context_pending: bool,
    pub repo_skills_context_pending: bool,
}

impl InvocationContext {
    pub fn with_invocation_id(mut self, value: i64) -> Self {
        self.invocation_id = Some(value);
        self
    }

    pub fn with_correlation_id(mut self, value: impl Into<Arc<str>>) -> Self {
        self.correlation_id = Some(value.into());
        self
    }

    pub fn with_provider(mut self, provider: ProviderCallMetadata) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_agent_id(mut self, value: impl Into<Arc<str>>) -> Self {
        self.agent_id = Some(value.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCallMetadata {
    pub kind: Arc<str>,
    pub session_key: Arc<str>,
}

impl ProviderCallMetadata {
    pub fn new(kind: impl Into<Arc<str>>, session_key: impl Into<Arc<str>>) -> Self {
        Self {
            kind: kind.into(),
            session_key: session_key.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvocationStart {
    pub tool_name: Arc<str>,
    pub arguments: Value,
    pub target_created_by_agent_id: Option<Arc<str>>,
    pub target_created_by_invocation_id: Option<i64>,
    pub continuation_kind: Option<InvocationContinuationKind>,
}

impl InvocationStart {
    pub fn new(tool_name: impl Into<Arc<str>>, arguments: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            arguments,
            target_created_by_agent_id: None,
            target_created_by_invocation_id: None,
            continuation_kind: None,
        }
    }

    pub fn with_target_created_by_agent_id(mut self, value: Option<Arc<str>>) -> Self {
        self.target_created_by_agent_id = value;
        self
    }

    pub fn with_target_created_by_invocation_id(mut self, value: Option<i64>) -> Self {
        self.target_created_by_invocation_id = value;
        self
    }

    pub fn with_continuation_kind(mut self, value: InvocationContinuationKind) -> Self {
        self.continuation_kind = Some(value);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationContinuationKind {
    Poll,
    Stdin,
    Kill,
}

impl InvocationContinuationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Stdin => "stdin",
            Self::Kill => "kill",
        }
    }
}

#[derive(Debug, Clone)]
pub enum InvocationOutcome {
    Success(Value),
    Error(String),
}

pub trait InvocationEvidenceRecorder: Send + Sync {
    /// Reserve an invocation context and enqueue evidence without waiting for
    /// history storage. The context must flow through service/session work.
    fn begin(
        &self,
        context: InvocationContext,
        start: InvocationStart,
    ) -> Result<InvocationContext>;

    /// Persist the exact logical handler result/error after it has been
    /// produced. Failure here must not rewrite the result returned to MCP.
    fn complete(&self, context: &InvocationContext, outcome: InvocationOutcome) -> Result<()>;

    /// Queue actual patch bytes already obtained by mutation, without extra IO.
    fn record_patch_evidence(
        &self,
        _context: &InvocationContext,
        _evidence: crate::apply_patch::PatchEvidence,
    ) {
    }

    /// A truthful warning about incomplete history, independent of tool success.
    fn warning(&self) -> Option<String> {
        None
    }
}

pub trait McpResultContextProvider: Send + Sync {
    fn appended_context(
        &self,
        context: &InvocationContext,
        workdir: Option<&str>,
        tool_succeeded: bool,
    ) -> Result<Option<String>>;
}
