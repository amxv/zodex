use std::sync::Arc;

use anyhow::{Result, anyhow};

use crate::apply_patch;
use crate::config::Config;
use crate::protocol::{
    ApplyPatchInput, ApplyPatchOutput, ExecCommandInput, ToolOutput, WriteStdinInput,
};
use crate::session::{SessionManager, SessionOrigin};

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
pub struct ComputerService {
    config: Arc<Config>,
    sessions: Arc<SessionManager>,
}

impl ComputerService {
    pub fn new(config: Arc<Config>) -> Self {
        let sessions = Arc::new(SessionManager::new(
            config.max_sessions,
            config.max_output_chars,
        ));
        Self { config, sessions }
    }

    pub async fn execute(&self, request: ServiceRequest) -> Result<ServiceResponse> {
        match request {
            ServiceRequest::ExecCommand { input, origin } => self
                .sessions
                .exec_command(input, &self.config, origin)
                .await
                .map(ServiceResponse::ToolOutput),
            ServiceRequest::WriteStdin { input } => self
                .sessions
                .write_stdin(input, &self.config)
                .await
                .map(ServiceResponse::ToolOutput),
            ServiceRequest::ApplyPatch { input } => self
                .apply_patch(input)
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

    pub fn apply_patch(&self, input: ApplyPatchInput) -> Result<String> {
        apply_patch::apply_patch(&input.patch, &input.workdir)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::config::Config;
    use crate::protocol::{ApplyPatchInput, CommandStatus, ExecCommandInput, WriteStdinInput};

    use super::{ComputerService, ServiceRequest};

    fn test_service() -> ComputerService {
        ComputerService::new(Arc::new(Config::default()))
    }

    #[tokio::test]
    async fn exec_command_service_returns_finished_output() {
        let service = test_service();
        let output = service
            .exec_command(ExecCommandInput {
                cmd: "echo service-ok".to_string(),
                yield_time_ms: Some(2_000),
                workdir: None,
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
                workdir: None,
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
}
