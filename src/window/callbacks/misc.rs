//! Miscellaneous Slint callbacks: paste parsing.
//!
//! Covers the add-server JSON paste parser.
//! Settings-tab edits live in `callbacks::settings`. Like the other callback
//! modules, UI writes go through the Slint event loop.

use std::rc::Rc;

use slint::{ComponentHandle, SharedString, VecModel};

use conduit::daemon::wizard_input;

use crate::window::view::{EnvVarRow, MainWindow};
use crate::window::wizard::set_env_rows;
use crate::window::CallbackContext;

pub(crate) fn install_misc_callbacks(
    window: &MainWindow,
    _ctx: &CallbackContext,
    env_rows: &Rc<VecModel<EnvVarRow>>,
) {
    // The Args field doubles as a paste-to-autofill input: when it holds a full
    // MCP JSON config (starts with '{' or '['), parse it and fill the form;
    // otherwise it's just plain arguments and we leave it alone.
    let weak_for_paste = window.as_weak();
    let env_for_paste = env_rows.clone();
    window.on_parse_paste(move || {
        let Some(ui) = weak_for_paste.upgrade() else {
            return;
        };
        let raw = ui.get_add_server_args().to_string();
        let trimmed = raw.trim_start();
        if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
            return; // plain args, not a config paste
        }
        let Ok(parsed) = wizard_input::parse(&raw) else {
            return; // looks like JSON but didn't parse — wait for more input
        };
        let transport = parsed.inferred_transport();
        ui.set_add_server_transport(SharedString::from(transport));
        if let Some(n) = parsed.name {
            ui.set_add_server_name(SharedString::from(n));
        }
        if let Some(c) = parsed.command {
            ui.set_add_server_command(SharedString::from(c));
        }
        ui.set_add_server_args(SharedString::from(parsed.args.join(" ")));
        if let Some(u) = parsed.url {
            ui.set_add_server_url(SharedString::from(u));
        }
        let pairs: Vec<(String, String)> = parsed
            .headers
            .iter()
            .map(|h| (h.name.clone(), h.value.clone()))
            .collect();
        if !pairs.is_empty() {
            set_env_rows(&env_for_paste, &pairs);
        }
        if ui.get_add_server_status_is_error() {
            ui.set_add_server_status(SharedString::from(""));
            ui.set_add_server_status_is_error(false);
        }
    });
}
