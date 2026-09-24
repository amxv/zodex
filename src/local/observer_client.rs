use std::fs;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, Url};

use super::observability::{
    ApiAgent, ApiAgentList, ApiInvocationDetail, ApiInvocationList, ApiStatusDocument,
    ApiTimelineDetail,
};
use super::{
    LOCAL_OBSERVABILITY_API_VERSION, LocalPaths, LocalRuntimeDiscovery,
    PRESENTATION_SCHEMA_VERSION, load_runtime_discovery,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_BEARER_LEN: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct ObserverBootstrap {
    pub(crate) discovery: LocalRuntimeDiscovery,
    pub(crate) status: ApiStatusDocument,
    pub(crate) agents: Vec<ApiAgent>,
}

#[derive(Debug, Clone)]
pub(crate) struct ObserverEventSelection {
    pub(crate) include_output: bool,
    pub(crate) output_agent_ids: Option<Vec<String>>,
}

impl Default for ObserverEventSelection {
    fn default() -> Self {
        Self {
            include_output: true,
            output_agent_ids: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalObserverClient {
    http: Client,
    base_url: Url,
    bearer: String,
    runtime_id: String,
}

impl LocalObserverClient {
    pub(crate) async fn discover(paths: &LocalPaths) -> Result<(Self, ObserverBootstrap)> {
        let discovery = load_runtime_discovery(paths)?
            .context("Zodex Local is not running: active runtime discovery is unavailable")?;
        validate_discovery(paths, &discovery)?;

        let bearer =
            fs::read_to_string(&discovery.observability.bearer_token_path).with_context(|| {
                format!(
                    "failed to read Local observability bearer at {}",
                    discovery.observability.bearer_token_path.display()
                )
            })?;
        let client = Self::attach(
            &discovery.observability.base_url,
            bearer.trim(),
            &discovery.runtime_id,
        )?;
        let status = client.status().await?;
        let agents = client.current_agents().await?;
        Ok((
            client,
            ObserverBootstrap {
                discovery,
                status,
                agents,
            },
        ))
    }

    pub(crate) fn attach(base_url: &str, bearer: &str, runtime_id: &str) -> Result<Self> {
        if bearer.len() < MIN_BEARER_LEN || bearer.contains(['\r', '\n']) {
            bail!("Local observability bearer is invalid; run `zodex local setup` again");
        }
        crate::install_rustls_crypto_provider();
        let base_url = validate_base_url(base_url)?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .context("failed to construct Local observer HTTP client")?;
        Ok(Self {
            http,
            base_url,
            bearer: bearer.to_owned(),
            runtime_id: runtime_id.to_owned(),
        })
    }

    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub(crate) async fn status(&self) -> Result<ApiStatusDocument> {
        let status: ApiStatusDocument = self.get_json("v1/status", &[]).await?;
        self.validate_runtime(&status.runtime_id)?;
        validate_api_schema(status.schema_version, "status")?;
        if status.api_version != LOCAL_OBSERVABILITY_API_VERSION {
            bail!(
                "Local observability API version changed from {} to {}; use a matching Zodex binary",
                LOCAL_OBSERVABILITY_API_VERSION,
                status.api_version
            );
        }
        if status.presentation_version != PRESENTATION_SCHEMA_VERSION {
            bail!(
                "Local presentation version changed from {} to {}; use a matching Zodex binary",
                PRESENTATION_SCHEMA_VERSION,
                status.presentation_version
            );
        }
        Ok(status)
    }

    pub(crate) async fn current_agents(&self) -> Result<Vec<ApiAgent>> {
        let list: ApiAgentList = self
            .get_json("v1/agents", &[("runtime", "current")])
            .await?;
        self.validate_runtime(&list.runtime_id)?;
        validate_api_schema(list.schema_version, "Agent list")?;
        Ok(list.agents)
    }

    pub(crate) async fn invocation(&self, id: i64) -> Result<ApiInvocationDetail> {
        let detail: ApiInvocationDetail =
            self.get_json(&format!("v1/invocations/{id}"), &[]).await?;
        self.validate_runtime(&detail.runtime_id)?;
        validate_api_schema(detail.schema_version, "invocation detail")?;
        validate_presentation_version(detail.presentation_version, "invocation")?;
        Ok(detail)
    }

    pub(crate) async fn recovery_invocations(
        &self,
        agent_id: Option<&str>,
        since_ms: i64,
        limit: usize,
    ) -> Result<ApiInvocationList> {
        let limit = limit.to_string();
        let since_ms = since_ms.to_string();
        let mut query = vec![
            ("last", limit.as_str()),
            ("recovery_since_ms", since_ms.as_str()),
        ];
        if let Some(agent_id) = agent_id {
            query.push(("agent_id", agent_id));
        }
        let list: ApiInvocationList = self.get_json("v1/invocations", &query).await?;
        self.validate_runtime(&list.runtime_id)?;
        validate_api_schema(list.schema_version, "recovery invocation list")?;
        validate_presentation_version(list.presentation_version, "recovery")?;
        Ok(list)
    }

    pub(crate) async fn timeline_detail(&self, presentation_id: &str) -> Result<ApiTimelineDetail> {
        let detail: ApiTimelineDetail = self
            .get_json(&format!("v1/timeline/{presentation_id}"), &[])
            .await?;
        self.validate_runtime(&detail.runtime_id)?;
        validate_api_schema(detail.schema_version, "timeline detail")?;
        validate_presentation_version(detail.presentation_version, "timeline")?;
        Ok(detail)
    }

    pub(crate) async fn events_response(
        &self,
        agent_id: Option<&str>,
        selection: &ObserverEventSelection,
    ) -> Result<Response> {
        let output_agent_ids;
        let mut query = Vec::new();
        if let Some(agent_id) = agent_id {
            query.push(("agent_id", agent_id));
        }
        if !selection.include_output {
            query.push(("include_output", "false"));
        } else if let Some(agent_ids) = selection.output_agent_ids.as_ref() {
            output_agent_ids = agent_ids.join(",");
            query.push(("output_agent_ids", output_agent_ids.as_str()));
        }
        ensure_success(self.get("v1/events", &query).await?).await
    }

    pub(crate) async fn raw_get(&self, path: &str, raw_query: Option<&str>) -> Result<Response> {
        let mut url = self
            .base_url
            .join(path)
            .with_context(|| format!("invalid Local observability path `{path}`"))?;
        url.set_query(raw_query.filter(|query| !query.is_empty()));
        let send = self.http.get(url).bearer_auth(&self.bearer).send();
        tokio::time::timeout(RESPONSE_HEADER_TIMEOUT, send)
            .await
            .context("Local observability request timed out")?
            .context("failed to connect to Local observability API")
    }

    async fn get_json<T>(&self, path: &str, query: &[(&str, &str)]) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = ensure_success(self.get(path, query).await?).await?;
        response
            .json::<T>()
            .await
            .with_context(|| format!("failed to decode Local observer response from /{path}"))
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response> {
        let url = self
            .base_url
            .join(path)
            .with_context(|| format!("invalid Local observability path `{path}`"))?;
        let send = self
            .http
            .get(url)
            .bearer_auth(&self.bearer)
            .query(query)
            .send();
        tokio::time::timeout(RESPONSE_HEADER_TIMEOUT, send)
            .await
            .context("Local observability request timed out")?
            .context("failed to connect to Local observability API")
    }

    fn validate_runtime(&self, runtime_id: &str) -> Result<()> {
        if runtime_id != self.runtime_id {
            bail!(
                "Local runtime changed from {} to {}; rediscover the active Local observer",
                self.runtime_id,
                runtime_id
            );
        }
        Ok(())
    }
}

fn validate_api_schema(schema_version: u32, surface: &str) -> Result<()> {
    if schema_version != LOCAL_OBSERVABILITY_API_VERSION {
        bail!(
            "Local {surface} schema version changed from {} to {}; use a matching Zodex binary",
            LOCAL_OBSERVABILITY_API_VERSION,
            schema_version
        );
    }
    Ok(())
}

fn validate_presentation_version(version: u32, surface: &str) -> Result<()> {
    if version != PRESENTATION_SCHEMA_VERSION {
        bail!("Local {surface} presentation version {version} is unsupported by this Zodex binary");
    }
    Ok(())
}

async fn ensure_success(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Local observability request failed".to_owned());
    bail!("{message} (HTTP {status})")
}

fn validate_discovery(paths: &LocalPaths, discovery: &LocalRuntimeDiscovery) -> Result<()> {
    if discovery.observability.api_version != LOCAL_OBSERVABILITY_API_VERSION {
        bail!(
            "Local discovery advertises observability API version {}, but this Zodex supports {}",
            discovery.observability.api_version,
            LOCAL_OBSERVABILITY_API_VERSION
        );
    }
    if discovery.observability.presentation_version != PRESENTATION_SCHEMA_VERSION {
        bail!(
            "Local discovery advertises presentation version {}, but this Zodex supports {}",
            discovery.observability.presentation_version,
            PRESENTATION_SCHEMA_VERSION
        );
    }
    if discovery.observability.bearer_token_path != paths.observability_bearer_file() {
        bail!(
            "Local discovery points at an unexpected observability bearer path {}; expected {}",
            discovery.observability.bearer_token_path.display(),
            paths.observability_bearer_file().display()
        );
    }
    if !discovery.observability.sse_available {
        bail!("active Local runtime does not advertise live SSE support")
    }
    Ok(())
}

fn validate_base_url(value: &str) -> Result<Url> {
    let mut url =
        Url::parse(value).context("Local discovery contains an invalid observability URL")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Local observability URL must be a credential-free http://127.0.0.1:<port> URL")
    }
    url.set_path("/");
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::validate_base_url;

    #[test]
    fn observer_url_must_remain_plain_loopback() {
        assert!(validate_base_url("http://127.0.0.1:43123").is_ok());
        assert!(validate_base_url("https://127.0.0.1:43123").is_err());
        assert!(validate_base_url("http://localhost:43123").is_err());
        assert!(validate_base_url("http://example.com:43123").is_err());
        assert!(validate_base_url("http://user@127.0.0.1:43123").is_err());
    }
}
