use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishPrRequest {
    pub repo_id: String,
    #[serde(default)]
    pub base: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
    pub bundle_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectPushRequest {
    pub repo: String,
    pub src: String,
    pub dst: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub bundle_base64: Option<String>,
    #[serde(default)]
    pub src_oid: Option<String>,
    #[serde(default)]
    pub src_object_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectPushResponse {
    pub repo: String,
    pub dst: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishPrResponse {
    pub pr_url: String,
    pub branch: String,
    pub pull_number: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PublishPrError {
    pub(super) error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PublisherRequest {
    PublishPr(PublishPrRequest),
    DirectPush(DirectPushRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum PublisherResponse {
    PublishPr(PublishPrResponse),
    DirectPush(DirectPushResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct GithubYoloRepoGrant {
    pub(super) repo: String,
    #[serde(default)]
    pub(super) expires_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct GithubModeRecord {
    pub(super) mode: String,
    #[serde(default)]
    pub(super) all_installed: bool,
    #[serde(default)]
    pub(super) repos: Vec<String>,
    #[serde(default)]
    pub(super) repo_grants: Vec<GithubYoloRepoGrant>,
    #[serde(default)]
    pub(super) expires_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(super) struct GithubAppClaims {
    pub(super) iat: u64,
    pub(super) exp: u64,
    pub(super) iss: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct InstallationTokenResponse {
    pub(super) token: String,
    #[serde(default)]
    pub(super) expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedInstallationToken {
    pub token: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct CreatePullRequestResponse {
    pub(super) html_url: String,
    pub(super) number: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct CreatePullRequestPayload<'a> {
    pub(super) title: &'a str,
    pub(super) body: &'a str,
    pub(super) head: &'a str,
    pub(super) base: &'a str,
    pub(super) draft: bool,
}
