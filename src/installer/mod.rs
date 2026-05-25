// One-shot service install at first run. Spec §"Autostart at login".
// Linux systemd user unit / macOS LaunchAgent plist / Windows Task Scheduler.

#[cfg(target_os = "linux")]
pub mod systemd_linux;

#[cfg(target_os = "macos")]
pub mod launchd_macos;

#[cfg(target_os = "windows")]
pub mod scheduled_task_windows;
