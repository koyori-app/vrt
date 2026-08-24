//! ビルドのスクリーンショットを baseline と突き合わせて差分を計算するジョブ。
//!
//! `POST /v1/ci/builds/{id}/finalize` が `pending → queued` の遷移と同時に投入し、
//! screenshots モードでは worker が取得した時点で `queued → processing` へ進める。
//!
//! 処理の流れ:
//!
//! 1. build / project をロードし、baseline を解決する（部分撮影が固定した
//!    `builds.baseline_id` があればそれ、無ければ [`service::baselines::latest_for`]）
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
use std::collections::{BTreeMap, HashSet};
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
/// `PgListener::connect_with` が finalize より遅れ、ビルドが queued のまま停止した）。
/// 取りこぼしゼロを優先してポーリング型を採用する。
///
/// ## ポーリング間隔
///
/// upstream の apalis-postgres 1.0.0-rc.8 は `Config::with_poll_interval` を
/// `PgPollFetcher` から読まず、アイドル時に最大 5 分まで指数バックオフする。
/// VRT は同じ API / DB スキーマのローカルパッチ（`vendor/apalis-postgres`）で
/// 1 秒固定にしている。これにより通知型の起動レースを持ち込まず、プロセス起動前に
/// 投入されたジョブも拾いつつ、アイドル後の取得遅延を 1 秒程度に抑える。
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
            service::builds::mark_failed(
                &state.db,
                build,
                truncate(&err.to_string(), 2000),
                entity::builds::BuildFailureOrigin::Vrt,
                "compare_internal",
            )
            .await
            .map_err(|e| -> BoxDynError { format!("mark build {build_id} failed: {e}").into() })?;
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
    let build = match (build.mode, build.status) {
        // screenshots モードのパイプライン先頭。worker が実際に取得してから
        // processing にするため、UI はキュー待ちと処理中を区別できる。
        (builds::BuildMode::Screenshots, BuildStatus::Queued) => {
            service::builds::transition(db, build, BuildStatus::Processing).await?
        }
        // storybook の render 完了後に積まれた compare job と、旧バージョンが
        // finalize 時点で Processing にしたジョブはそのまま続行する。
        (_, BuildStatus::Processing) => build,
        (mode, status) => {
            tracing::info!(%build_id, ?mode, ?status, "skipping compare job outside its processing phase");
            return Ok(());
        }
    };

    let project = service::projects::get_project(db, build.project_id).await?;

    // リトライ安全性: 前回の途中結果を捨ててからやり直す。
    service::comparisons::delete_for_build(db, build_id).await?;

    // baseline の解決は 2 系統ある。
    //
    // - 部分撮影（capture plan / only_story_ids）が固定した `builds.baseline_id` が
    //   あればそれを使う。ここで最新を引き直すと、計画〜比較の間に別ビルドが
    //   承認された場合にクライアントが計画した baseline と違うものと比較してしまう。
    // - 固定が無ければ従来どおり比較時点の最新を解決する（全撮影ビルド。
    //   作成が古くても最新 baseline と比較できる——他ブランチの fallback を含む）。
    let baseline = match build.baseline_id {
        Some(id) => Some(service::baselines::get_baseline(db, id).await?),
        None => service::baselines::latest_for(db, &project, &build.branch).await?,
    };
    let baseline_entries = match &baseline {
        Some(b) => service::baselines::entries(db, b.id).await?,
        None => Vec::new(),
    };
    let shots = service::screenshots::list_for_build(db, build_id).await?;

    // screenshots モードの部分アップロード: 撮影前に保存された capture plan の
    // 選択集合の外にある baseline エントリは「今回撮らなかった」だけで削除ではない。
    // baseline の PNG をこのビルドのスクリーンショットとして複製してから通常の
    // 比較に入れる（unchanged になり、承認時も全スクリーンショット昇格の経路が
    // そのまま新 baseline へ引き継ぐ。storybook モードの only_story_ids 流用と
    // 同じ帰結）。ただし plan の manifest（現行 index）から消えた名前は複製せず、
    // 完全外部結合で `removed` として報告させる——流用で削除を隠さない。
    let shots = match service::builds::capture_plan(&build)
        .map_err(|e| anyhow::anyhow!("read capture plan: {e}"))?
    {
        Some(plan) => {
            materialize_carry_forward(state, &project, &build, &baseline_entries, &plan, shots)
                .await?
        }
        None => shots,
    };

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
            (Some(shot), Some(entry)) => {
                let outcome = compare_pair(state, &project, &build, shot, entry).await?;
                if outcome.content_hash_skipped {
                    counts.content_hash_skipped += 1;
                }
                outcome
            }
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
            "compare complete: total {} changed {} added {} removed {} unchanged {} content_hash_skipped {}",
            counts.total, counts.changed, counts.added, counts.removed, counts.unchanged,
            counts.content_hash_skipped
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
        content_hash_skipped = counts.content_hash_skipped,
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
    content_hash_skipped: bool,
}

impl Outcome {
    fn added() -> Self {
        Self {
            id: Uuid::new_v4(),
            status: ComparisonStatus::Added,
            diff_storage_key: None,
            diff_pixel_count: None,
            diff_ratio: None,
            content_hash_skipped: false,
        }
    }

    fn removed() -> Self {
        Self {
            id: Uuid::new_v4(),
            status: ComparisonStatus::Removed,
            diff_storage_key: None,
            diff_pixel_count: None,
            diff_ratio: None,
            content_hash_skipped: false,
        }
    }
}

/// スクリーンショットが baseline 流用の複製（`metadata.reused == true`）か。
fn is_reused(shot: &screenshots::Model) -> bool {
    shot.metadata
        .as_ref()
        .and_then(|m| m.get("reused"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 一時的でありうる失敗（ストレージ IO 等）を短いバックオフ付きでやり直す。
///
/// carry-forward は 1 エントリごとに download → upload → insert の 3 段で、
/// どの段の一時失敗も従来はそのままビルドの failed 直行だった（[`process`] は
/// `run` の Err を mark_failed に落とす）。ジョブ全体を apalis のリトライへ
/// 返す設計にすると、リトライ枯渇時に mark_failed を通らずビルドが
/// processing のまま宙吊りになるため、リトライは**ジョブ内**で行い、
/// 使い切ったときだけ従来どおり failed へ落とす。
async fn with_transient_retries<T, F, Fut>(what: &str, mut f: F) -> Result<T, anyhow::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
{
    const ATTEMPTS: u64 = 3;
    let mut last: Option<anyhow::Error> = None;
    for attempt in 1..=ATTEMPTS {
        match f().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                if attempt < ATTEMPTS {
                    tracing::warn!(attempt, what, error = %e, "carry-forward step failed; retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempt)).await;
                }
                last = Some(e);
            }
        }
    }
    Err(last
        .expect("at least one attempt ran")
        .context(format!("{what} (after {ATTEMPTS} attempts)")))
}

/// 計画の選択集合の外にある baseline エントリを、このビルドのスクリーンショットとして複製する。
///
/// finalize 時点の「計画 == アップロード」検証をここでも再確認する。finalize の
/// 検証と遷移の間に紛れ込んだアップロードや、計画に無い名前の混入を、比較を
/// 始める前に落とすための多重防御（流用の複製は `reused` メタデータで除外して数える）。
///
/// 複製するのは「選択外かつ現行 manifest に残っている」名前だけ。manifest から
/// 消えた名前（story の削除）は複製せず、完全外部結合が `removed` として報告する。
/// ここで無差別に複製すると、削除された story が unchanged に化けて永久に
/// baseline へ残り続ける。
///
/// ## 物理複製を選ぶ理由（baseline 参照の流用にしない）
///
/// baseline エントリの `storage_key` を今回のショット行から直接参照すれば
/// DB トランザクションだけで完結するが、それは**ビルドをまたいだキー共有**を
/// 新設することになる。`prune_old_builds` は「ビルドのオブジェクトはビルドと
/// 共に死ぬ」を前提に、baseline の参照元ビルドだけを保護してストレージを
/// 削除する——古い参照元ビルドが保護から外れた時点で、それを参照し続ける
/// 新しいビルドのショット（と、その承認で昇格した新 baseline のエントリ）が
/// ぶら下がりになる。参照方式には参照カウント相当の寿命管理の作り直しが要り、
/// 本 PR の範囲を超えるため、複製で「キーはビルド内に閉じる」性質を保つ。
///
/// ## 非原子性への対処（決定的キー + upsert + リトライ + 補償削除）
///
/// download → upload → insert は原子的にできない（ストレージは DB
/// トランザクションに参加しない）。代わりに:
///
/// - スクリーンショット ID を `(build_id, name)` から決定的に導出する
///   （[`service::screenshots::carry_forward_screenshot_id`]）。upload と insert の
///   間で落ちても、再実行が**同じキー**へ上書き保存して行を挿すため、
///   孤児オブジェクトは再実行で自然に回収される
/// - insert は `(build_id, name)` UNIQUE への `ON CONFLICT DO NOTHING`。
///   再実行・並行実行が重複行エラーで落ちない
/// - 各段は [`with_transient_retries`] で短いリトライを持ち、一時的な
///   ストレージ/DB の失敗が即ビルド failed に直行しない
/// - insert がリトライ後も失敗し、かつ行が存在しないと確認できた場合だけ、
///   アップロード済みオブジェクトを補償削除する（行が確認できないときは
///   消さない——既存行が参照するオブジェクトを壊さない側に倒す）
async fn materialize_carry_forward(
    state: &JobState,
    project: &projects::Model,
    build: &builds::Model,
    baseline_entries: &[baseline_entries::Model],
    plan: &service::builds::CapturePlan,
    shots: Vec<screenshots::Model>,
) -> Result<Vec<screenshots::Model>, anyhow::Error> {
    use sea_orm::sea_query::OnConflict;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let db = &state.db;

    let selected = plan.selected_set();
    let manifest = plan.manifest_set();

    let uploaded: HashSet<&str> = shots
        .iter()
        .filter(|s| !is_reused(s))
        .map(|s| s.name.as_str())
        .collect();
    if uploaded != selected {
        let mut missing: Vec<&&str> = selected.difference(&uploaded).collect();
        let mut extra: Vec<&&str> = uploaded.difference(&selected).collect();
        missing.sort();
        extra.sort();
        anyhow::bail!(
            "the uploaded screenshots do not match the capture plan \
             (planned but not uploaded: {missing:?}; uploaded but not planned: {extra:?})"
        );
    }

    let existing: HashSet<String> = shots.iter().map(|s| s.name.clone()).collect();
    let mut carried = 0usize;
    let mut vanished = 0usize;
    for entry in baseline_entries {
        if selected.contains(entry.name.as_str()) || existing.contains(&entry.name) {
            continue;
        }
        if !manifest.contains(entry.name.as_str()) {
            // 現行 index に存在しない = story が消えた。流用せず removed に倒す。
            vanished += 1;
            continue;
        }

        // baseline の PNG バイト列をそのまま今回のスクリーンショットとして保存する。
        // バイト列が同一なので、後段の比較が unchanged と判定する
        // （render_build.rs の Reuse 経路と同じ手口）。寸法は baseline エントリの
        // 記録値を引き継ぐ（バイト列同一なので再デコード検証は不要）。
        let screenshot_id =
            service::screenshots::carry_forward_screenshot_id(build.id, &entry.name);
        let key = service::screenshots::screenshot_key(
            project.tenant_id,
            project.id,
            build.id,
            screenshot_id,
        );

        let png = with_transient_retries("download baseline PNG", || async {
            service::screenshots::read_all(&state.storage, &entry.storage_key)
                .await
                .map_err(|e| anyhow::anyhow!("download baseline for `{}`: {e}", entry.name))
        })
        .await?;
        let png = bytes::Bytes::from(png);

        with_transient_retries("upload carried-forward copy", || {
            let png = png.clone();
            let key = key.clone();
            async move {
                service::screenshots::upload_png(&state.storage, &key, png)
                    .await
                    .map_err(|e| anyhow::anyhow!("upload carried-forward `{}`: {e}", entry.name))
            }
        })
        .await?;

        let active = screenshots::ActiveModel {
            id: sea_orm::ActiveValue::Set(screenshot_id),
            build_id: sea_orm::ActiveValue::Set(build.id),
            name: sea_orm::ActiveValue::Set(entry.name.clone()),
            storage_key: sea_orm::ActiveValue::Set(key.clone()),
            width: sea_orm::ActiveValue::Set(entry.width),
            height: sea_orm::ActiveValue::Set(entry.height),
            // carry-forward は上で読み込んだ PNG バイト列を無加工でコピーする。
            // migration 前の baseline は hash が NULL なので、継承ではなく、この既読
            // バイト列から再計算する。追加の storage 読み出しは発生しない。
            content_hash: sea_orm::ActiveValue::Set(Some(service::screenshots::content_hash(&png))),
            metadata: sea_orm::ActiveValue::Set(Some(serde_json::json!({ "reused": true }))),
            created_at: sea_orm::ActiveValue::Set(Utc::now().fixed_offset()),
        };
        let inserted = with_transient_retries("insert carried-forward row", || {
            let active = active.clone();
            async move {
                screenshots::Entity::insert(active)
                    .on_conflict(
                        OnConflict::columns([
                            screenshots::Column::BuildId,
                            screenshots::Column::Name,
                        ])
                        .do_nothing()
                        .to_owned(),
                    )
                    .exec_without_returning(db)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("store carried-forward screenshot `{}`: {e}", entry.name)
                    })
            }
        })
        .await;

        if let Err(insert_err) = inserted {
            // 行が確実に存在しない場合だけ、アップロード済みオブジェクトを補償削除する。
            // 存在確認自体に失敗したら消さない（既存行の実体を壊す方が高くつく）。
            let row_absent = screenshots::Entity::find()
                .filter(screenshots::Column::BuildId.eq(build.id))
                .filter(screenshots::Column::Name.eq(entry.name.clone()))
                .one(db)
                .await
                .map(|row| row.is_none())
                .unwrap_or(false);
            if row_absent && let Err(delete_err) = state.storage.delete(&key).await {
                tracing::warn!(
                    build_id = %build.id,
                    key = %key,
                    error = %delete_err,
                    "failed to delete an orphaned carried-forward object"
                );
            }
            return Err(insert_err);
        }
        carried += 1;
    }

    if carried > 0 {
        service::build_logs::append(
            db,
            build.id,
            LogLevel::Info,
            format!("carried forward {carried} baseline screenshots outside the planned set"),
        )
        .await?;
    }
    if vanished > 0 {
        service::build_logs::append(
            db,
            build.id,
            LogLevel::Info,
            format!(
                "{vanished} baseline entr{} no longer in the story manifest; reporting as removed",
                if vanished == 1 { "y is" } else { "ies are" }
            ),
        )
        .await?;
    }

    // 複製ぶんを含めた最新の一覧で比較する。
    Ok(service::screenshots::list_for_build(db, build.id).await?)
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

    // marker は昇格時点の hash 再照合と full decode の成功だけを証明する。
    // その後の欠損・破損は証明しないため、fast path の直前に baseline/current の
    // 保存実体を再読して hash を照合する。読めない場合は比較ジョブを失敗させる。
    let hashes_match = service::screenshots::content_hashes_match(
        shot.content_hash.as_deref(),
        entry.content_hash.as_deref(),
    ) && service::screenshots::content_hashes_match(
        entry.content_hash.as_deref(),
        entry.verified_content_hash.as_deref(),
    );
    if hashes_match {
        let baseline_ok = service::screenshots::verify_stored_content_hash(
            storage,
            &entry.storage_key,
            entry.content_hash.as_deref(),
        )
        .await?;
        let current_ok = service::screenshots::verify_stored_content_hash(
            storage,
            &shot.storage_key,
            shot.content_hash.as_deref(),
        )
        .await?;
        if baseline_ok && current_ok {
            return Ok(Outcome {
                id: Uuid::new_v4(),
                status: ComparisonStatus::Unchanged,
                diff_storage_key: None,
                diff_pixel_count: Some(0),
                diff_ratio: Some(0.0),
                content_hash_skipped: true,
            });
        }
        if !baseline_ok {
            anyhow::bail!(
                "baseline entry `{}` integrity check failed: \
                 stored content does not match recorded content hash",
                entry.name
            );
        }
        anyhow::bail!(
            "screenshot `{}` integrity check failed: \
             stored content does not match recorded content hash",
            shot.name
        );
    }

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
        content_hash_skipped: false,
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
            content_hash: None,
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
            content_hash: None,
            verified_content_hash: None,
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
