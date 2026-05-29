//! Top-level Slint callback wiring — fans out to the per-tab installers.
//!
//! Each `install_*_callbacks` registers `window.on_*` handlers for one tab.
//! Handlers run on the Slint (main) thread; any DB/network work is moved onto
//! the tokio worker via `rt.spawn`, and results flow back through
//! `upgrade_in_event_loop`. A callback declared in `.slint` but not wired here
//! is a silent no-op.

use std::rc::Rc;
use std::sync::Arc;

use slint::{ModelRc, VecModel};

use conduit::daemon::approval::ApprovalCenter;
use conduit::daemon::db::Db;
use conduit::daemon::events::Bus;
use conduit::daemon::skills::SkillManager;

use crate::ui::{EnvVarRow, MainWindow};

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
    // Shared, Rust-owned model backing the wizard's env-var/header rows. A single
    // VecModel identity (mutated in place) keeps TextInput focus stable while
    // typing; the wizard/servers/misc installers all edit this same handle.
    let env_rows: Rc<VecModel<EnvVarRow>> = Rc::new(VecModel::default());
    window.set_add_server_env_rows(ModelRc::from(env_rows.clone()));

    install_skills_callbacks(window, &rt, &skills);
    install_server_callbacks(window, db, approvals, bus, port, &rt);
    install_ide_callbacks(window, db, port, &rt);
    install_misc_callbacks(window, db, &rt, &env_rows);
    install_wizard_callbacks(window, db.clone(), bus.clone(), rt, &env_rows);
}
