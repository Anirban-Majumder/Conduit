//! GUI-side OS integration: tray icon, OS notifications, global hotkey.
//!
//! These pieces live outside the Slint event loop — each owns a dedicated
//! `std::thread` (tray/hotkey are `!Send`, notification backends block) and
//! talks to the daemon via the event bus and the DB actor. Spec §"Tray",
//! §"Approval UX", §"Open questions" (Wayland fallback).

pub mod hotkey;
pub mod notification_router;
pub mod notifications;
pub mod tray;
