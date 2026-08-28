//! ワーカーの生存監視まわりの統合テスト。
//!
//! 判定そのものは純関数のユニットテストで固定してあるので、ここでは
//! 本物の Postgres に対して SQL が意図どおり動くことだけを見る。

mod common;

use std::time::Duration;

use common::TestApp;
use job::liveness::{DEFAULT_STALE_AFTER, WatchedWorker, observe, queue_health, stale_workers};
use reqwest::StatusCode;

/// TestApp はワーカーを起動するので、そのキューは「消費者がいる」状態で見える。
#[tokio::test(flavor = "multi_thread")]
async fn queue_health_shows_the_running_workers() {
    let app = TestApp::new().await;

    let queues = queue_health(&app.state.pg_pool, DEFAULT_STALE_AFTER)
        .await
        .expect("read queue health");

    let compare = queues
        .iter()
        .find(|q| q.queue.starts_with("compare_build"))
        .expect("compare_build queue is registered");

    assert!(
        compare.live_workers >= 1,
        "a running worker must be counted: {compare:?}"
    );
    let age = compare
        .newest_heartbeat_age
        .expect("a registered worker has a heartbeat");
    assert!(
        age < DEFAULT_STALE_AFTER,
        "a just-started worker must not look stale: {age:?}"
    );
}

/// 登録されていないワーカー ID は「行が無い」として観測され、猶予外では異常になる。
/// ここを取り違えると、起動に失敗したワーカーを永久に見逃す。
#[tokio::test(flavor = "multi_thread")]
async fn an_unregistered_worker_is_observed_as_missing() {
    let app = TestApp::new().await;

    let watched = vec![WatchedWorker {
        queue: "compare_build".to_string(),
        worker_id: "compare_build-worker-never-registered".to_string(),
    }];

    let observed = observe(&app.state.pg_pool, &watched)
        .await
        .expect("observe heartbeats");
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].1, None,
        "an unknown worker has no heartbeat row"
    );

    let observations: Vec<_> = observed
        .iter()
        .map(|(id, age)| job::liveness::Observation {
            worker_id: id,
            age: *age,
        })
        .collect();
    assert!(stale_workers(&observations, DEFAULT_STALE_AFTER, true).is_empty());
    assert_eq!(
        stale_workers(&observations, DEFAULT_STALE_AFTER, false).len(),
        1
    );
}

/// `/health/queues` は認証なしで読めて、集計値だけを返す。
#[tokio::test(flavor = "multi_thread")]
async fn queues_endpoint_is_public_and_hides_worker_ids() {
    let app = TestApp::new().await;

    let response = app.get("/health/queues").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("queues json");

    let queues = body["queues"].as_array().expect("queues array");
    assert!(!queues.is_empty(), "the test app runs workers: {body}");

    for queue in queues {
        assert!(queue["queue"].is_string());
        assert!(queue["live_workers"].is_i64());
        assert!(queue["waiting_jobs"].is_i64());
    }

    // ワーカー ID（`<queue>-worker-<uuid>`）が漏れていないこと。
    let raw = body.to_string();
    assert!(
        !raw.contains("-worker-"),
        "worker ids must not be exposed: {raw}"
    );
}

/// 待ちジョブが無いキューでは待ち時間が null になる（0 と取り違えない）。
#[tokio::test(flavor = "multi_thread")]
async fn an_idle_queue_reports_no_wait() {
    let app = TestApp::new().await;

    let queues = queue_health(&app.state.pg_pool, Duration::from_secs(180))
        .await
        .expect("read queue health");
    let compare = queues
        .iter()
        .find(|q| q.queue.starts_with("compare_build"))
        .expect("compare_build queue is registered");

    assert_eq!(compare.waiting_jobs, 0);
    assert_eq!(compare.oldest_wait, None);
}
