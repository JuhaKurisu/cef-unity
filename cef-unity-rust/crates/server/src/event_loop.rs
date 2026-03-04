// Platform-specific event loop for CEF message pump + IPC polling.

#[cfg(not(target_os = "macos"))]
mod generic;
#[cfg(target_os = "macos")]
mod macos;

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

#[cfg(target_os = "macos")]
pub use macos::{run_event_loop, schedule_pump};

#[cfg(not(target_os = "macos"))]
pub use generic::{run_event_loop, schedule_pump};
