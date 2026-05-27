use std::sync::{
    atomic::{AtomicI64, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use tokio::sync::broadcast::Receiver;
use tokio_util::sync::CancellationToken;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};

use crate::daemon::db::Db;
use crate::daemon::events::Event;

const ICON_SIZE: u32 = 22;
const ATTENTION_DOT_RADIUS: i16 = 3;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
#[expect(
    clippy::struct_field_names,
    reason = "matches the menu-action vocabulary in §6"
)]
pub struct TrayCallbacks {
    pub on_show: Arc<dyn Fn() + Send + Sync>,
    pub on_quit: Arc<dyn Fn() + Send + Sync>,
    pub on_pause_toggle: Arc<dyn Fn(bool) + Send + Sync>,
}

impl std::fmt::Debug for TrayCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrayCallbacks").finish_non_exhaustive()
    }
}

enum TrayCmd {
    SetPendingCount(i64),
    SetPaused(bool),
}

/// Handle to the tray thread. Drop it to ask the tray to shut down (the
/// thread also exits when its `cancel` token fires).
#[derive(Debug)]
pub struct TrayHandle {
    cmd_tx: mpsc::Sender<TrayCmd>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl TrayHandle {
    pub fn set_pending_count(&self, n: i64) {
        let _ignored = self.cmd_tx.send(TrayCmd::SetPendingCount(n));
    }
    pub fn set_paused(&self, paused: bool) {
        let _ignored = self.cmd_tx.send(TrayCmd::SetPaused(paused));
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        drop(self.cmd_tx.clone());
        if let Some(j) = self.join.take() {
            // Tray thread polls cancel every POLL_INTERVAL; ~100ms upper bound.
            j.join().ok();
        }
    }
}

/// Installs the tray icon + menu on a dedicated thread. Returns `None` if tray
/// init fails (e.g. no SNI host on Linux). Caller MUST keep the handle alive.
///
/// The tray thread owns `TrayIcon` + `MenuItem`s (all `!Send`) and processes
/// menu events + command-mpsc messages on its own loop.
pub fn install(
    db: Db,
    rt: tokio::runtime::Handle,
    bus_rx: Receiver<Event>,
    cancel: CancellationToken,
    callbacks: TrayCallbacks,
) -> Option<TrayHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<TrayCmd>();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<bool>(1);

    let cancel_for_thread = cancel.clone();
    let join = std::thread::Builder::new()
        .name("conduit-tray".into())
        .spawn(move || tray_thread(cmd_rx, cancel_for_thread, callbacks, ready_tx))
        .ok()?;

    if !matches!(ready_rx.recv_timeout(Duration::from_secs(2)), Ok(true)) {
        // Tray init failed inside the thread; it will exit on its own.
        let _ignored = join.join();
        return None;
    }

    spawn_badge_updater(db, rt, bus_rx, cancel, cmd_tx.clone());

    Some(TrayHandle {
        cmd_tx,
        join: Some(join),
    })
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "channels are intentionally moved into the thread"
)]
fn tray_thread(
    cmd_rx: mpsc::Receiver<TrayCmd>,
    cancel: CancellationToken,
    callbacks: TrayCallbacks,
    ready_tx: mpsc::SyncSender<bool>,
) {
    // Linux tray uses GTK (via muda) for menu construction. Must be init'd on
    // this thread before any TrayIcon / MenuItem is created.
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = gtk::init() {
            tracing::warn!(error = %e, "gtk::init failed; no tray icon");
            let _ignored = ready_tx.send(false);
            return;
        }
    }

    let icon_idle = build_icon(false);
    let icon_attention = build_icon(true);

    let menu = Menu::new();
    let show_item = MenuItem::new("Show window", true, None);
    let pending_item = MenuItem::new("Pending approvals (0)", false, None);
    let pause_item = MenuItem::new("Pause routing", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    if let Err(e) = menu.append_items(&[
        &show_item,
        &pending_item,
        &PredefinedMenuItem::separator(),
        &pause_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]) {
        tracing::warn!(error = %e, "tray: failed to append menu items");
        let _ignored = ready_tx.send(false);
        return;
    }

    let tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Conduit")
        .with_icon(icon_idle.clone())
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "tray init failed; continuing without tray (install xdotool on Linux)"
            );
            let _ignored = ready_tx.send(false);
            return;
        }
    };

    let _ignored = ready_tx.send(true);
    tracing::info!("tray icon installed");

    let show_id = show_item.id().clone();
    let pause_id = pause_item.id().clone();
    let quit_id = quit_item.id().clone();

    let last_count = AtomicI64::new(0);
    let mut paused_local = false;
    let menu_rx = MenuEvent::receiver();

    while !cancel.is_cancelled() {
        // Drain pending GTK events (menu/icon dispatch on Linux) without
        // blocking. With no events present `main_iteration_do(false)` returns
        // immediately, so the sleep below caps the busy loop.
        #[cfg(target_os = "linux")]
        {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
        }

        while let Ok(ev) = menu_rx.try_recv() {
            if ev.id == show_id {
                (callbacks.on_show)();
            } else if ev.id == pause_id {
                paused_local = !paused_local;
                update_pause_label(&pause_item, paused_local);
                (callbacks.on_pause_toggle)(paused_local);
            } else if ev.id == quit_id {
                (callbacks.on_quit)();
            }
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                TrayCmd::SetPendingCount(n) => {
                    apply_pending_count(
                        n,
                        &tray,
                        &pending_item,
                        &last_count,
                        &icon_idle,
                        &icon_attention,
                    );
                }
                TrayCmd::SetPaused(p) => {
                    paused_local = p;
                    update_pause_label(&pause_item, p);
                }
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    drop(tray);
    drop(pending_item);
    drop(pause_item);
    drop(show_item);
    drop(quit_item);
}

fn spawn_badge_updater(
    db: Db,
    rt: tokio::runtime::Handle,
    mut bus_rx: Receiver<Event>,
    cancel: CancellationToken,
    cmd_tx: mpsc::Sender<TrayCmd>,
) {
    rt.spawn(async move {
        // Initial sync.
        if let Ok(pending) = db.list_pending_approvals().await {
            let n = i64::try_from(pending.len()).unwrap_or(i64::MAX);
            let _ignored = cmd_tx.send(TrayCmd::SetPendingCount(n));
        }
        if let Ok(Some(v)) = db.get_settings_kv("routing_paused").await {
            let _ignored = cmd_tx.send(TrayCmd::SetPaused(v == "true"));
        }

        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                evt = bus_rx.recv() => match evt {
                    Ok(Event::ApprovalPending { .. } | Event::ApprovalResolved { .. }) => {
                        if let Ok(pending) = db.list_pending_approvals().await {
                            let n = i64::try_from(pending.len()).unwrap_or(i64::MAX);
                            if cmd_tx.send(TrayCmd::SetPendingCount(n)).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    });
}

fn apply_pending_count(
    n: i64,
    tray: &tray_icon::TrayIcon,
    pending_item: &MenuItem,
    last_count: &AtomicI64,
    icon_idle: &Icon,
    icon_attention: &Icon,
) {
    pending_item.set_text(format!("Pending approvals ({n})"));
    pending_item.set_enabled(n > 0);

    let prev = last_count.swap(n, Ordering::SeqCst);
    let crossed_threshold = (prev == 0) != (n == 0);
    if crossed_threshold {
        let icon = if n > 0 { icon_attention } else { icon_idle };
        if let Err(e) = tray.set_icon(Some(icon.clone())) {
            tracing::warn!(error = %e, "tray set_icon failed");
        }
    }
}

fn update_pause_label(pause_item: &MenuItem, paused: bool) {
    let label = if paused {
        "Resume routing"
    } else {
        "Pause routing"
    };
    pause_item.set_text(label);
}

/// Constructs an RGBA icon at 22×22 — a slate-grey rounded square with an
/// optional red attention dot in the top-right corner.
fn build_icon(attention: bool) -> Icon {
    let w = ICON_SIZE as usize;
    let h = ICON_SIZE as usize;
    let mut rgba = vec![0u8; w * h * 4];

    let glyph = [0xE8u8, 0xE8, 0xE8, 0xFF];
    let dot = [0xFFu8, 0x5C, 0x5C, 0xFF];

    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let i = (y * w + x) * 4;
            if let Some(slot) = rgba.get_mut(i..i + 4) {
                slot.copy_from_slice(&glyph);
            }
        }
    }

    if attention {
        // Unsigned arithmetic with absolute deltas — avoids signed-cast lints.
        let cx = w.saturating_sub(4);
        let cy = 4usize;
        let r = ATTENTION_DOT_RADIUS as usize;
        let r2 = r * r;
        for y in 0..h {
            for x in 0..w {
                let dx = x.max(cx) - x.min(cx);
                let dy = y.max(cy) - y.min(cy);
                if dx * dx + dy * dy <= r2 {
                    let i = (y * w + x) * 4;
                    if let Some(slot) = rgba.get_mut(i..i + 4) {
                        slot.copy_from_slice(&dot);
                    }
                }
            }
        }
    }

    match Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE) {
        Ok(icon) => icon,
        Err(e) => {
            tracing::error!(error = %e, "invalid tray icon RGBA buffer");
            // Fallback: 1×1 transparent so the tray doesn't die on this branch.
            Icon::from_rgba(vec![0u8; 4], 1, 1).unwrap_or_else(|_| {
                tracing::error!("1x1 fallback rejected — Icon API contract broken");
                std::process::abort();
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_and_attention_icons_build() {
        let _idle = build_icon(false);
        let _attn = build_icon(true);
    }

    #[test]
    fn pending_count_threshold_logic() {
        // 0 → N>0 must trigger a swap; N>0 → N>0 must not; N>0 → 0 must trigger.
        let last = AtomicI64::new(0);
        let prev = last.swap(3, Ordering::SeqCst);
        assert!((prev == 0) != (3 == 0));

        let prev = last.swap(5, Ordering::SeqCst);
        assert!((prev == 0) == (5 == 0));

        let prev = last.swap(0, Ordering::SeqCst);
        assert!((prev == 0) != (0 == 0));
    }
}
