//! Accounts on the desktop: logging out, switching accounts, following
//! changes to the account list, and showing the administrator prompt.

use super::Desktop;
use crate::gui::account::Account;
use crate::gui::apps;
use alloc::boxed::Box;
use alloc::vec::Vec;

impl Desktop {
    /// Logs out: every process of the account ends, the system's own
    /// windows close, and Shore asks who is next.
    pub(super) fn log_out(&mut self) {
        let uid = crate::users::session().uid;
        let mut ended = 0;
        for p in crate::proc::all() {
            if p.exit_code().is_none() && p.cred.lock().uid == uid {
                let _ = crate::proc::kill(p.pid);
                ended += 1;
            }
        }
        let builtin: Vec<u32> = self.windows.iter().filter(|w| w.app.client_id().is_none()).map(|w| w.id).collect();
        for id in builtin {
            self.close(id);
        }
        log!("gui", "logged out uid {} ({} process(es) ended)", uid, ended);
        crate::fs::sync_all();
        self.close_menu();
        self.launcher = None;
        self.show_login();
    }

    /// True if the Aurora menu should offer to switch accounts.
    pub(super) fn several_accounts(&self) -> bool {
        crate::users::db::all().len() > 1
    }

    /// The account list changed (Settings → Users): refresh what shows it.
    pub(super) fn accounts_changed(&mut self) {
        let uid = crate::users::session().uid;
        // A changed role takes effect for apps started from now on.
        crate::users::set_session(uid);
        self.account = Account::get(uid).or_else(Account::load);
        self.damage_all();
    }

    /// Shows the administrator prompt for the oldest waiting request, and
    /// closes prompts whose request was answered or withdrawn.
    pub(super) fn sync_elevation(&mut self) {
        let pending = crate::users::elevate::pending();
        let stale: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.app.elevation().is_some_and(|id| pending.as_ref().is_none_or(|p| p.id != id)))
            .map(|w| w.id)
            .collect();
        for id in stale {
            self.close(id);
        }
        if let Some(req) = pending {
            match self.windows.iter().find(|w| w.app.elevation() == Some(req.id)).map(|w| w.id) {
                Some(id) => self.raise(id),
                None => {
                    self.add_window(Box::new(apps::ElevateDialog::new(req)));
                    crate::drivers::audio::play_sound("notify");
                }
            }
        }
    }

    /// Shows the privacy prompt for the oldest waiting question (see
    /// `sandbox::consent`), and closes prompts no longer waited for.
    pub(super) fn sync_consent(&mut self) {
        let pending = crate::sandbox::consent::pending();
        let stale: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.app.consent().is_some_and(|id| pending.as_ref().is_none_or(|p| p.id != id)))
            .map(|w| w.id)
            .collect();
        for id in stale {
            self.close(id);
        }
        if let Some(req) = pending {
            match self.windows.iter().find(|w| w.app.consent() == Some(req.id)).map(|w| w.id) {
                Some(id) => self.raise(id),
                None => {
                    self.add_window(Box::new(apps::ConsentDialog::new(req)));
                    crate::drivers::audio::play_sound("notify");
                }
            }
        }
    }

    /// Shows the open and save panels apps wait for (see `sandbox::panel`).
    pub(super) fn sync_panel(&mut self) {
        let pending = crate::sandbox::panel::pending();
        let stale: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.app.panel().is_some_and(|id| pending.as_ref().is_none_or(|p| p.id != id)))
            .map(|w| w.id)
            .collect();
        for id in stale {
            self.close(id);
        }
        if let Some(req) = pending {
            match self.windows.iter().find(|w| w.app.panel() == Some(req.id)).map(|w| w.id) {
                Some(id) => self.raise(id),
                None => self.add_window(Box::new(apps::PanelDialog::new(req))),
            }
        }
    }

    /// Waking from sleep locks the screen when the account has a password.
    pub(super) fn lock_after_sleep(&mut self) {
        if self.account.as_ref().is_some_and(|a| a.has_password) {
            self.lock();
        }
    }
}
