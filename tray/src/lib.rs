//! `memocampus-tray`: the Windows tray application that keeps the Memocampus MCP server (the
//! existing `memocampus-mcp` executable, Streamable HTTP only) running, and manages it from outside.
//!
//! Issue #67, Slice 1 is the base: silent start, a management channel for a graceful stop, a job
//! object against orphans, the supervisor with its states, the settings file and the single-instance
//! guard. The tray icon, its menu and the logon auto-start are Slice 2.
//!
//! What this crate never does: read a vault, write `config.json` or `policy.json`, choose which
//! vault is active, publish anything, change the transport or the listen address beyond a loopback
//! port, or stop a process it did not start.

pub mod classify;
pub mod instance;
pub mod settings;
pub mod state;
pub mod supervisor;

#[cfg(windows)]
pub mod cmdline;
#[cfg(windows)]
pub mod winproc;
