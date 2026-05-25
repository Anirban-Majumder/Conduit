// Global hotkey. Spec §"Open questions" — Wayland-no-SNI fallback to reach the
// GUI window when the tray is unavailable.

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

/// Register Ctrl+Alt+Shift+M as the default focus hotkey (spec default).
/// Returns None if registration fails (already registered elsewhere etc.).
pub fn try_install() -> Option<HotkeyHandles> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "hotkey: manager init failed");
            return None;
        }
    };
    let key = HotKey::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyM,
    );
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
        .name("mcp-router-hotkey".into())
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
