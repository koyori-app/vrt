use std::process::{Command, Output};

fn start_worker(retention_days: Option<&str>, app_url: Option<&str>) -> Output {
    // Isolate environment and .env loading; no database or queue may be touched.
    let directory =
        std::env::temp_dir().join(format!("vrt-worker-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join(".env"), "").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_vrt-worker"));
    command
        .env_clear()
        .current_dir(&directory)
        .env("DATABASE_URL", "not-a-database-url")
        .env("REDIS_URL", "not-a-redis-url");
    if let Some(value) = retention_days {
        command.env("STORAGE_MIN_RETENTION_DAYS", value);
    }
    if let Some(value) = app_url {
        command.env("APP_URL", value);
    }
    let output = command.output();
    std::fs::remove_dir_all(directory).unwrap();
    output.unwrap()
}

#[test]
fn invalid_retention_days_stop_worker_startup() {
    for value in ["90d", "-1", "4294967296"] {
        let output = start_worker(Some(value), Some("https://vrt.example.test"));
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("STORAGE_MIN_RETENTION_DAYS") && stderr.contains(value),
            "invalid retention {value} must be reported before other configuration or connections: {stderr}"
        );
    }
}

#[test]
fn valid_or_unset_retention_days_reach_the_next_configuration_check() {
    for value in [
        None,
        Some(""),
        Some("0"),
        Some("90"),
        Some(" 90 "),
        Some("4294967295"),
    ] {
        let output = start_worker(value, None);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("APP_URL is required"),
            "valid retention {value:?} must pass parsing: {stderr}"
        );
    }
}
