//! Settings › Updates and the header's "Update ready" button. StreamIt asks GitHub once
//! at start and whenever Check now is pressed; it installs only on a press, and only
//! between services. The new version runs from the next start.
use super::{Pending, StreamItApp};
use crate::update::{self, Release};
use crate::view;
use std::path::PathBuf;
use streamit_core::engine::Snapshot;
use tracing::{info, warn};

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum UpdateState {
    #[default]
    Checking,
    UpToDate,
    CheckFailed(String),
    Available(Release),
    Installing(Release),
    InstallFailed(Release, String),
    /// Installed; the new version starts from `exe`.
    Installed(Release, PathBuf),
}

/// What the Updates card shows: a sentence and the one button that applies.
#[derive(Debug, PartialEq)]
pub(super) struct UpdateCard {
    pub text: String,
    pub action: Option<UpdateAction>,
    /// The action is offered but must wait for the service to finish.
    pub blocked: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum UpdateAction {
    Check,
    Install,
    Restart,
}

pub(super) fn update_card(state: &UpdateState, snap: &Snapshot) -> UpdateCard {
    let current = update::CURRENT;
    let between = view::between_services(snap);
    let card =
        |text: String, action, gated: bool| UpdateCard { text, action: Some(action), blocked: gated && !between };
    match state {
        UpdateState::Checking => {
            UpdateCard { text: format!("Version {current}. Checking for updates…"), action: None, blocked: false }
        }
        UpdateState::UpToDate => card(format!("Version {current} is the latest."), UpdateAction::Check, false),
        UpdateState::CheckFailed(e) => {
            card(format!("Version {current}. Could not check for updates: {e}"), UpdateAction::Check, false)
        }
        UpdateState::Available(r) => card(
            format!("Version {} is ready to install (you have {current}).", r.version),
            UpdateAction::Install,
            true,
        ),
        UpdateState::Installing(r) => {
            UpdateCard { text: format!("Installing version {}…", r.version), action: None, blocked: false }
        }
        UpdateState::InstallFailed(r, e) => {
            card(format!("Version {} could not be installed: {e}", r.version), UpdateAction::Install, true)
        }
        UpdateState::Installed(r, _) => card(
            format!("Version {} is installed. Restart StreamIt to use it.", r.version),
            UpdateAction::Restart,
            true,
        ),
    }
}

/// The header shows a button while an update waits to be installed or used.
pub(super) fn header_hint(state: &UpdateState) -> Option<&'static str> {
    match state {
        UpdateState::Available(_) | UpdateState::InstallFailed(..) => Some("Update ready"),
        UpdateState::Installed(..) => Some("Restart to update"),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Updates {
    pub state: UpdateState,
    check: Pending<Result<Option<Release>, String>>,
    install: Pending<Result<PathBuf, String>>,
}

impl StreamItApp {
    pub(super) fn check_for_update(&mut self) {
        self.updates.state = UpdateState::Checking;
        self.updates.check.start(&self.rt, &self.ctx, update::check());
    }

    pub(super) fn poll_updates(&mut self) {
        if let Some(r) = self.updates.check.take() {
            self.updates.state = match r {
                Ok(Some(r)) => {
                    info!(version = %r.version, "an update is available");
                    UpdateState::Available(r)
                }
                Ok(None) => UpdateState::UpToDate,
                Err(e) => {
                    warn!("update check: {e}");
                    UpdateState::CheckFailed(e)
                }
            };
        }
        if let Some(r) = self.updates.install.take()
            && let UpdateState::Installing(release) = &self.updates.state
        {
            let release = release.clone();
            self.updates.state = match r {
                Ok(exe) => {
                    info!(version = %release.version, "update installed");
                    UpdateState::Installed(release, exe)
                }
                Err(e) => {
                    warn!("update install: {e}");
                    UpdateState::InstallFailed(release, e)
                }
            };
        }
    }

    /// Runs the card's button. Install and Restart re-check that no service is running.
    pub(super) fn update_action(&mut self, action: UpdateAction, snap: &Snapshot) {
        if action != UpdateAction::Check && !view::between_services(snap) {
            return;
        }
        match (action, self.updates.state.clone()) {
            (UpdateAction::Check, _) => self.check_for_update(),
            (UpdateAction::Install, UpdateState::Available(r) | UpdateState::InstallFailed(r, _)) => {
                info!(version = %r.version, "installing update");
                self.updates.state = UpdateState::Installing(r.clone());
                self.updates.install.start(&self.rt, &self.ctx, update::install(r));
            }
            (UpdateAction::Restart, UpdateState::Installed(..)) if self.settings_dirty() => {
                self.notice = Some("Save or discard your settings changes first, then restart.".into());
            }
            (UpdateAction::Restart, UpdateState::Installed(r, exe)) => match update::start(&exe) {
                Ok(()) => {
                    info!(version = %r.version, "restarting into the update");
                    self.close_confirmed = true;
                    self.ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                }
                Err(e) => {
                    self.notice = Some(format!("Could not start the new version: {e}. Close and reopen StreamIt."))
                }
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;
    use streamit_core::golive::Stage;

    fn release() -> Release {
        let json = r#"{"tag_name":"v99.0.0","html_url":"u","assets":[{"name":"A","browser_download_url":"d"}]}"#;
        update::newer(json, "1.0.0", "A").unwrap().unwrap()
    }

    #[test]
    fn install_and_restart_wait_for_the_service_to_finish() {
        let idle = Snapshot::default();
        let live = Snapshot { stage: Stage::Live, ..Snapshot::default() };
        let busy = Snapshot { job: Some("Prepare"), ..Snapshot::default() };
        for state in [UpdateState::Available(release()), UpdateState::Installed(release(), PathBuf::from("s.exe"))] {
            assert!(!update_card(&state, &idle).blocked);
            assert!(update_card(&state, &live).blocked);
            assert!(update_card(&state, &busy).blocked);
        }
        // Checking never changes anything, so it is always allowed.
        assert!(!update_card(&UpdateState::UpToDate, &live).blocked);
    }

    #[test]
    fn the_card_offers_the_next_step() {
        let idle = Snapshot::default();
        let card = update_card(&UpdateState::Available(release()), &idle);
        assert_eq!(card.action, Some(UpdateAction::Install));
        assert!(card.text.contains("99.0.0"), "{}", card.text);
        assert_eq!(update_card(&UpdateState::Checking, &idle).action, None);
        assert_eq!(update_card(&UpdateState::Installing(release()), &idle).action, None);
        assert_eq!(update_card(&UpdateState::CheckFailed("offline".into()), &idle).action, Some(UpdateAction::Check));
        let failed = update_card(&UpdateState::InstallFailed(release(), "checksum".into()), &idle);
        assert_eq!(failed.action, Some(UpdateAction::Install), "a failed install can be tried again");
        assert_eq!(release().version, Version::new(99, 0, 0));
    }

    #[test]
    fn the_header_hints_only_when_there_is_something_to_do() {
        assert_eq!(header_hint(&UpdateState::UpToDate), None);
        assert_eq!(header_hint(&UpdateState::CheckFailed("x".into())), None);
        assert_eq!(header_hint(&UpdateState::Available(release())), Some("Update ready"));
        assert_eq!(header_hint(&UpdateState::Installed(release(), PathBuf::new())), Some("Restart to update"));
    }
}
