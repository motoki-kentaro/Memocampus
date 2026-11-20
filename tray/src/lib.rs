//! `memocampus-tray`: the Windows tray application that keeps the Memocampus MCP server (the
//! existing `memocampus-mcp` executable, Streamable HTTP only) running, and manages it from outside.
//!
//! Issue #67, Slice 1 is the base: silent start, a management channel for a graceful stop, a job
//! object against orphans, the supervisor with its states, the settings file and the single-instance
//! guard. Slice 2 adds the tray icon and its menu (`ui`, `view`), the logon auto-start through the
//! per-user Run key (`autostart`) and the operations behind the menu (`controller`).
//!
//! What this crate never does: read a vault, write `config.json` or `policy.json`, choose which
//! vault is active, publish anything, change the transport or the listen address beyond a loopback
//! port, or stop a process it did not start.

pub mod autostart;
pub mod classify;
pub mod controller;
pub mod instance;
pub mod mode;
pub mod quit;
pub mod render;
pub mod settings;
pub mod state;
pub mod supervisor;
pub mod view;
pub mod wake;

#[cfg(windows)]
pub mod cmdline;
#[cfg(windows)]
pub mod ui;
#[cfg(windows)]
pub mod winproc;
