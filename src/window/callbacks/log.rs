//! Log-tab Slint callback wiring: search, decision filter, and copy-args.
//!
//! Each callback sets the corresponding UI property and triggers a filter-aware
//! `refresh_audit` so the displayed rows always match the live filter state.
//! A generation counter drops out-of-order DB responses so stale results never
//! overwrite a fresher paint.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use slint::ComponentHandle;

use crate::window::refresh::refresh_audit_gen;
use crate::window::util::copy_to_clipboard;
use crate::window::view::MainWindow;
use crate::window::CallbackContext;

pub(crate) fn install_log_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let weak = window.as_weak();
    let db = ctx.db.clone();
    let rt = ctx.rt.clone();

    // Monotonic counter: each search/filter change increments it; each spawned
    // query captures the value at spawn time and the UI write is skipped if a
    // newer generation has been issued by the time the result arrives.
    let gen = Arc::new(AtomicU64::new(0));

    window.on_audit_search_changed({
        let weak = weak.clone();
        let db = db.clone();
        let rt = rt.clone();
        let gen = gen.clone();
        move |_query| {
            let my_gen = gen.fetch_add(1, Ordering::Relaxed) + 1;
            let weak = weak.clone();
            let db = db.clone();
            let gen = gen.clone();
            rt.spawn(async move {
                refresh_audit_gen(&weak, &db, my_gen, &gen).await;
            });
        }
    });

    window.on_audit_decision_filter_changed(move |_decision| {
        let my_gen = gen.fetch_add(1, Ordering::Relaxed) + 1;
        let weak = weak.clone();
        let db = db.clone();
        let gen = gen.clone();
        rt.spawn(async move {
            refresh_audit_gen(&weak, &db, my_gen, &gen).await;
        });
    });

    window.on_copy_audit_args(move |text| {
        if !text.is_empty() {
            copy_to_clipboard(&text);
        }
    });
}
