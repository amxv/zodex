mod api;
mod git;
mod github;
mod server;
mod validation;

pub use api::{
    DirectPushRequest, DirectPushResponse, MintedInstallationToken, PublishPrRequest,
    PublishPrResponse,
};
pub use git::{build_publish_request, create_head_bundle, detect_repo_root, ensure_clean_worktree};
pub use github::{
    create_pull_request, mint_publisher_installation_token,
    mint_publisher_installation_token_with_metadata, mint_reader_installation_token,
    resolve_repo_installation_id,
};
pub use server::{serve_publisher, submit_direct_push_request, submit_publish_request};
pub use validation::{build_publish_branch_name, validate_publish_request};

pub(super) const GITHUB_API_BASE: &str = "https://api.github.com";
pub(super) const GITHUB_API_VERSION: &str = "2022-11-28";
pub(super) const SOCKET_DIR_MODE: u32 = 0o750;
pub(super) const SOCKET_MODE: u32 = 0o660;
pub(super) const ASKPASS_MODE: u32 = 0o700;
pub(super) const MAX_SOCKET_REQUEST_BYTES: usize = 48 * 1024 * 1024;
pub(super) const IMPORTED_REF: &str = "refs/heads/__zodex_imported";
pub(super) const ASKPASS_SCRIPT_NAME: &str = "git-askpass.sh";
pub(super) const DEFAULT_USER_AGENT: &str = "zodex-prd/0.1";
pub(super) const GITHUB_MODE_STATE_PATH: &str = "/var/lib/zodex/mode/state.json";
pub(super) const DIRECT_PUSH_IMPORTED_REF: &str = "refs/zodex/direct-push";

#[cfg(test)]
mod tests;
