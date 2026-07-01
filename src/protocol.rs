use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ExecCommandInput {
    pub cmd: String,
    pub yield_time_ms: Option<u64>,
    pub workdir: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WriteStdinInput {
    pub session_handle: String,
    pub chars: Option<String>,
    pub yield_time_ms: Option<u64>,
    pub kill_process: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ApplyPatchInput {
    pub patch: String,
    pub workdir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ApplyPatchOutput {
    pub output: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommandStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TerminationReason {
    Exit,
    Timeout,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolOutput {
    pub summary: String,
    pub output: String,
    pub status: CommandStatus,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<TerminationReason>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApplyPatchInput, WriteStdinInput};

    #[test]
    fn apply_patch_requires_workdir() {
        let err = serde_json::from_value::<ApplyPatchInput>(json!({
            "patch": "*** Begin Patch\n*** End Patch\n"
        }))
        .expect_err("workdir should be required");

        assert!(
            err.to_string().contains("workdir"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn write_stdin_requires_session_handle() {
        let err = serde_json::from_value::<WriteStdinInput>(json!({
            "chars": "echo hi\n"
        }))
        .expect_err("session_handle should be required");

        assert!(
            err.to_string().contains("session_handle"),
            "unexpected error: {err}"
        );
    }
}
