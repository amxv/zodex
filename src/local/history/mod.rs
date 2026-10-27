mod clear;
mod event_identity;
mod events;
mod file_store;
mod lifecycle;
mod live_display;
mod materialized;
mod output_display;
mod query;
mod retention;
mod schema;
mod store;
mod timeline;
mod timeline_cursor;
mod timeline_polls;
mod worker;

#[cfg(test)]
mod clear_tests;
#[cfg(test)]
mod first_seen_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod maintenance_tests;
#[cfg(test)]
mod migration_tests;
#[cfg(test)]
mod output_completion_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod shutdown_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod timeline_tests;

pub use clear::clear_local_agent_history;
pub use query::{
    HistoryAgentSummary, HistoryAgentWorkdir, HistoryFileEvidence, HistoryFormat,
    HistoryInvocation, HistoryQuery, HistoryStoreStatus, LocalHistoryReader, LocalRecentActivity,
};
pub use schema::HISTORY_SCHEMA_VERSION;
pub use worker::{LocalHistoryRuntime, LocalHistoryRuntimeConfig};

pub(crate) use events::{HISTORY_LIVE_EVENT_SCHEMA_VERSION, HistoryLiveEvent};
pub(crate) use materialized::HistoryDiffProjection;
pub(crate) use query::{HistoryAgentRecord, HistoryOutputChunk, HistoryOutputMetadata};
pub(crate) use store::normalize_declared_workdir;
pub(crate) use timeline::{HistoryTimelineCheckpoint, HistoryTimelineMode, HistoryTimelineQuery};
pub(crate) use timeline_cursor::HistoryTimelineCursor;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub fn clear_local_history(path: &Path) -> Result<()> {
    for candidate in history_store_paths(path) {
        if candidate.exists() {
            fs::remove_file(&candidate).with_context(|| {
                format!(
                    "failed to remove Local history store {}",
                    candidate.display()
                )
            })?;
        }
    }
    Ok(())
}

pub(crate) fn history_store_paths(path: &Path) -> [std::path::PathBuf; 3] {
    [
        path.to_path_buf(),
        std::path::PathBuf::from(format!("{}-wal", path.display())),
        std::path::PathBuf::from(format!("{}-shm", path.display())),
    ]
}
