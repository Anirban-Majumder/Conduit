//! Log-tab Slint callback wiring: search, decision filter, and copy-args.
//!
//! Each callback sets the corresponding UI property and triggers a filter-aware
//! `refresh_audit` so the displayed rows always match the live filter state.

use slint::ComponentHandle;

use crate::window::refresh::refresh_audit;
use crate::window::util::copy_to_clipboard;
use crate::window::view::MainWindow;
use crate::window::CallbackContext;

pub(crate) fn install_log_callbacks(window: &MainWindow, ctx: &CallbackContext) {
    let weak = window.as_weak();
    let db = ctx.db.clone();
    let rt = ctx.rt.clone();

    window.on_audit_search_changed({
        let weak = weak.clone();
        let db = db.clone();
        let rt = rt.clone();
        move |_query| {
            let weak = weak.clone();
            let db = db.clone();
            rt.spawn(async move {
                refresh_audit(&weak, &db).await;
            });
        }
    });

    window.on_audit_decision_filter_changed(move |_decision| {
        let weak = weak.clone();
        let db = db.clone();
        rt.spawn(async move {
            refresh_audit(&weak, &db).await;
        });
    });

    window.on_copy_audit_args(move |text| {
        if !text.is_empty() {
            copy_to_clipboard(&text);
        }
    });
}
