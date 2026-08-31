//! The registry that turns individual [`Dependency`] implementations into a
//! single preflight check.

use std::sync::Arc;

use crate::core::config::{AppMode, ConfigStore};
use crate::core::deps::{
    Dependency, DependencyId, MpvDependency, PlayerChoice, PreflightItem, PreflightReport,
    SyncplayClientDependency,
};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{DependencyProgress, EventBus, ProgressSink};

pub struct DependencyManager {
    dependencies: Vec<Box<dyn Dependency>>,
    settings: Arc<ConfigStore>,
}

impl DependencyManager {
    /// The set syncparty ships with.
    pub fn standard(settings: Arc<ConfigStore>) -> Self {
        Self::with(
            vec![
                Box::new(SyncplayClientDependency::new(Arc::clone(&settings)))
                    as Box<dyn Dependency>,
                Box::new(MpvDependency::new(Arc::clone(&settings))),
            ],
            settings,
        )
    }

    pub fn with(dependencies: Vec<Box<dyn Dependency>>, settings: Arc<ConfigStore>) -> Self {
        Self {
            dependencies,
            settings,
        }
    }

    fn find(&self, id: DependencyId) -> Option<&dyn Dependency> {
        self.dependencies
            .iter()
            .map(AsRef::as_ref)
            .find(|dependency| dependency.id() == id)
    }

    /// Probes every dependency the mode needs.
    ///
    /// Detections run concurrently because each one spawns at least one
    /// process, and doing them in series is the difference between a preflight
    /// screen that appears instantly and one that visibly stalls.
    pub async fn preflight(&self, mode: AppMode) -> PreflightReport {
        let relevant = self
            .dependencies
            .iter()
            .filter(|dependency| dependency.required_for().applies_to(mode));

        let items = futures::future::join_all(relevant.map(|dependency| async move {
            PreflightItem {
                id: dependency.id(),
                display_name: dependency.display_name().to_owned(),
                status: dependency.detect().await,
                can_auto_install: dependency.can_auto_install().await,
                needs_elevation: dependency.needs_elevation(),
                manual_url: dependency.manual_url().to_owned(),
                supports_manual_path: dependency.supports_manual_path(),
                override_path: dependency
                    .manual_path_key()
                    .and_then(|key| self.settings.executable_override(key)),
            }
        }))
        .await;

        PreflightReport { mode, items }
    }

    /// Points a dependency at a program the user chose, or clears that choice.
    ///
    /// The new path is verified by re-detecting, and a path that does not
    /// actually yield a working program is rolled back rather than saved.
    /// Otherwise a typo would leave the dependency permanently broken with no
    /// obvious way back.
    pub async fn set_manual_path(&self, id: DependencyId, path: Option<String>) -> Result<()> {
        let dependency = self
            .find(id)
            .ok_or_else(|| SyncPartyError::Other(format!("unknown dependency: {id:?}")))?;

        let key = dependency.manual_path_key().ok_or_else(|| {
            SyncPartyError::Other(format!(
                "{} cannot be located by hand",
                dependency.display_name()
            ))
        })?;

        let previous = self.settings.executable_override(key);
        self.settings.set_executable_override(key, path.clone())?;

        if path.is_none() || dependency.detect().await.is_installed() {
            return Ok(());
        }

        self.settings.set_executable_override(key, previous)?;

        Err(SyncPartyError::DependencyMissing(format!(
            "{} was not found at that location",
            dependency.display_name()
        )))
    }

    /// Installs one dependency, streaming progress onto the event bus.
    pub async fn install(
        &self,
        id: DependencyId,
        choice: Option<PlayerChoice>,
        bus: &dyn EventBus,
    ) -> Result<()> {
        let progress = DependencyProgress::new(bus, id);
        self.install_with(id, choice, &progress).await
    }

    pub async fn install_with(
        &self,
        id: DependencyId,
        choice: Option<PlayerChoice>,
        progress: &dyn ProgressSink,
    ) -> Result<()> {
        let dependency = self
            .find(id)
            .ok_or_else(|| SyncPartyError::Other(format!("unknown dependency: {id:?}")))?;

        dependency.install(progress, choice).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::core::deps::{DependencyStatus, ModeRequirement};
    use crate::core::events::test_support::RecordingEventBus;
    use crate::core::events::AppEvent;
    use crate::core::paths::AppPaths;

    struct FakeDependency {
        id: DependencyId,
        requirement: ModeRequirement,
        status: DependencyStatus,
        manual_key: Option<&'static str>,
    }

    impl FakeDependency {
        fn installed(id: DependencyId, requirement: ModeRequirement) -> Self {
            Self {
                id,
                requirement,
                status: DependencyStatus::Installed {
                    version: None,
                    path: None,
                },
                manual_key: None,
            }
        }

        fn missing(id: DependencyId, requirement: ModeRequirement) -> Self {
            Self {
                id,
                requirement,
                status: DependencyStatus::Missing,
                manual_key: None,
            }
        }
    }

    #[async_trait]
    impl Dependency for FakeDependency {
        fn id(&self) -> DependencyId {
            self.id
        }

        fn display_name(&self) -> &str {
            "fake"
        }

        fn required_for(&self) -> ModeRequirement {
            self.requirement
        }

        async fn detect(&self) -> DependencyStatus {
            self.status.clone()
        }

        async fn install(
            &self,
            progress: &dyn ProgressSink,
            _choice: Option<PlayerChoice>,
        ) -> Result<()> {
            progress.report("installing", Some(50), None);
            Ok(())
        }

        fn manual_url(&self) -> &str {
            "https://example.com"
        }

        fn needs_elevation(&self) -> bool {
            false
        }

        async fn can_auto_install(&self) -> bool {
            true
        }

        fn supports_manual_path(&self) -> bool {
            self.manual_key.is_some()
        }

        fn manual_path_key(&self) -> Option<&'static str> {
            self.manual_key
        }
    }

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn settings(label: &str) -> Arc<ConfigStore> {
        // Rust tests in this module run in parallel. A shared directory made
        // one test delete another test's open settings file, which Windows
        // correctly rejects with AccessDenied.
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "syncparty-manager-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Arc::new(ConfigStore::load(AppPaths::rooted_at(dir)).expect("settings"))
    }

    fn manager() -> DependencyManager {
        manager_with(settings("default"))
    }

    fn manager_with(settings: Arc<ConfigStore>) -> DependencyManager {
        DependencyManager::with(
            vec![
                Box::new(FakeDependency::installed(
                    DependencyId::SyncplayClient,
                    ModeRequirement::Both,
                )) as Box<dyn Dependency>,
                // Host-only here purely to exercise the filter; what the real
                // player is required for is `mpv.rs`'s business, not this
                // module's.
                Box::new(FakeDependency::missing(
                    DependencyId::Mpv,
                    ModeRequirement::HostOnly,
                )),
            ],
            settings,
        )
    }

    #[tokio::test]
    async fn a_guest_is_not_asked_for_a_host_only_dependency() {
        let report = manager().preflight(AppMode::Guest).await;

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].id, DependencyId::SyncplayClient);
        assert!(report.is_satisfied());
    }

    #[tokio::test]
    async fn a_host_sees_what_it_is_missing() {
        let report = manager().preflight(AppMode::Host).await;

        assert_eq!(report.items.len(), 2);
        assert!(!report.is_satisfied());
        assert_eq!(
            report.missing().map(|item| item.id).collect::<Vec<_>>(),
            vec![DependencyId::Mpv]
        );
    }

    #[tokio::test]
    async fn installing_publishes_progress_for_that_dependency() {
        let bus = RecordingEventBus::default();

        manager()
            .install(DependencyId::SyncplayClient, None, &bus)
            .await
            .expect("install");

        let events = bus.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AppEvent::InstallProgress {
                dependency: DependencyId::SyncplayClient,
                percent: Some(50),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_dependency_that_cannot_be_located_by_hand_refuses_a_path() {
        let error = manager()
            .set_manual_path(DependencyId::Mpv, Some("/somewhere".to_owned()))
            .await
            .expect_err("a dependency with no manual path key refuses one");

        assert_eq!(error.kind(), "other");
    }

    #[tokio::test]
    async fn a_path_that_does_not_work_is_rolled_back() {
        let settings = settings("rollback");
        let manager = DependencyManager::with(
            vec![Box::new(FakeDependency {
                id: DependencyId::Mpv,
                requirement: ModeRequirement::Both,
                status: DependencyStatus::Missing,
                manual_key: Some("mpv"),
            }) as Box<dyn Dependency>],
            Arc::clone(&settings),
        );

        let error = manager
            .set_manual_path(DependencyId::Mpv, Some("/nowhere/mpv".to_owned()))
            .await
            .expect_err("detection still fails, so the path is no good");

        assert_eq!(error.kind(), "dependency_missing");
        assert_eq!(
            settings.executable_override("mpv"),
            None,
            "a path that does not work must not be left behind"
        );
    }

    #[tokio::test]
    async fn a_working_path_is_kept_and_reported_by_preflight() {
        let settings = settings("accepted");
        let manager = DependencyManager::with(
            vec![Box::new(FakeDependency {
                id: DependencyId::Mpv,
                requirement: ModeRequirement::Both,
                // Stands in for a real binary being found at the chosen path.
                status: DependencyStatus::Installed {
                    version: None,
                    path: None,
                },
                manual_key: Some("mpv"),
            }) as Box<dyn Dependency>],
            Arc::clone(&settings),
        );

        manager
            .set_manual_path(DependencyId::Mpv, Some("C:/portable/mpv.exe".to_owned()))
            .await
            .expect("accepted");

        let report = manager.preflight(AppMode::Guest).await;
        assert_eq!(
            report.items[0].override_path.as_deref(),
            Some("C:/portable/mpv.exe")
        );
        assert!(report.items[0].supports_manual_path);

        manager
            .set_manual_path(DependencyId::Mpv, None)
            .await
            .expect("cleared");
        assert_eq!(settings.executable_override("mpv"), None);
    }

    #[tokio::test]
    async fn installing_an_unknown_dependency_is_an_error_not_a_panic() {
        let bus = RecordingEventBus::default();
        // A manager that has never heard of the player, so asking it to
        // install one is a question it cannot answer.
        let manager = DependencyManager::with(
            vec![Box::new(FakeDependency::installed(
                DependencyId::SyncplayClient,
                ModeRequirement::Both,
            )) as Box<dyn Dependency>],
            settings("default"),
        );

        let error = manager
            .install(DependencyId::Mpv, None, &bus)
            .await
            .expect_err("unknown dependency");

        assert_eq!(error.kind(), "other");
    }

    /// The set a real host is asked for. Nothing about a party requires Python
    /// any more, and a preflight that still demanded it would block hosts on a
    /// runtime the server no longer uses.
    #[tokio::test]
    async fn a_host_needs_only_the_client_and_a_player() {
        let paths = AppPaths::rooted_at(std::env::temp_dir().join("syncparty-deps-test"));
        let settings = Arc::new(ConfigStore::load(paths).expect("settings"));

        let report = DependencyManager::standard(settings)
            .preflight(AppMode::Host)
            .await;
        let ids: Vec<_> = report.items.iter().map(|item| item.id).collect();

        assert_eq!(ids, vec![DependencyId::SyncplayClient, DependencyId::Mpv]);
    }
}
