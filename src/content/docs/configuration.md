---
title: Configuration
description: Configure bind addresses, TLS, API keys, workspace defaults, session limits, GitHub App credentials, publisher settings, and grant behavior.
order: 4
category: Architecture
summary: The `/etc/zodex/config.toml` fields that control the daemon, tools, GitHub access, and publisher service.
---

## Config path

All CLIs accept a config path. The default is:

```bash
/etc/zodex/config.toml
```

Use `--config` when operating against another file:

```bash
zodex --config /etc/zodex/config.toml status
zodex-agent --config /etc/zodex/config.toml github list-grants
```

If the file is missing, zodex loads its built-in defaults.

## Server and API settings

Important runtime defaults:

```toml
bind_host = "0.0.0.0"
bind_port = 443
http_bind_port = 8080
api_key = "zodex-runtime-key"
tls_mode = "auto"
tls_cert_path = "/var/lib/zodex/tls/cert.pem"
tls_key_path = "/var/lib/zodex/tls/key.pem"
```

`zodexd` always expects TLS cert and key files for the TLS listener. Run one of these before starting the daemon directly:

```bash
zodex tls setup
zodex start
```

The optional HTTP listener is controlled by `http_bind_port`. The same app routes are served, but the `/v1/*` endpoints still require Bearer auth.

## Tool execution limits

Execution-related defaults:

```toml
max_sessions = 64
default_exec_timeout_ms = 7200000
max_exec_timeout_ms = 7200000
default_exec_yield_time_ms = 10000
default_write_yield_time_ms = 10000
max_output_chars = 200000
default_workdir = "/workspace"
```

`exec_command` resolves its working directory from the tool input first, then from `default_workdir`. Long-running commands can keep a session open and return a `session_handle` for later polling or stdin writes.

## Guest users and paths

Default runtime users and paths:

```toml
agent_user = "zodex-agent"
agent_home = "/home/zodex-agent"
publisher_user = "zodex-publisher"
service_group = "zodex"
publisher_socket_path = "/var/lib/zodex/publisher/run/zodex-prd.sock"
```

The agent should not run as root. The publisher path must be writable by the configured publisher user.

## Reader GitHub App

Reader app fields:

```toml
reader_app_id = 123456
reader_installation_id = 11111111
reader_private_key_path = "/etc/zodex/reader/private-key.pem"
```

The reader app should have only `Contents: Read-only`, and be installed on repositories the runtime may read. Clone/fetch tokens request only `Contents: read`. `publish-pr` is handled by the publisher daemon, not the reader app.

## Push-grant GitHub App

Publisher and grant fields:

```toml
publisher_app_id = 987654
publisher_client_id = "Iv1.real-device-flow-client-id"
publisher_private_key_path = "/etc/zodex/publisher/private-key.pem"
publisher_branch_prefix = "agent"
publisher_max_bundle_bytes = 33554432
publisher_max_title_chars = 240
publisher_max_body_chars = 16000
```

The publisher / push-grant app should have `Contents: Read & write`, `Pull requests: Read & write`, Device Flow enabled, and user access token expiration enabled. Agent-side `publish-pr` sends a local HEAD bundle to the publisher daemon, which uses the publisher app to push a generated branch and open the PR while keeping credentials inside the daemon.

## Publish targets

Publish targets identify repositories that publisher-side flows can operate on:

```toml
[[publisher_targets]]
id = "zodex"
repo = "amxv/zodex"
default_base = "main"
installation_id = 22222222

[[publisher_installations]]
account = "amxv"
default_base = "main"
installation_id = 22222222
```

`publisher_targets` is the explicit allowlist used by `publish-pr`. `publisher_installations` records account-level installations so operator-only GitHub modes can represent an all-installed-repos scope while still staying inside the GitHub App installation boundary.

The day-to-day `request-push` flow uses the repo argument and active grant state. Publish targets are still useful for internal publisher flows and explicit repo allowlists.
