mod cards;
mod client;
#[cfg(test)]
mod client_tests;
mod input;
mod model;
#[cfg(test)]
mod model_tests;
mod render;
#[cfg(test)]
mod render_tests;
mod sse;
#[cfg(test)]
mod test_support;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::io::Write as _;
use std::io::{self, IsTerminal as _};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use client::{NETWORK_EVENT_CAPACITY, ObserverClient, WatchNetworkEvent};
use input::map_key;
pub use model::WatchOptions;
use model::{ConnectionState, WatchApp, WatchEffect};

use super::LocalPaths;

pub async fn run_local_watch(paths: &LocalPaths, options: WatchOptions) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("`zodex local watch` requires an interactive terminal")
    }
    if options.all && options.agent.is_some() {
        bail!("`zodex local watch` cannot combine `--all` and `--agent`")
    }
    if let Some(agent_id) = options.agent.as_deref()
        && !valid_agent_id(agent_id)
    {
        bail!("`zodex local watch --agent` expects an Agent ID matching [a-z0-9]{{4}}")
    }

    let (client, bootstrap) = ObserverClient::discover(paths).await?;
    let mut app = WatchApp::new(&bootstrap, options);
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error).context("failed to initialize Local watch terminal");
        }
    };
    let mut restore_guard = RestoreGuard::new(ratatui::restore);
    let run_result = run_loop(&mut terminal, client, &mut app).await;
    let restore_result =
        ratatui::try_restore().context("failed to restore terminal after Local watch");
    if restore_result.is_ok() {
        restore_guard.disarm();
    }
    restore_result?;
    run_result
}

fn valid_agent_id(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    client: ObserverClient,
    app: &mut WatchApp,
) -> Result<()> {
    let (network_tx, mut network_rx) = mpsc::channel(NETWORK_EVENT_CAPACITY);
    let mut subscription = Subscription::start(&client, 1, app.stream_filter(), network_tx.clone());
    let mut generation = 1_u64;
    let mut input = EventStream::new();
    let mut redraw = tokio::time::interval(Duration::from_secs(1));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| render::render(frame, app))?;
        tokio::select! {
            _ = redraw.tick() => {}
            event = input.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if let Some(input) = map_key(key, app.search_input.is_some()) {
                            let effects = app.apply_input(input);
                            if apply_effects(
                                effects,
                                &client,
                                app,
                                &network_tx,
                                &mut subscription,
                                &mut generation,
                            ).await? {
                                return Ok(());
                            }
                        }
                    }
                    Some(Ok(Event::Resize(_, _))) | Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error).context("failed to read terminal input"),
                    None => return Ok(()),
                }
            }
            event = network_rx.recv() => {
                let Some(event) = event else {
                    return Ok(());
                };
                handle_network_event(
                    event,
                    generation,
                    &client,
                    app,
                    &network_tx,
                    &mut subscription,
                    &mut generation,
                ).await?;
            }
        }
    }
}

async fn handle_network_event(
    event: WatchNetworkEvent,
    current_generation: u64,
    client: &ObserverClient,
    app: &mut WatchApp,
    network_tx: &mpsc::Sender<WatchNetworkEvent>,
    subscription: &mut Subscription,
    generation: &mut u64,
) -> Result<()> {
    match event {
        WatchNetworkEvent::Connected(event_generation)
            if event_generation == current_generation =>
        {
            subscription.mark_connected(event_generation);
            let recovering = !matches!(app.connection, ConnectionState::Connecting);
            app.set_connected();
            if recovering {
                match recover_from_history(client, app).await {
                    Ok(()) => app.set_recovered(
                        "live stream reconnected; state recovered from durable history",
                    ),
                    Err(error) => app.set_degraded(error),
                }
            }
            // Bootstrap status/Agent discovery is necessarily a snapshot taken
            // before the SSE subscription is established. Refresh those facts
            // at the connection boundary so an Agent first seen in that race
            // window does not leave watch stuck in its stale waiting/picker
            // state. Do not recover invocation history here on the first
            // connection: the timeline remains strictly from-now.
            let effects = refresh_agents(client, app).await;
            let _ =
                apply_effects(effects, client, app, network_tx, subscription, generation).await?;
        }
        WatchNetworkEvent::Disconnected(event_generation, message)
            if event_generation == current_generation =>
        {
            app.set_degraded(message);
        }
        WatchNetworkEvent::Live(event_generation, live)
            if subscription.accepts_generation(event_generation) =>
        {
            let effects = apply_live_update(client, app, &live).await;
            let _ =
                apply_effects(effects, client, app, network_tx, subscription, generation).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn apply_live_update(
    client: &ObserverClient,
    app: &mut WatchApp,
    live: &crate::local::history::HistoryLiveEvent,
) -> Vec<WatchEffect> {
    if !app.should_process_live_event(live) {
        return Vec::new();
    }
    let gap = live.event_type == "gap";
    let invocation_id = live.invocation_id;
    let known = invocation_id.is_some_and(|id| app.knows_invocation(id));
    let effects = app.note_live_event(live);

    if gap {
        match recover_from_history(client, app).await {
            Ok(()) => app.set_recovered("live event gap recovered from durable history"),
            Err(error) => app.set_degraded(error),
        }
    } else if let Some(invocation_id) = invocation_id {
        if !known {
            if let Err(error) =
                merge_live_detail(client, app, invocation_id, live.presentation_id.as_deref()).await
            {
                app.set_degraded(format!(
                    "live invocation {invocation_id} could not be loaded: {error:#}"
                ));
            }
        } else if live.event_type == "output" {
            if let Some(chunks) = live
                .payload
                .get("chunks")
                .and_then(serde_json::Value::as_array)
            {
                let text = chunks
                    .iter()
                    .filter_map(|chunk| chunk.get("text").and_then(serde_json::Value::as_str))
                    .collect::<String>();
                if !text.is_empty() {
                    app.append_live_output(invocation_id, &text);
                }
            }
        } else if matches!(
            live.event_type.as_str(),
            "presentation_updated" | "invocation_completed"
        ) && let Err(error) =
            merge_live_detail(client, app, invocation_id, live.presentation_id.as_deref()).await
        {
            app.set_degraded(format!(
                "live invocation {invocation_id} could not be refreshed: {error:#}"
            ));
        }
    }
    effects
}

async fn merge_live_detail(
    client: &ObserverClient,
    app: &mut WatchApp,
    invocation_id: i64,
    presentation_id: Option<&str>,
) -> Result<(), String> {
    let detail = client
        .invocation(invocation_id)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let is_poll = detail.presentation.records.iter().any(|record| {
        matches!(
            record.kind,
            crate::local::PresentationKind::PollAggregate { .. }
        )
    });
    if is_poll {
        if let Some(presentation_id) = presentation_id {
            app.cache_detail(detail.clone());
            match client.timeline_detail(presentation_id).await {
                Ok(canonical) => app.merge_timeline_record(canonical.record),
                Err(error) => {
                    app.merge_detail(detail);
                    return Err(format!(
                        "failed to reconcile canonical poll presentation `{presentation_id}`: {error:#}"
                    ));
                }
            }
        } else {
            app.merge_detail(detail);
            let canonical = client
                .recovery_invocations(app.stream_filter().as_deref(), app.recovery_since_ms())
                .await
                .map_err(|error| format!("failed to reconcile poll presentation: {error:#}"))?;
            app.merge_presentation(canonical.presentation);
        }
    } else {
        app.merge_detail(detail);
    }
    Ok(())
}

async fn apply_effects(
    mut effects: Vec<WatchEffect>,
    client: &ObserverClient,
    app: &mut WatchApp,
    network_tx: &mpsc::Sender<WatchNetworkEvent>,
    subscription: &mut Subscription,
    generation: &mut u64,
) -> Result<bool> {
    while let Some(effect) = effects.pop() {
        match effect {
            WatchEffect::Quit => return Ok(true),
            WatchEffect::Resubscribe(filter) => {
                *generation = generation.saturating_add(1);
                app.connection = ConnectionState::Connecting;
                subscription.replace(client, *generation, filter, network_tx.clone());
            }
            WatchEffect::RefreshAgents => {
                effects.extend(refresh_agents(client, app).await);
            }
            WatchEffect::CycleAgents(direction) => match client.current_agents().await {
                Ok(agents) => {
                    let _ = app.set_agents(agents);
                    effects.extend(app.cycle_agents_after_refresh(direction));
                }
                Err(error) => {
                    app.set_degraded(format!(
                        "failed to refresh current Agents before switching: {error:#}"
                    ));
                }
            },
            WatchEffect::Copy(text) => {
                if let Err(error) = copy_to_clipboard(&text).await {
                    app.set_degraded(format!("clipboard copy failed: {error:#}"));
                }
            }
        }
    }
    Ok(false)
}

async fn refresh_agents(client: &ObserverClient, app: &mut WatchApp) -> Vec<WatchEffect> {
    let agents = match client.current_agents().await {
        Ok(agents) => agents,
        Err(error) => {
            app.set_degraded(format!("failed to refresh current Agents: {error:#}"));
            return Vec::new();
        }
    };
    let effects = app.set_agents(agents);
    match client.status().await {
        Ok(status) => app.set_status_active_process_count(status.active_process_count),
        Err(error) => {
            app.set_degraded(format!("failed to refresh Local status: {error:#}"));
        }
    }
    effects
}

async fn recover_from_history(client: &ObserverClient, app: &mut WatchApp) -> Result<(), String> {
    let filter = app.stream_filter();
    let mut invocation_ids = app.known_invocation_ids();
    let mut errors = Vec::new();
    match client
        .recovery_invocations(filter.as_deref(), app.recovery_since_ms())
        .await
    {
        Ok(list) => {
            if list.invocations.len() == 100 {
                errors.push(
                    "durable recovery reached the 100-invocation API bound; use `zodex local history` to inspect possible older missed activity"
                        .to_owned(),
                );
            }
            invocation_ids.extend(list.invocations.into_iter().map(|record| record.id));
        }
        Err(error) => errors.push(format!(
            "failed to query durable Local recovery state: {error:#}"
        )),
    }
    invocation_ids.sort_unstable();
    invocation_ids.dedup();
    for invocation_id in invocation_ids {
        match client.invocation(invocation_id).await {
            Ok(detail) => app.merge_detail(detail),
            Err(error) => errors.push(format!(
                "failed to recover Local invocation {invocation_id}: {error:#}"
            )),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

struct EventStreamSubscription {
    generation: u64,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl EventStreamSubscription {
    fn start(
        client: &ObserverClient,
        generation: u64,
        filter: Option<String>,
        sender: mpsc::Sender<WatchNetworkEvent>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let task = client.spawn_event_stream(generation, filter, sender, cancellation.clone());
        Self {
            generation,
            cancellation,
            task,
        }
    }

    fn stop(self) -> u64 {
        self.cancellation.cancel();
        self.task.abort();
        self.generation
    }
}

struct Subscription {
    current: EventStreamSubscription,
    previous: Option<EventStreamSubscription>,
    draining_generation: Option<u64>,
}

impl Subscription {
    fn start(
        client: &ObserverClient,
        generation: u64,
        filter: Option<String>,
        sender: mpsc::Sender<WatchNetworkEvent>,
    ) -> Self {
        Self {
            current: EventStreamSubscription::start(client, generation, filter, sender),
            previous: None,
            draining_generation: None,
        }
    }

    fn replace(
        &mut self,
        client: &ObserverClient,
        generation: u64,
        filter: Option<String>,
        sender: mpsc::Sender<WatchNetworkEvent>,
    ) {
        if let Some(previous) = self.previous.take() {
            self.draining_generation = Some(previous.stop());
        }
        let next = EventStreamSubscription::start(client, generation, filter, sender);
        let previous = std::mem::replace(&mut self.current, next);
        self.previous = Some(previous);
    }

    fn mark_connected(&mut self, generation: u64) {
        if self.current.generation != generation {
            return;
        }
        if let Some(previous) = self.previous.take() {
            self.draining_generation = Some(previous.stop());
        }
    }

    fn accepts_generation(&self, generation: u64) -> bool {
        self.current.generation == generation
            || self
                .previous
                .as_ref()
                .is_some_and(|previous| previous.generation == generation)
            || self.draining_generation == Some(generation)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.current.cancellation.cancel();
        self.current.task.abort();
        if let Some(previous) = self.previous.as_mut() {
            previous.cancellation.cancel();
            previous.task.abort();
        }
    }
}

struct RestoreGuard<F: FnMut()> {
    restore: Option<F>,
}

impl<F: FnMut()> RestoreGuard<F> {
    fn new(restore: F) -> Self {
        Self {
            restore: Some(restore),
        }
    }

    fn disarm(&mut self) {
        self.restore = None;
    }
}

impl<F: FnMut()> Drop for RestoreGuard<F> {
    fn drop(&mut self) {
        if let Some(mut restore) = self.restore.take() {
            restore();
        }
    }
}

#[cfg(target_os = "macos")]
async fn copy_to_clipboard(text: &str) -> Result<()> {
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to start /usr/bin/pbcopy")?;
        child
            .stdin
            .as_mut()
            .context("pbcopy stdin was unavailable")?
            .write_all(text.as_bytes())
            .context("failed to write clipboard content")?;
        let status = child.wait().context("failed to wait for pbcopy")?;
        if !status.success() {
            bail!("pbcopy exited with {status}")
        }
        Ok(())
    })
    .await
    .context("clipboard worker failed")?
}

#[cfg(target_os = "windows")]
async fn copy_to_clipboard(text: &str) -> Result<()> {
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new("clip.exe")
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to start clip.exe")?;
        child
            .stdin
            .as_mut()
            .context("clip.exe stdin was unavailable")?
            .write_all(text.as_bytes())
            .context("failed to write clipboard content")?;
        let status = child.wait().context("failed to wait for clip.exe")?;
        if !status.success() {
            bail!("clip.exe exited with {status}")
        }
        Ok(())
    })
    .await
    .context("clipboard worker failed")?
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn copy_to_clipboard(_text: &str) -> Result<()> {
    bail!("clipboard copy is available in the supported macOS and Windows Local runtimes")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::RestoreGuard;

    #[test]
    fn restore_guard_runs_once_on_drop_and_can_be_disarmed() {
        let calls = Rc::new(Cell::new(0));
        {
            let calls = calls.clone();
            let _guard = RestoreGuard::new(move || calls.set(calls.get() + 1));
        }
        assert_eq!(calls.get(), 1);

        {
            let calls = calls.clone();
            let mut guard = RestoreGuard::new(move || calls.set(calls.get() + 1));
            guard.disarm();
        }
        assert_eq!(calls.get(), 1);
    }
}
