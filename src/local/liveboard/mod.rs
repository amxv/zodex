mod assets;
mod bridge;
mod discovery;
mod launch;
#[cfg(target_os = "macos")]
mod notifier;
mod prefs;
mod server;

#[cfg(test)]
mod server_tests;

pub use launch::{copy_local_liveboard_url, local_liveboard_url, run_local_liveboard};

pub(crate) use discovery::{
    LocalLiveboardDiscovery, remove_liveboard_discovery, write_liveboard_discovery,
};
#[cfg(target_os = "macos")]
pub(crate) use notifier::LiveboardLinkNotifier;
pub(crate) use server::{LocalLiveboardHost, start_liveboard_host};
