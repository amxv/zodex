use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use tokio::sync::Mutex;

use crate::apply_patch;
use crate::config::Config;
use crate::invocation::InvocationContext;
use crate::protocol::{
    ApplyPatchInput, ApplyPatchOutput, ExecCommandInput, ToolOutput, WriteStdinInput,
};
use crate::session::{
    RuntimeShutdownResult, SessionCounts, SessionManager, SessionOrigin, SessionRuntimePolicy,
};

#[derive(Debug, Clone)]
pub enum ServiceRequest {
    ExecCommand {
        input: ExecCommandInput,
        origin: SessionOrigin,
    },
    WriteStdin {
        input: WriteStdinInput,
    },
    ApplyPatch {
        input: ApplyPatchInput,
    },
}

#[derive(Debug, Clone)]
pub enum ServiceResponse {
    ToolOutput(ToolOutput),
    ApplyPatchOutput(ApplyPatchOutput),
}

impl ServiceResponse {
    pub fn into_tool_output(self) -> Result<ToolOutput> {
        match self {
            Self::ToolOutput(output) => Ok(output),
            Self::ApplyPatchOutput(_) => Err(anyhow!(
                "internal service mismatch: expected tool output response"
            )),
        }
    }

    pub fn into_apply_patch_output(self) -> Result<ApplyPatchOutput> {
        match self {
            Self::ApplyPatchOutput(output) => Ok(output),
            Self::ToolOutput(_) => Err(anyhow!(
                "internal service mismatch: expected apply_patch response"
            )),
        }
    }
}

#[derive(Clone)]
pub struct ZodexService {
    config: Arc<Config>,
    sessions: Arc<SessionManager>,
    admission: Arc<ServiceAdmission>,
}

struct ServiceAdmission {
    closed: AtomicBool,
    barrier: Mutex<()>,
}

impl ServiceAdmission {
    fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            barrier: Mutex::new(()),
        }
    }

    async fn admit(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!(
                "service runtime is stopping; new tool calls are not accepted"
            ));
        }
        let guard = self.barrier.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!(
                "service runtime is stopping; new tool calls are not accepted"
            ));
        }
        drop(guard);
        Ok(())
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let guard = self.barrier.lock().await;
        drop(guard);
    }

    fn accepting(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
    }
}

impl ZodexService {
    pub fn new(config: Arc<Config>) -> Self {
        Self::with_session_policy(config, SessionRuntimePolicy::sprite())
    }

    pub fn with_session_policy(config: Arc<Config>, policy: SessionRuntimePolicy) -> Self {
        let sessions = Arc::new(SessionManager::with_policy(
            config.max_sessions,
            config.max_output_chars,
            policy,
        ));
        Self {
            config,
            sessions,
            admission: Arc::new(ServiceAdmission::new()),
        }
    }

    pub async fn execute(&self, request: ServiceRequest) -> Result<ServiceResponse> {
        self.execute_with_context(request, InvocationContext::default())
            .await
    }

    pub async fn execute_with_context(
        &self,
        request: ServiceRequest,
        invocation: InvocationContext,
    ) -> Result<ServiceResponse> {
        self.admission.admit().await?;
        match request {
            ServiceRequest::ExecCommand { input, origin } => self
                .sessions
                .exec_command_with_context(input, &self.config, origin, invocation)
                .await
                .map(ServiceResponse::ToolOutput),
            ServiceRequest::WriteStdin { input } => self
                .sessions
                .write_stdin_with_context(input, &self.config, invocation)
                .await
                .map(ServiceResponse::ToolOutput),
            ServiceRequest::ApplyPatch { input } => self
                .apply_patch(input)
                .await
                .map(|output| ServiceResponse::ApplyPatchOutput(ApplyPatchOutput { output })),
        }
    }

    pub async fn exec_command(&self, input: ExecCommandInput) -> Result<ToolOutput> {
        self.exec_command_with_origin(input, SessionOrigin::direct())
            .await
    }

    pub async fn write_stdin(&self, input: WriteStdinInput) -> Result<ToolOutput> {
        self.execute(ServiceRequest::WriteStdin { input })
            .await?
            .into_tool_output()
    }

    pub async fn exec_command_with_origin(
        &self,
        input: ExecCommandInput,
        origin: SessionOrigin,
    ) -> Result<ToolOutput> {
        self.execute(ServiceRequest::ExecCommand { input, origin })
            .await?
            .into_tool_output()
    }

    pub async fn apply_patch(&self, input: ApplyPatchInput) -> Result<String> {
        apply_patch::apply_patch(&input.patch, &input.workdir).await
    }

    pub fn accepting_new_sessions(&self) -> bool {
        self.admission.accepting() && self.sessions.accepting_new_sessions()
    }

    pub async fn session_counts(&self) -> Result<SessionCounts> {
        self.sessions.session_counts().await
    }

    pub async fn session_creator_context(
        &self,
        session_handle: &str,
    ) -> Option<crate::session::SessionCreatorContext> {
        self.sessions.session_creator_context(session_handle).await
    }

    pub async fn shutdown_sessions(&self) -> Result<RuntimeShutdownResult> {
        self.close_admission().await;
        self.sessions.shutdown_all().await
    }

    pub async fn close_admission(&self) {
        self.admission.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::config::Config;
    use crate::protocol::{ApplyPatchInput, CommandStatus, ExecCommandInput, WriteStdinInput};

    use super::{ServiceRequest, ZodexService};

    fn test_service() -> ZodexService {
        ZodexService::new(Arc::new(Config::default()))
    }

    fn test_workdir() -> String {
        std::env::current_dir()
            .expect("test current directory")
            .to_string_lossy()
            .to_string()
    }

    #[tokio::test]
    async fn exec_command_service_returns_finished_output() {
        let service = test_service();
        let output = service
            .exec_command(ExecCommandInput {
                cmd: "echo service-ok".to_string(),
                yield_time_ms: Some(2_000),
                workdir: test_workdir(),
                timeout_ms: None,
            })
            .await
            .expect("exec_command should succeed");

        assert_eq!(output.status, CommandStatus::Exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(output.session_id.is_none());
        assert!(output.output.contains("service-ok"));
    }

    #[tokio::test]
    async fn write_stdin_service_continues_existing_session() {
        let service = test_service();

        let started = service
            .exec_command(ExecCommandInput {
                cmd: "bash --noprofile --norc".to_string(),
                yield_time_ms: Some(50),
                workdir: test_workdir(),
                timeout_ms: Some(60_000),
            })
            .await
            .expect("stateful shell should start");
        let session_handle = started
            .session_handle
            .expect("expected running session handle");

        let echoed = service
            .write_stdin(WriteStdinInput {
                session_handle: session_handle.clone(),
                chars: Some("echo service-session\n".to_string()),
                yield_time_ms: Some(500),
                kill_process: Some(false),
            })
            .await
            .expect("write_stdin should succeed");

        assert_eq!(echoed.status, CommandStatus::Running);
        assert!(echoed.output.contains("service-session"));

        let exited = service
            .write_stdin(WriteStdinInput {
                session_handle,
                chars: Some("exit\n".to_string()),
                yield_time_ms: Some(2_000),
                kill_process: Some(false),
            })
            .await
            .expect("session should exit");

        assert_eq!(exited.status, CommandStatus::Exited);
        assert_eq!(exited.exit_code, Some(0));
        assert!(exited.session_handle.is_none());
    }

    #[tokio::test]
    async fn apply_patch_service_applies_relative_patch_path() {
        let service = test_service();
        let dir = tempdir().expect("tempdir");
        let patch = "*** Begin Patch\n*** Add File: created.txt\n+hello-service\n*** End Patch\n";

        let output = service
            .apply_patch(ApplyPatchInput {
                patch: patch.to_string(),
                workdir: dir.path().to_string_lossy().to_string(),
            })
            .await
            .expect("apply_patch should succeed");

        let created = dir.path().join("created.txt");
        assert!(output.contains(&format!("A {}", created.display())));
        assert_eq!(
            fs::read_to_string(created).expect("created file should be readable"),
            "hello-service\n"
        );
    }

    #[tokio::test]
    async fn execute_dispatches_apply_patch_through_shared_service_layer() {
        let service = test_service();
        let dir = tempdir().expect("tempdir");
        let patch =
            "*** Begin Patch\n*** Add File: dispatched.txt\n+hello-dispatch\n*** End Patch\n";

        let output = service
            .execute(ServiceRequest::ApplyPatch {
                input: ApplyPatchInput {
                    patch: patch.to_string(),
                    workdir: dir.path().to_string_lossy().to_string(),
                },
            })
            .await
            .expect("service dispatch should succeed")
            .into_apply_patch_output()
            .expect("apply_patch output expected");

        assert!(
            output
                .output
                .contains("Success. Updated the following files:")
        );
        assert!(output.output.contains(&format!(
            "A {}",
            dir.path().join("dispatched.txt").display()
        )));
        assert_eq!(
            fs::read_to_string(dir.path().join("dispatched.txt"))
                .expect("dispatched file should be readable"),
            "hello-dispatch\n"
        );
    }

    #[tokio::test]
    async fn closing_service_admission_rejects_every_mutating_request_before_side_effects() {
        let service = test_service();
        let dir = tempdir().expect("tempdir");
        service.close_admission().await;
        assert!(!service.accepting_new_sessions());

        let exec_error = service
            .execute(ServiceRequest::ExecCommand {
                input: ExecCommandInput {
                    cmd: "touch should-not-exist".to_string(),
                    yield_time_ms: Some(1_000),
                    workdir: dir.path().to_string_lossy().to_string(),
                    timeout_ms: None,
                },
                origin: crate::session::SessionOrigin::direct(),
            })
            .await
            .unwrap_err();
        assert!(
            exec_error
                .to_string()
                .contains("new tool calls are not accepted")
        );
        assert!(!dir.path().join("should-not-exist").exists());

        let patch_error = service
            .execute(ServiceRequest::ApplyPatch {
                input: ApplyPatchInput {
                    patch:
                        "*** Begin Patch\n*** Add File: should-not-exist.txt\n+no\n*** End Patch\n"
                            .to_string(),
                    workdir: dir.path().to_string_lossy().to_string(),
                },
            })
            .await
            .unwrap_err();
        assert!(
            patch_error
                .to_string()
                .contains("new tool calls are not accepted")
        );
        assert!(!dir.path().join("should-not-exist.txt").exists());

        let write_error = service
            .execute(ServiceRequest::WriteStdin {
                input: WriteStdinInput {
                    session_handle: "not-admitted".to_string(),
                    chars: Some("echo no\n".to_string()),
                    yield_time_ms: Some(100),
                    kill_process: Some(false),
                },
            })
            .await
            .unwrap_err();
        assert!(
            write_error
                .to_string()
                .contains("new tool calls are not accepted")
        );
    }
}
