//! ビルドのスクリーンショットを baseline と突き合わせて差分を計算するジョブ。
//!
//! `POST /v1/ci/builds/{id}/finalize` が `pending → processing` の遷移と同時に投入する。
//!
//! 処理の流れ:
//!
//! 1. build / project をロードし、[`service::baselines::latest_for`] で baseline を解決
//! 2. スクリーンショットと baseline エントリを **name で完全外部結合**し、
//!    片側だけに存在するものを `added` / `removed` にする
//! 3. 両側に存在するペアは PNG をストレージから読み、[`service::diff::diff_images`] を実行。
//!    `diff_ratio > project.diff_ratio_fail` なら `changed`、そうでなければ `unchanged`
//! 4. 差分ありのときだけ diff 画像をアップロード
//! 5. 集計を build に書き戻し、`passed`（差分ゼロ）か `changes_detected` に遷移
//!
//! リトライ安全性: 開始時にそのビルドの comparisons を全削除するため、
//! 途中で落ちて再実行されても行が重複しない。

use apalis::prelude::{BoxDynError, Data, TaskSink};
use apalis_postgres::{Config, PgPool, PostgresStorage};
use chrono::Utc;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, prelude::Uuid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use entity::{
    baseline_entries, builds, builds::BuildStatus, comparisons, comparisons::ComparisonStatus,
    projects, screenshots,
};
use service::build_logs::LogLevel;
use service::builds::BuildCounts;
use service::diff::{DiffOptions, diff_images};
use service::storage::StorageBackend;

use crate::JobState;

pub const QUEUE_NAME: &str = "compare_build";
pub const MAX_RETRIES: usize = 3;
/// ワーカーの同時実行数。diff は CPU バウンドなので控えめにする。
pub const WORKER_CONCURRENCY: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareBuildJob {
    pub build_id: Uuid,
}

/// `CompareBuildJob` のストレージ。
///
/// ## なぜ `PostgresStorage::new_with_notify` を使わないのか
///
/// apalis-postgres 1.0.0-rc.8 のストレージには 2 系統のフェッチャがある。
///
/// - `PgNotify`（`new_with_notify`）: `LISTEN apalis::job::insert` の**通知のみ**で駆動する。
///   ソースコメントいわく "A fetcher that does nothing, used for notify-based storage"。
///   定期ポーリングのフォールバックが一切無いため、**ワーカー起動直後に
///   `LISTEN` が張られるより先に投入されたジョブの通知は永久に失われる**
///   （テーブルには残るが誰も取りに来ない）。
/// - `PgFetcher`（既定 / `new_with_config`）: `apalis.get_jobs()` を定期実行する。
///   テーブルを毎回引き直すので、いつ投入されたジョブでも必ず拾える。
///
/// 実際に統合テストで前者を踏んだ（並列実行で負荷が上がると
/// `PgListener::connect_with` が finalize より遅れ、ビルドが processing のまま停止した）。
/// 取りこぼしゼロを優先してポーリング型を採用する。
///
/// ## 既知の制約（upstream）
///
/// `Config::with_poll_interval` で渡した `poll_strategy` は
/// apalis-postgres 1.0.0-rc.8 では**どちらのフェッチャからも読まれない**（デッドコンフィグ）。
/// `PgFetcher` の待ち時間は `PgPollFetcher` にハードコードされた指数バックオフ
/// （初期 1s → 2 倍ずつ → 上限 5 分。ジョブを 1 件でも拾えば 1s にリセット）で決まる。
/// つまり「約 8.5 分以上まったくジョブが無かった直後の 1 本目」は最大 5 分待たされうる。
/// 連続するビルドはリセット後なので即時に近い。
/// ここを詰めるには upstream が `poll_strategy` を尊重するか、
/// バックオフ上限を設定可能にする必要がある。
pub type CompareBuildStorage = PostgresStorage<CompareBuildJob>;

/// 指定キュー名でストレージを組み立てる。
///
/// キュー名を差し替えられるようにしてあるのは統合テストのため。
/// テストは 1 プロセス内で複数のアプリ（＝複数ワーカー）を並行に立ち上げ、
/// それぞれが自分の tokio ランタイム上でワーカーを回す。キューを共有すると
/// 「テスト A のワーカーがテスト B のジョブをロックしたままランタイムごと消える」
/// という取りこぼしが起き、`reenqueue_orphaned`（既定 30s）まで復旧しない。
pub fn build_storage_for_queue(pool: &PgPool, queue: &str) -> CompareBuildStorage {
    PostgresStorage::new_with_config(pool, &Config::new(queue))
}

pub fn build_storage(pool: &PgPool) -> CompareBuildStorage {
    build_storage_for_queue(pool, QUEUE_NAME)
}

/// apalis-postgres のジョブテーブルを作成してからストレージを返す。
pub async fn setup(pool: &PgPool) -> Result<Arc<CompareBuildStorage>, anyhow::Error> {
    setup_with_queue(pool, QUEUE_NAME).await
}

/// キュー名を指定してセットアップする（統合テスト用）。
pub async fn setup_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<CompareBuildStorage>, anyhow::Error> {
    // apalis のジョブテーブル作成はキューごとではなく DB 全体の操作なので、
    // プロセス内で 1 回に絞る（[`crate::ensure_apalis_schema`] 参照）。
    crate::ensure_apalis_schema(pool).await?;
    Ok(Arc::new(build_storage_for_queue(pool, queue)))
}

pub async fn enqueue(
    storage: &CompareBuildStorage,
    job: CompareBuildJob,
) -> Result<(), anyhow::Error> {
    let mut storage = storage.clone();
    storage
        .push(job)
        .await
        .map_err(|e| anyhow::anyhow!("push compare build job: {e}"))?;
    Ok(())
}

/// ワーカーのエントリポイント。
///
/// 回復不能なエラーはビルドを `failed` に落として `Ok(())` を返す（無限リトライ回避）。
/// ここで `Err` を返すのはビルド行にすら書き戻せなかったケースだけで、
/// その場合のみ apalis のリトライに委ねる。
pub async fn process(job: CompareBuildJob, state: Data<JobState>) -> Result<(), BoxDynError> {
    let build_id = job.build_id;
    let outcome = match run(build_id, &state).await {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::error!(%build_id, error = %err, "compare build job failed");
            // 失敗理由を成果物のログにも 1 行残す（UI/CI から追える）。
            service::build_logs::append(
                &state.db,
                build_id,
                LogLevel::Error,
                format!("compare failed: {}", truncate(&err.to_string(), 2000)),
            )
            .await
            .map_err(|e| -> BoxDynError { format!("append compare failure log: {e}").into() })?;
            let build = service::builds::get_build(&state.db, build_id)
                .await
                .map_err(|e| -> BoxDynError { format!("reload build {build_id}: {e}").into() })?;
            service::builds::mark_failed(&state.db, build, truncate(&err.to_string(), 2000))
                .await
                .map_err(|e| -> BoxDynError {
                    format!("mark build {build_id} failed: {e}").into()
                })?;
            Ok(())
        }
    };

    // ビルドが終端状態（passed / changes_detected / failed）に落ち着いたので、
    // GitHub のコミットステータスを更新する。紐付けが無い・App 未設定のときは
    // ジョブ側が何もせず終わるため、ここでは条件を見ない。
    crate::github_status::enqueue_best_effort(&state.github_status_storage, build_id).await;

    // 完了ビルドが増えたので、保持数の上限を超えた古いビルドを掃除する。
    // 失敗してもビルド完了自体は失敗させない（ベストエフォート）。
    if let Ok(build) = service::builds::get_build(&state.db, build_id).await {
        service::builds::prune_project_builds_best_effort(
            &state.db,
            &state.storage,
            build.project_id,
        )
        .await;
    }

    outcome
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

async fn run(build_id: Uuid, state: &JobState) -> Result<(), anyhow::Error> {
    let db = &state.db;

    let build = service::builds::get_build(db, build_id).await?;
    if build.status != BuildStatus::Processing {
        // finalize 済みでないビルドは処理しない（重複投入・遅延到着の保護）。
        tracing::info!(%build_id, status = ?build.status, "skipping compare job for non-processing build");
        return Ok(());
    }

    let project = service::projects::get_project(db, build.project_id).await?;

    // リトライ安全性: 前回の途中結果を捨ててからやり直す。
    service::comparisons::delete_for_build(db, build_id).await?;

    let baseline = service::baselines::latest_for(db, &project, &build.branch).await?;
    let baseline_entries = match &baseline {
        Some(b) => service::baselines::entries(db, b.id).await?,
        None => Vec::new(),
    };
    let shots = service::screenshots::list_for_build(db, build_id).await?;

    let pairs = join_by_name(shots, baseline_entries);
    let total = pairs.len();

    // 比較対象数が確定した時点で開始行を残す。
    service::build_logs::append(
        db,
        build_id,
        LogLevel::Info,
        format!("compare started: {total} comparisons"),
    )
    .await?;

    let mut counts = BuildCounts::default();
    let mut processed = 0usize;
    let now = Utc::now().fixed_offset();

    for (name, (shot, entry)) in pairs {
        counts.total += 1;

        let outcome = match (shot.as_ref(), entry.as_ref()) {
            (Some(_), None) => Outcome::added(),
            (None, Some(_)) => Outcome::removed(),
            (Some(shot), Some(entry)) => compare_pair(state, &project, &build, shot, entry).await?,
            // join のキーは必ずどちらかに由来するので到達しない。
            (None, None) => unreachable!("join key without any side"),
        };

        match outcome.status {
            ComparisonStatus::Added => counts.added += 1,
            ComparisonStatus::Removed => counts.removed += 1,
            ComparisonStatus::Changed => counts.changed += 1,
            ComparisonStatus::Unchanged => counts.unchanged += 1,
            _ => {}
        }

        comparisons::ActiveModel {
            id: Set(outcome.id),
            build_id: Set(build_id),
            name: Set(name),
            screenshot_id: Set(shot.as_ref().map(|s| s.id)),
            baseline_entry_id: Set(entry.as_ref().map(|e| e.id)),
            status: Set(outcome.status),
            review_status: Set(service::comparisons::initial_review_status(outcome.status)),
            diff_storage_key: Set(outcome.diff_storage_key),
            diff_pixel_count: Set(outcome.diff_pixel_count),
            diff_ratio: Set(outcome.diff_ratio),
            error_message: Set(None),
            reviewed_by: Set(None),
            reviewed_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;

        // 進捗は 10 件ごと + 最後の 1 件だけ残す（117 件で 117 行は過剰なため）。
        processed += 1;
        if processed.is_multiple_of(10) || processed == total {
            service::build_logs::append(
                db,
                build_id,
                LogLevel::Info,
                format!("compared {processed}/{total}"),
            )
            .await?;
        }
    }

    // 完了サマリ。内訳を 1 行で残す。
    service::build_logs::append(
        db,
        build_id,
        LogLevel::Info,
        format!(
            "compare complete: total {} changed {} added {} removed {} unchanged {}",
            counts.total, counts.changed, counts.added, counts.removed, counts.unchanged
        ),
    )
    .await?;

    let build =
        service::builds::apply_counts(db, build, counts, baseline.as_ref().map(|b| b.id)).await?;

    let next = if counts.has_differences() {
        BuildStatus::ChangesDetected
    } else {
        BuildStatus::Passed
    };
    let build = service::builds::transition(db, build, next).await?;

    tracing::info!(
        %build_id,
        number = build.number,
        status = ?build.status,
        total = counts.total,
        changed = counts.changed,
        added = counts.added,
        removed = counts.removed,
        unchanged = counts.unchanged,
        "compare build finished"
    );

    // GitHub のコミットステータス更新は [`process`] が（失敗経路も含めて）投入する。

    Ok(())
}

/// 1 比較ぶんの計算結果。
struct Outcome {
    id: Uuid,
    status: ComparisonStatus,
    diff_storage_key: Option<String>,
    diff_pixel_count: Option<i64>,
    diff_ratio: Option<f64>,
}

impl Outcome {
    fn added() -> Self {
        Self {
            id: Uuid::new_v4(),
            status: ComparisonStatus::Added,
            diff_storage_key: None,
            diff_pixel_count: None,
            diff_ratio: None,
        }
    }

    fn removed() -> Self {
        Self {
            id: Uuid::new_v4(),
            status: ComparisonStatus::Removed,
            diff_storage_key: None,
            diff_pixel_count: None,
            diff_ratio: None,
        }
    }
}

/// name をキーにスクリーンショットと baseline エントリを完全外部結合する。
fn join_by_name(
    shots: Vec<screenshots::Model>,
    entries: Vec<baseline_entries::Model>,
) -> BTreeMap<String, (Option<screenshots::Model>, Option<baseline_entries::Model>)> {
    let mut map: BTreeMap<String, (Option<screenshots::Model>, Option<baseline_entries::Model>)> =
        BTreeMap::new();
    for shot in shots {
        let name = shot.name.clone();
        map.entry(name).or_default().0 = Some(shot);
    }
    for entry in entries {
        let name = entry.name.clone();
        map.entry(name).or_default().1 = Some(entry);
    }
    map
}

/// baseline と今回のスクリーンショットのペアを比較する。
async fn compare_pair(
    state: &JobState,
    project: &projects::Model,
    build: &builds::Model,
    shot: &screenshots::Model,
    entry: &baseline_entries::Model,
) -> Result<Outcome, anyhow::Error> {
    let storage: &Arc<dyn StorageBackend> = &state.storage;

    let baseline_image = service::screenshots::load_rgba(storage, &entry.storage_key).await?;
    let current_image = service::screenshots::load_rgba(storage, &shot.storage_key).await?;

    let options = DiffOptions {
        threshold: project.diff_threshold,
        include_aa: false,
    };

    // diff は CPU バウンド。ワーカーのランタイムを塞がないよう blocking プールへ逃がす。
    let result = tokio::task::spawn_blocking(move || {
        let result = diff_images(&baseline_image, &current_image, &options);
        let encoded = service::screenshots::encode_png(&result.diff_image);
        (result.diff_pixel_count, result.diff_ratio, encoded)
    })
    .await
    .map_err(|e| anyhow::anyhow!("diff task join: {e}"))?;

    let (diff_pixel_count, diff_ratio, encoded) = result;
    let comparison_id = Uuid::new_v4();

    // diff_ratio_fail は「これを超えたら差分あり」。既定 0.0 なら 1px でも changed。
    let changed = diff_ratio > project.diff_ratio_fail;

    let diff_storage_key = if changed {
        let key =
            service::screenshots::diff_key(project.tenant_id, project.id, build.id, comparison_id);
        service::screenshots::upload_png(storage, &key, encoded?)
            .await
            .map_err(|e| anyhow::anyhow!("upload diff image: {e}"))?;
        Some(key)
    } else {
        None
    };

    Ok(Outcome {
        id: comparison_id,
        status: if changed {
            ComparisonStatus::Changed
        } else {
            ComparisonStatus::Unchanged
        },
        diff_storage_key,
        diff_pixel_count: Some(diff_pixel_count as i64),
        diff_ratio: Some(diff_ratio),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(name: &str) -> screenshots::Model {
        screenshots::Model {
            id: Uuid::new_v4(),
            build_id: Uuid::new_v4(),
            name: name.to_string(),
            storage_key: format!("key/{name}"),
            width: 1,
            height: 1,
            metadata: None,
            created_at: Utc::now().fixed_offset(),
        }
    }

    fn entry(name: &str) -> baseline_entries::Model {
        baseline_entries::Model {
            id: Uuid::new_v4(),
            baseline_id: Uuid::new_v4(),
            name: name.to_string(),
            storage_key: format!("baseline/{name}"),
            width: 1,
            height: 1,
        }
    }

    #[test]
    fn full_outer_join_marks_added_and_removed() {
        let joined = join_by_name(
            vec![shot("home"), shot("about")],
            vec![entry("home"), entry("legacy")],
        );

        assert_eq!(joined.len(), 3);
        // 両方にある
        assert!(joined["home"].0.is_some() && joined["home"].1.is_some());
        // 今回だけ → added
        assert!(joined["about"].0.is_some() && joined["about"].1.is_none());
        // baseline だけ → removed
        assert!(joined["legacy"].0.is_none() && joined["legacy"].1.is_some());
    }

    #[test]
    fn join_is_stable_and_sorted_by_name() {
        let joined = join_by_name(vec![shot("z"), shot("a"), shot("m")], vec![]);
        let names: Vec<_> = joined.keys().cloned().collect();
        assert_eq!(names, vec!["a", "m", "z"]);
    }

    #[test]
    fn truncate_limits_error_messages() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate(&"x".repeat(50), 10).len(), 10);
    }
}
