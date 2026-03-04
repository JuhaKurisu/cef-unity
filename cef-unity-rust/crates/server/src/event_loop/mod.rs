// Platform-specific event loop for CEF message pump + IPC polling.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod generic;

use ipc_channel::ipc::{IpcReceiver, IpcSender};

use cef_unity_ipc::{Command, Response};

use crate::server::CefServer;

pub struct ServerState {
    pub cef_server: CefServer,
    pub cmd_rx: IpcReceiver<Command>,
    pub resp_tx: IpcSender<Response>,
    pub running: bool,
    pub pump_count: u64,
}

/// Run the event loop until shutdown. Returns the ServerState for cleanup.
#[cfg(target_os = "macos")]
pub fn run_event_loop(state: ServerState) -> ServerState {
    macos::run_event_loop(state)
}

#[cfg(not(target_os = "macos"))]
pub fn run_event_loop(state: ServerState) -> ServerState {
    generic::run_event_loop(state)
}
