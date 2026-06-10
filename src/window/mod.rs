//! Binary-crate window layer: generated Slint bindings (`view`) + controller.
//!
//! `callbacks*` wire `window.on_*` handlers, `refresh` repaints UI models from
//! the DB, `wizard` drives the add-server flow, `skills` the Skills tab, `util`
//! holds shared helpers. The load-bearing rule throughout: callbacks run on the
//! Slint thread, real work is `rt.spawn`ed onto tokio, and UI mutation only ever
//! happens via `upgrade_in_event_loop`.

use std::sync::Arc;

use conduit::daemon::config::writer::ConfigWriter;
use conduit::daemon::config::SharedSettings;
use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::ApprovalCenter;
use conduit::daemon::events::Bus;
use conduit::daemon::skills::SkillManager;

pub(crate) mod view;

pub(crate) mod callbacks;
pub(crate) mod refresh;
pub(crate) mod skills;
pub(crate) mod util;
pub(crate) mod wizard;

pub(crate) use view::MainWindow;

/// Shared dependencies threaded into every `install_*_callbacks` function.
/// Adding a new dep here is a one-line change; callers don't need signature updates.
pub(crate) struct CallbackContext {
    pub(crate) db: Db,
    pub(crate) rt: tokio::runtime::Handle,
    pub(crate) skills: Arc<SkillManager>,
    pub(crate) approvals: Arc<ApprovalCenter>,
    pub(crate) bus: Bus,
    pub(crate) port: u16,
    pub(crate) config: ConfigWriter,
    pub(crate) settings: SharedSettings,
    pub(crate) config_toml_path: std::path::PathBuf,
}
