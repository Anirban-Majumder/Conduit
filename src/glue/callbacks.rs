//! Top-level Slint callback wiring — fans out to the per-tab installers.
//!
//! Each `install_*_callbacks` registers `window.on_*` handlers for one tab.
//! Handlers run on the Slint (main) thread; any DB/network work is moved onto
//! the tokio worker via `rt.spawn`, and results flow back through
//! `upgrade_in_event_loop`. A callback declared in `.slint` but not wired here
//! is a silent no-op.

use std::sync::Arc;

use conduit::daemon::approval::ApprovalCenter;
use conduit::daemon::db::Db;
use conduit::daemon::events::Bus;
use conduit::daemon::skills::SkillManager;

use crate::ui::MainWindow;

use crate::glue::callbacks_ide::install_ide_callbacks;
use crate::glue::callbacks_misc::install_misc_callbacks;
use crate::glue::callbacks_servers::install_server_callbacks;
use crate::glue::skills::install_skills_callbacks;
use crate::glue::wizard::install_wizard_callbacks;

pub(crate) fn install_callbacks(
    window: &MainWindow,
    db: &Db,
    approvals: &Arc<ApprovalCenter>,
    bus: &Bus,
    port: u16,
    rt: tokio::runtime::Handle,
    skills: Arc<SkillManager>,
) {
    install_skills_callbacks(window, &rt, &skills);
    install_server_callbacks(window, db, approvals, bus, port, &rt);
    install_ide_callbacks(window, db, port, &rt);
    install_misc_callbacks(window, db, &rt);
    install_wizard_callbacks(window, db.clone(), bus.clone(), rt);
}
