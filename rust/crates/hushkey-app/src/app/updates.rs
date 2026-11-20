use super::{log, network, platform, update, App, Badge, Mode, Msg, Release, FAILURE_NOTICE, GREY};
use crate::config::Updates;
use crate::update::{Status, Version};
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct UpdateState {
    status: Status,
    offer: Option<Release>,
    /// Invalidates old replies even if the user switches channels and back.
    generation: u64,
    /// A manual request that joined a background check also receives feedback.
    asked: bool,
    announced: Option<Version>,
    /// Continuous listening may still be finishing its last utterance.
    restart: Option<PathBuf>,
}

impl UpdateState {
    pub(super) fn installing(&self) -> bool {
        matches!(self.status, Status::Installing { .. })
    }

    pub(super) fn policy_changed(&mut self, before: Updates, after: Updates) {
        if before.includes_previews() != after.includes_previews()
            || (self.status == Status::Checking && !after.automatic() && !self.asked)
        {
            self.status = Status::Idle;
            self.offer = None;
            self.announced = None;
        }
    }

    fn begin_check(&mut self, asked: bool) -> Option<u64> {
        if self.status == Status::Checking {
            self.asked |= asked;
            return None;
        }
        if self.installing() {
            return None;
        }
        self.generation += 1;
        self.asked = asked;
        self.status = Status::Checking;
        Some(self.generation)
    }

    fn finish_check(&mut self, id: u64, result: Result<Option<Release>, String>) -> bool {
        if self.status != Status::Checking || self.generation != id {
            return false;
        }
        match result {
            Ok(release) => self.offer = release,
            // An automatic refresh can fail transiently. Keep the existing
            // offer until a successful check supersedes it.
            Err(e) if !self.asked && self.offer.is_some() => {
                log::line(&format!("update check: {e}"));
            }
            Err(e) => {
                log::line(&format!("update check: {e}"));
                self.offer = None;
                self.status = Status::Failed(e);
                return true;
            }
        }
        self.status = match &self.offer {
            Some(r) => Status::Available {
                version: r.version.to_string(),
                preview: r.preview,
            },
            None => Status::UpToDate,
        };
        true
    }

    pub(super) fn offered_version(&self) -> Option<String> {
        match &self.status {
            Status::Available { version, .. } => Some(version.clone()),
            _ => None,
        }
    }

    fn start_install(&mut self, version: &str) -> Option<Release> {
        if !matches!(&self.status, Status::Available { version: offered, .. } if offered == version)
        {
            return None;
        }
        let release = self.offer.take()?;
        self.status = Status::Installing {
            version: version.into(),
        };
        Some(release)
    }
}

impl App {
    pub(super) fn update_status(&self) -> Status {
        self.updates.status.clone()
    }

    pub(super) fn check_update(&mut self, asked: bool) {
        let Some(id) = self.updates.begin_check(asked) else {
            return;
        };
        self.tray.offer_update(None);
        if !platform::setup::updates_itself() {
            self.updates.status = Status::Managed;
            if asked && self.can_show_update_notice() {
                self.notice(
                    GREY,
                    network::words(
                        self.s,
                        "Updates über die Paketverwaltung installieren.",
                        "Use your package manager to update Hush-Key.",
                    ),
                );
            }
            return;
        }
        let pre = self.config.updates.includes_previews();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateChecked(id, update::check(pre)));
        });
    }

    pub(super) fn update_checked(&mut self, id: u64, result: Result<Option<Release>, String>) {
        if !self.updates.finish_check(id, result) {
            return;
        }
        let version = self.updates.offered_version();
        self.tray.offer_update(version.as_deref());
        if let Some(v) = version {
            log::line(&format!("update available: v{v}"));
        } else if self.updates.asked && self.can_show_update_notice() {
            match self.updates.status {
                Status::UpToDate => self.notice(GREY, self.s.up_to_date),
                Status::Failed(_) => self.notice(GREY, self.s.update_failed),
                _ => {}
            }
        }
    }

    fn can_show_update_notice(&self) -> bool {
        self.recording.is_none()
            && !self.hearing
            && !self.busy
            && self.notice_until.is_none()
            && !(self.config.guide && !self.guide_away && self.waiting())
    }

    pub(super) fn notify_available_update(&mut self) {
        let Some(release) = &self.updates.offer else {
            return;
        };
        if !matches!(self.updates.status, Status::Available { .. })
            || self.updates.announced.as_ref() == Some(&release.version)
            || !self.can_show_update_notice()
        {
            return;
        }
        let label = network::words(self.s, "Update verfügbar", "Update available");
        let text = format!("{label} · v{}", release.version);
        self.updates.announced = Some(release.version.clone());
        self.notice_for(GREY, &text, FAILURE_NOTICE);
    }

    pub(super) fn install_offered_update(&mut self, version: &str) {
        if self.updates.offered_version().as_deref() != Some(version) || !self.can_switch() {
            return;
        }
        if self.hearing || self.draining.is_some() || self.pending_inserts > 0 {
            self.failure_notice(network::words(
                self.s,
                "Das Diktat wird noch verarbeitet. Bitte danach das Update installieren.",
                "A dictation is still finishing. Install the update once it is ready.",
            ));
            return;
        }
        let Some(release) = self.updates.start_install(version) else {
            return;
        };
        // Pause continuous capture, preserving its preference across restart
        // and draining any utterance already queued on the worker.
        self.stop_listening(true);
        log::line(&format!("installing the update to v{}", release.version));
        self.tray.offer_update(None);
        self.notice_until = None;
        self.overlay.show(GREY, self.s.update_running);
        self.tray.show(Badge::Loading, self.s.update_running);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::UpdateInstalled(update::install(&release)));
        });
    }

    pub(super) fn update_installed(&mut self, result: Result<Option<PathBuf>, String>) {
        match result {
            Ok(Some(exe)) => {
                self.updates.restart = Some(exe);
                return;
            }
            Ok(None) => {
                log::line("update cancelled at the password dialog");
                self.updates.status = Status::Cancelled;
                self.notice(GREY, self.s.update_cancelled);
            }
            Err(e) => {
                log::line(&format!("update failed: {e}"));
                let text = if e.starts_with(crate::setup::PACKAGE_BUSY) {
                    self.s.package_busy
                } else {
                    self.s.update_failed
                };
                self.updates.status = Status::Failed(e);
                self.failure_notice(text);
            }
        }
        self.idle();
        if self.config.mode == Mode::Listen {
            self.set_listening(true);
        }
    }

    pub(super) fn restart_after_update(&mut self) {
        if self.draining.is_some() || self.pending_inserts > 0 {
            return;
        }
        if let Some(exe) = self.updates.restart.take() {
            log::line(&format!("update in place, restarting: {}", exe.display()));
            match platform::setup::launch(&exe, &["--after-update"]) {
                Ok(()) => self.quit_requested = true,
                Err(e) => self.update_installed(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        let json = serde_json::json!([{
            "tag_name": "v0.14.0-beta.1",
            "assets": [
                {"name": "app", "browser_download_url": "app"},
                {"name": "SHA256SUMS", "browser_download_url": "sums"}
            ]
        }]);
        update::newest(&json, |name| name == "app", true).unwrap()
    }

    #[test]
    fn manual_checks_join_work_and_stale_replies_cannot_restore_a_preview() {
        let mut state = UpdateState::default();
        let original = state.begin_check(false).unwrap();
        assert_eq!(state.begin_check(true), None);
        assert!(state.asked);
        state.policy_changed(Updates::Pre, Updates::Auto);
        state.policy_changed(Updates::Auto, Updates::Pre);
        assert!(!state.finish_check(original, Ok(Some(release()))));
        let current = state.begin_check(false).unwrap();
        assert!(!state.finish_check(original, Ok(Some(release()))));
        assert_eq!(state.status, Status::Checking);
        assert!(state.finish_check(current, Ok(None)));
        assert_eq!(state.status, Status::UpToDate);
        assert_eq!(state.offered_version(), None);
    }

    #[test]
    fn installing_requires_the_displayed_offer_and_runs_once() {
        let mut state = UpdateState::default();
        let id = state.begin_check(false).unwrap();
        assert!(state.finish_check(id, Ok(Some(release()))));
        let version = state.offered_version().unwrap();
        assert!(state.start_install("0.13.1").is_none());
        assert_eq!(state.start_install(&version), Some(release()));
        assert!(state.installing());
        assert!(state.start_install(&version).is_none());
        assert!(state.begin_check(true).is_none());
        assert!(!state.finish_check(id, Ok(None)));
    }

    #[test]
    fn disabling_scheduled_checks_keeps_explicit_checks_but_drops_automatic_results() {
        for asked in [true, false] {
            let mut state = UpdateState::default();
            let id = state.begin_check(asked).unwrap();
            state.policy_changed(Updates::Pre, Updates::ManualPre);
            assert_eq!(state.finish_check(id, Ok(Some(release()))), asked);
            assert_eq!(state.offered_version().is_some(), asked);
            state.policy_changed(Updates::ManualPre, Updates::Off);
            assert!(state.start_install("0.14.0-beta.1").is_none());
        }
    }

    #[test]
    fn automatic_refresh_failure_preserves_the_offer_and_manual_errors_are_displayed() {
        let mut state = UpdateState::default();
        let id = state.begin_check(true).unwrap();
        state.finish_check(id, Ok(Some(release())));
        let offered = state.status.clone();
        let id = state.begin_check(false).unwrap();
        state.finish_check(id, Err("offline".into()));
        assert_eq!(state.status, offered);
        let id = state.begin_check(true).unwrap();
        state.finish_check(id, Err("offline".into()));
        assert_eq!(state.status, Status::Failed("offline".into()));
        assert_eq!(state.offered_version(), None);
    }
}
