//! The tray does not own the Local runtime. CLI status observations, not
//! completion of Start/Stop subprocesses, determine what the menu displays.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    Stopped,
    Stale,
    Unconfigured,
    Unavailable,
}

impl RuntimeState {
    pub fn parse(state: &str) -> Self {
        match state {
            "running" => Self::Running,
            "stopped" => Self::Stopped,
            "stale" => Self::Stale,
            "unconfigured" => Self::Unconfigured,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    id: u64,
    operation: Operation,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Presentation {
    pub label: &'static str,
    pub tooltip: &'static str,
    pub start_enabled: bool,
    pub stop_enabled: bool,
    pub liveboard_enabled: bool,
}

#[derive(Default)]
pub struct Lifecycle {
    observed: Option<RuntimeState>,
    pending: Option<Pending>,
}

impl Lifecycle {
    pub fn begin(&mut self, id: u64, operation: Operation) {
        self.pending = Some(Pending { id, operation });
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn observe(&mut self, state: RuntimeState) -> Option<u64> {
        self.observed = Some(state);
        let reached_target = matches!(
            (self.pending, state),
            (
                Some(Pending {
                    operation: Operation::Start,
                    ..
                }),
                RuntimeState::Running
            ) | (
                Some(Pending {
                    operation: Operation::Stop,
                    ..
                }),
                RuntimeState::Stopped
            ) | (
                Some(Pending {
                    operation: Operation::Stop,
                    ..
                }),
                RuntimeState::Unconfigured
            )
        );
        if reached_target {
            // The CLI might still be waiting for readiness or a descendant
            // inherited its stdio pipe. Status is authoritative either way.
            return self.pending.take().map(|pending| pending.id);
        }
        None
    }

    pub fn finish(&mut self, id: u64) {
        if self.pending.is_some_and(|pending| pending.id == id) {
            self.pending = None;
        }
    }

    pub fn presentation(&self, valid_folder: bool, other_command_pending: bool) -> Presentation {
        if let Some(pending) = self.pending {
            return match pending.operation {
                Operation::Start => Presentation {
                    label: "Starting Zodex…",
                    tooltip: "Zodex: starting",
                    start_enabled: false,
                    stop_enabled: false,
                    liveboard_enabled: false,
                },
                Operation::Stop => Presentation {
                    label: "Stopping Zodex…",
                    tooltip: "Zodex: stopping",
                    start_enabled: false,
                    stop_enabled: false,
                    liveboard_enabled: false,
                },
            };
        }
        let mut state = match self.observed.unwrap_or(RuntimeState::Unavailable) {
            RuntimeState::Running => Presentation {
                label: "Zodex is Running",
                tooltip: "Zodex: running",
                start_enabled: false,
                stop_enabled: true,
                liveboard_enabled: true,
            },
            RuntimeState::Stopped => Presentation {
                label: "Start Zodex",
                tooltip: "Zodex: stopped",
                start_enabled: valid_folder,
                stop_enabled: false,
                liveboard_enabled: false,
            },
            RuntimeState::Stale => Presentation {
                label: "Zodex Needs Cleanup",
                tooltip: "Zodex: stale runtime, choose Stop to clean up",
                start_enabled: false,
                stop_enabled: true,
                liveboard_enabled: false,
            },
            RuntimeState::Unconfigured => Presentation {
                label: "Run zodex local setup first",
                tooltip: "Zodex: not configured",
                start_enabled: false,
                stop_enabled: false,
                liveboard_enabled: false,
            },
            RuntimeState::Unavailable => Presentation {
                label: "Zodex Status Unavailable",
                tooltip: "Zodex: unable to read status",
                start_enabled: false,
                stop_enabled: false,
                liveboard_enabled: false,
            },
        };
        if other_command_pending {
            state.start_enabled = false;
            state.stop_enabled = false;
            state.liveboard_enabled = false;
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_status_wins_before_a_slow_start_command_finishes() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(RuntimeState::Stopped);
        lifecycle.begin(1, Operation::Start);
        assert_eq!(lifecycle.presentation(true, false).label, "Starting Zodex…");
        assert_eq!(lifecycle.observe(RuntimeState::Stopped), None);
        assert!(!lifecycle.presentation(true, false).start_enabled);
        assert_eq!(lifecycle.observe(RuntimeState::Running), Some(1));
        let state = lifecycle.presentation(true, false);
        assert_eq!(state.label, "Zodex is Running");
        assert!(state.stop_enabled && state.liveboard_enabled);
        lifecycle.finish(1); // The old Start command finally exits.
        assert_eq!(lifecycle.presentation(true, false), state);
    }

    #[test]
    fn stopped_status_wins_before_a_slow_stop_command_finishes() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(RuntimeState::Running);
        lifecycle.begin(2, Operation::Stop);
        assert_eq!(lifecycle.presentation(true, false).label, "Stopping Zodex…");
        assert_eq!(lifecycle.observe(RuntimeState::Running), None);
        assert_eq!(lifecycle.observe(RuntimeState::Stopped), Some(2));
        assert!(lifecycle.presentation(true, false).start_enabled);
        assert!(!lifecycle.presentation(true, false).stop_enabled);
    }

    #[test]
    fn unrelated_late_command_cannot_clear_a_new_transition() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.begin(3, Operation::Start);
        lifecycle.observe(RuntimeState::Running);
        lifecycle.begin(4, Operation::Stop);
        lifecycle.finish(3);
        assert_eq!(lifecycle.presentation(true, false).label, "Stopping Zodex…");
        lifecycle.observe(RuntimeState::Stopped);
        assert_eq!(lifecycle.presentation(true, false).label, "Start Zodex");
    }

    #[test]
    fn unknown_status_does_not_claim_running_or_enable_destructive_actions() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.observe(RuntimeState::Running);
        lifecycle.observe(RuntimeState::Unavailable);
        let presentation = lifecycle.presentation(true, false);
        assert_eq!(presentation.label, "Zodex Status Unavailable");
        assert!(!presentation.stop_enabled);
        assert!(!presentation.start_enabled);
    }
}
