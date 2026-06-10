//! Global focus hotkey (from `gui.focus_hotkey`, default Ctrl+Alt+Shift+M) to
//! raise the GUI window.
//!
//! Wayland-no-SNI fallback for when the tray is unavailable (spec §"Open
//! questions"). The registration handle lives for the process lifetime — main
//! holds it and dropping it unregisters. The event pump runs on its own
//! `std::thread` and invokes the caller-supplied focus closure.

use std::str::FromStr;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};

/// Built once at startup; held alive by main (dropping unregisters). The
/// `manager` field is the live registration handle.
pub struct HotkeyHandles {
    pub manager: GlobalHotKeyManager,
    pub hotkey_id: u32,
}

impl std::fmt::Debug for HotkeyHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotkeyHandles")
            .field("hotkey_id", &self.hotkey_id)
            .finish_non_exhaustive()
    }
}

/// Register the configured focus hotkey.
///
/// An unparseable spec falls back to the Ctrl+Alt+Shift+M default — a
/// hand-edited config must not cost the hotkey. Returns None if registration
/// fails (already registered elsewhere etc.).
pub fn try_install(spec: &str) -> Option<HotkeyHandles> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "hotkey: manager init failed");
            return None;
        }
    };
    let key = HotKey::from_str(spec).unwrap_or_else(|e| {
        tracing::warn!(spec, error = %e, "hotkey: invalid focus_hotkey; using default");
        HotKey::new(
            Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
            Code::KeyM,
        )
    });
    let id = key.id();
    if let Err(e) = manager.register(key) {
        tracing::warn!(error = %e, "hotkey: register failed");
        return None;
    }
    Some(HotkeyHandles {
        manager,
        hotkey_id: id,
    })
}

/// Spawn a `std::thread` that pumps `GlobalHotKeyEvent`'s process-global channel.
pub fn spawn_event_pump(hotkey_id: u32, on_press: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("conduit-hotkey".into())
        .spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if ev.id == hotkey_id {
                    on_press();
                }
            }
        })
        .ok();
}
