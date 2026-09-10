use std::process::{Command, Output};

fn start_binary(binary: &str, variables: &[(&str, &str)]) -> Output {
    // Isolate environment and .env loading; no database or queue may be touched.
    let directory =
        std::env::temp_dir().join(format!("vrt-worker-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join(".env"), "").unwrap();
    let mut command = Command::new(binary);
    command.env_clear().current_dir(&directory);
    for (name, value) in variables {
        command.env(name, value);
    }
    let output = command.output();
    std::fs::remove_dir_all(directory).unwrap();
    output.unwrap()
}

fn start_worker(retention_days: Option<&str>, app_url: Option<&str>) -> Output {
    let mut variables = vec![
        ("DATABASE_URL", "not-a-database-url"),
        ("REDIS_URL", "not-a-redis-url"),
    ];
    if let Some(value) = retention_days {
        variables.push(("STORAGE_MIN_RETENTION_DAYS", value));
    }
    if let Some(value) = app_url {
        variables.push(("APP_URL", value));
    }
    start_binary(env!("CARGO_BIN_EXE_vrt-worker"), &variables)
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

#[test]
fn invalid_app_url_stops_worker_startup() {
    // API の Settings と同じ検証を通す。scheme 無しの値を通すと commit status の
    // target_url が不正なまま GitHub へ送られる。
    for value in ["vrt.example.test", "http:/vrt.example.test", "ftp://vrt.example.test"] {
        let output = start_worker(None, Some(value));
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("APP_URL") && stderr.contains(value),
            "invalid APP_URL {value} must stop the worker before connections: {stderr}"
        );
    }
}

#[test]
fn invalid_heartbeat_staleness_stops_worker_and_runner_startup() {
    for value in ["0", "59", "180s", "3m"] {
        let worker = start_binary(
            env!("CARGO_BIN_EXE_vrt-worker"),
            &[
                ("DATABASE_URL", "not-a-database-url"),
                ("REDIS_URL", "not-a-redis-url"),
                ("APP_URL", "https://vrt.example.test"),
                ("WORKER_HEARTBEAT_STALE_SECS", value),
            ],
        );
        let runner = start_binary(
            env!("CARGO_BIN_EXE_vrt-runner"),
            &[
                ("DATABASE_URL", "not-a-database-url"),
                ("CHROMIUM_PATH", "/not/a/chromium"),
                ("WORKER_HEARTBEAT_STALE_SECS", value),
            ],
        );

        for (binary, output) in [("vrt-worker", worker), ("vrt-runner", runner)] {
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("WORKER_HEARTBEAT_STALE_SECS") && stderr.contains(value),
                "invalid heartbeat threshold {value} must stop {binary} before connections: {stderr}"
            );
        }
    }
}
