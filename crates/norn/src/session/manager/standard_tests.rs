use super::SessionManager;

#[test]
#[serial_test::serial]
fn standard_manager_uses_checked_session_store() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    temp_env::with_var("NORN_HOME", Some(temp.path().as_os_str()), || {
        let manager = SessionManager::standard()?;
        assert_eq!(manager.data_dir(), temp.path().join("session-store"));
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
