// Platform-specific event loop for CEF message pump + IPC polling.

#[cfg(not(target_os = "macos"))]
mod generic;
#[cfg(target_os = "macos")]
mod macos;

use std::sync::mpsc;

use ipc_channel::ipc::IpcSender;

use cef_unity_ipc::{Command, Response};

use crate::server::CefServer;

pub struct ServerState {
    pub cef_server: CefServer,
    /// IPC bridge thread がコマンドを転送してくる mpsc チャネル。
    pub cmd_rx: mpsc::Receiver<Command>,
    pub resp_tx: IpcSender<Response>,
    pub running: bool,
    pub pump_count: u64,
}

#[cfg(target_os = "macos")]
pub use macos::{run_event_loop, schedule_pump};

#[cfg(not(target_os = "macos"))]
pub use generic::{run_event_loop, schedule_pump};
