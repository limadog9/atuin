//! Project synced script records into the local script database.

use atuin_client::record::sqlite_store::SqliteStore;
use atuin_client::settings::Settings;
use atuin_common::encryption::paseto_v4::Key;
use atuin_domain::record::{HostId, RecordTag};
use atuin_scripts::database::Database;
use atuin_scripts::store::ScriptStore;
use eyre::Result;
use tokio::sync::Mutex;

/// Avoid replaying an unchanged append-only script log on every sync tick.
#[derive(Default)]
pub(super) struct ScriptProjector {
    last_build: Mutex<Option<(String, u64)>>,
}

impl ScriptProjector {
    pub(super) async fn build(
        &self,
        settings: &Settings,
        store: &SqliteStore,
        host_id: HostId,
        key: &Key,
    ) -> Result<()> {
        let mut last_build = self.last_build.lock().await;
        let count = store.len_tag(&RecordTag::Script).await?;
        if count == 0 && last_build.is_none()
            || last_build
                .as_ref()
                .is_some_and(|(path, n)| path == &settings.scripts.db_path && *n == count)
        {
            return Ok(());
        }

        let database = Database::new(&settings.scripts.db_path, settings.local_timeout).await?;
        let scripts = ScriptStore::new(store.clone(), host_id, key.clone());
        scripts.build(database).await?;

        // Leave the marker unchanged on failure so the next tick retries without a new download.
        *last_build = Some((settings.scripts.db_path.clone(), count));
        drop(last_build);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use atuin_scripts::store::script::Script;
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    use super::*;

    struct Env {
        dir: TempDir,
        settings: Settings,
        store: ScriptStore,
        projector: ScriptProjector,
    }

    impl Env {
        async fn project(&self) -> Result<()> {
            self.projector
                .build(
                    &self.settings,
                    &self.store.store,
                    self.store.host_id,
                    &self.store.encryption_key,
                )
                .await
        }

        async fn database(&self) -> Database {
            Database::new(&self.settings.scripts.db_path, Duration::from_secs(5)).await.unwrap()
        }
    }

    #[fixture]
    async fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::utc();
        settings.scripts.db_path = dir.path().join("scripts.db").to_string_lossy().into_owned();
        let store = ScriptStore::new(
            SqliteStore::in_memory(Duration::from_secs(5)).await.unwrap(),
            HostId(atuin_common::utils::uuid_v7()),
            Key::generate(),
        );
        Env {
            dir,
            settings,
            store,
            projector: ScriptProjector::default(),
        }
    }

    #[fixture]
    fn script() -> Script {
        Script::builder()
            .name("test".into())
            .script("echo original".into())
            .tags(vec!["old".into()])
            .build()
    }

    #[rstest]
    #[tokio::test]
    async fn synced_script_lifecycle(
        #[future(awt)] env: Env,
        mut script: Script,
        #[values(false, true)] rename: bool,
    ) {
        env.store.create(script.clone()).await.unwrap();
        env.project().await.unwrap();
        let db = env.database().await;
        assert_eq!(db.get_by_name(&script.name).await.unwrap(), Some(script.clone()));

        script.script = "echo changed".into();
        script.tags = vec!["new".into()];
        if rename {
            script.name = "renamed".into();
        }
        env.store.update(script.clone()).await.unwrap();
        env.project().await.unwrap();
        assert_eq!(db.get_by_name(&script.name).await.unwrap(), Some(script.clone()));
        if rename {
            assert!(db.get_by_name("test").await.unwrap().is_none());
        }

        env.store.delete(script.id).await.unwrap();
        env.project().await.unwrap();
        assert!(db.list().await.unwrap().is_empty());
    }

    #[rstest]
    #[tokio::test]
    async fn failed_build_is_retried_without_new_records(#[future(awt)] env: Env, script: Script) {
        env.store.create(script.clone()).await.unwrap();
        std::fs::write(&env.settings.scripts.db_path, "not a database").unwrap();
        assert!(env.project().await.is_err());
        std::fs::remove_file(&env.settings.scripts.db_path).unwrap();
        env.project().await.unwrap();
        assert_eq!(env.database().await.get_by_name(&script.name).await.unwrap(), Some(script));
    }

    #[rstest]
    #[tokio::test]
    async fn a_new_database_path_is_projected_without_new_records(
        #[future(awt)] mut env: Env,
        script: Script,
    ) {
        env.store.create(script.clone()).await.unwrap();
        env.project().await.unwrap();
        env.settings.scripts.db_path =
            env.dir.path().join("other-scripts.db").to_string_lossy().into_owned();
        env.project().await.unwrap();
        assert_eq!(env.database().await.get_by_name(&script.name).await.unwrap(), Some(script));
    }

    #[rstest]
    #[tokio::test]
    async fn history_only_sync_does_not_create_a_script_database(#[future(awt)] env: Env) {
        env.project().await.unwrap();
        assert!(!Path::new(&env.settings.scripts.db_path).exists());
    }

    #[rstest]
    #[tokio::test]
    async fn restart_recovers_stale_projection(#[future(awt)] mut env: Env, mut script: Script) {
        env.store.create(script.clone()).await.unwrap();
        env.project().await.unwrap();
        script.script = "echo downloaded before restart".into();
        env.store.update(script.clone()).await.unwrap();
        env.projector = ScriptProjector::default();
        env.project().await.unwrap();
        assert_eq!(env.database().await.get_by_name(&script.name).await.unwrap(), Some(script));
    }
}
