//! Top-level Slint callback wiring — fans out to the per-tab installers.
//!
//! Each `install_*_callbacks` registers `window.on_*` handlers for one tab.
//! Handlers run on the Slint (main) thread; any DB/network work is moved onto
//! the tokio worker via `rt.spawn`, and results flow back through
//! `upgrade_in_event_loop`. A callback declared in `.slint` but not wired here
//! is a silent no-op.

mod ide;
mod log;
mod misc;
mod scope;
mod servers;

use std::rc::Rc;
use std::sync::Arc;

use slint::{ModelRc, VecModel};

use conduit::daemon::db::Db;
use conduit::daemon::downstream::approval::ApprovalCenter;
use conduit::daemon::events::Bus;
use conduit::daemon::skills::SkillManager;

use crate::window::view::{EnvVarRow, MainWindow};

use crate::window::callbacks::ide::install_ide_callbacks;
use crate::window::callbacks::log::install_log_callbacks;
use crate::window::callbacks::misc::install_misc_callbacks;
use crate::window::callbacks::scope::install_scope_callbacks;
use crate::window::callbacks::servers::install_server_callbacks;
use crate::window::skills::install_skills_callbacks;
use crate::window::wizard::install_wizard_callbacks;
use crate::window::CallbackContext;

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

    let ctx = CallbackContext {
        db: db.clone(),
        rt,
        skills,
        approvals: approvals.clone(),
        bus: bus.clone(),
        port,
    };

    install_skills_callbacks(window, &ctx);
    install_server_callbacks(window, &ctx);
    install_scope_callbacks(window, &ctx);
    install_ide_callbacks(window, &ctx);
    install_misc_callbacks(window, &ctx, &env_rows);
    install_wizard_callbacks(window, &ctx, &env_rows);
    install_log_callbacks(window, &ctx);
}
