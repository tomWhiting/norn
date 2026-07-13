use std::sync::Arc;

use super::*;

#[tokio::test]
#[serial_test::serial]
async fn cancellation_before_adoption_commit_kills_the_process()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let cwd = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(home.path().as_os_str()))], async {
        let manager = Arc::new(ProcessManager::new(Some("guard-fixture".to_owned()), None));
        let handle = manager.spawn("sleep 30", cwd.path(), None).await?;
        let private_fs_permit = DescriptorGovernor::global()?
            .acquire(crate::resource::PRIVATE_FS_OPERATION_PEAK)
            .await?;
        let adoption = PendingAdoption::new(handle.clone(), private_fs_permit);
        let pending = tokio::spawn(async move {
            let _adoption = adoption;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        pending.abort();
        let join = pending.await;
        if join.is_ok() {
            return Err(
                std::io::Error::other("cancellation fixture unexpectedly completed").into(),
            );
        }

        assert_eq!(handle.status(), ProcessStatus::Killed);
        manager.shutdown();
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}
