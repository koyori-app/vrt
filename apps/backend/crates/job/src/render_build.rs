//! アップロードされた Storybook バンドルをヘッドレス Chromium で撮影するジョブ。
//!
//! `POST /v1/ci/builds/{id}/finalize`（`mode = storybook`）が
//! `pending → rendering` の遷移と同時に投入する。
//!
//! 処理の流れ:
//!
//! 1. build / project をロードし、`rendering` でなければ何もせず終わる（重複投入の保護）
//! 2. `builds.storybook_key` の zip をストレージから読み、一時ディレクトリへ安全に展開
//!    （[`service::render::bundle`] が zip-slip / zip bomb / symlink を弾く）
//! 3. `index.json` からストーリー一覧を作り、ループバックの静的サーバーを立てる
//! 4. ストーリーを**逐次**レンダリングして PNG を `screenshots` に保存
//!    （name は `{title}/{name}`、metadata に `{story_id, title}`）
//! 5. `rendering → processing` に遷移し、`CompareBuildJob` を投入して既存の比較経路へ繋ぐ
//!
//! リトライ安全性: 開始時にそのビルドのスクリーンショット行を全削除するため、
//! 途中で落ちて再実行されても `(build_id, name)` の UNIQUE 制約にぶつからない。

use apalis::prelude::{BoxDynError, Data, TaskSink};
use apalis_postgres::{Config, PgPool, PostgresStorage};
use sea_orm::prelude::Uuid;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use entity::{baseline_entries, builds::BuildMode, builds::BuildStatus, screenshots};
use service::build_logs::LogLevel;
use service::render::{RenderError, RenderOptions, StaticServer, StoryRenderer};

use crate::JobState;

pub const QUEUE_NAME: &str = "render_build";
pub const MAX_RETRIES: usize = 2;
/// ワーカーの同時実行数。1 ジョブがブラウザ 1 個を丸ごと持つので控えめにする。
pub const WORKER_CONCURRENCY: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderBuildJob {
    pub build_id: Uuid,
    /// 撮影が必要なストーリー ID のリスト（TurboSnap 相当のスキップ制御）。
    ///
    /// `None` のときは全ストーリーを撮影する（従来どおり）。`Some` のときは
    /// ここに無いストーリーを baseline から流用する。
    /// `#[serde(default)]` はキューに残る旧ジョブ（このフィールドを持たない
    /// JSON）との後方互換のため必須。
    #[serde(default)]
    pub only_story_ids: Option<Vec<String>>,
}

/// `RenderBuildJob` のストレージ。
///
/// フェッチャの選択理由は [`crate::compare_build::CompareBuildStorage`] と同じ
/// （通知型は起動前に投入されたジョブを取りこぼすためポーリング型を使う）。
pub type RenderBuildStorage = PostgresStorage<RenderBuildJob>;

pub fn build_storage_for_queue(pool: &PgPool, queue: &str) -> RenderBuildStorage {
    PostgresStorage::new_with_config(pool, &Config::new(queue))
}

pub fn build_storage(pool: &PgPool) -> RenderBuildStorage {
    build_storage_for_queue(pool, QUEUE_NAME)
}

pub async fn setup(pool: &PgPool) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    setup_with_queue(pool, QUEUE_NAME).await
}

/// キュー名を指定してセットアップする（統合テスト用）。
pub async fn setup_with_queue(
    pool: &PgPool,
    queue: &str,
) -> Result<Arc<RenderBuildStorage>, anyhow::Error> {
    crate::ensure_apalis_schema(pool).await?;
    Ok(Arc::new(build_storage_for_queue(pool, queue)))
}

pub async fn enqueue(
    storage: &RenderBuildStorage,
    job: RenderBuildJob,
) -> Result<(), anyhow::Error> {
    let mut storage = storage.clone();
    storage
        .push(job)
        .await
        .map_err(|e| anyhow::anyhow!("push render build job: {e}"))?;
    Ok(())
}

/// ワーカーのエントリポイント。
///
/// 回復不能なエラーはビルドを `failed` に落として `Ok(())` を返す（無限リトライ回避）。
/// `Err` を返すのはビルド行にすら書き戻せなかったケースだけ。
pub async fn process(job: RenderBuildJob, state: Data<JobState>) -> Result<(), BoxDynError> {
    let build_id = job.build_id;

    match run(build_id, job.only_story_ids, &state).await {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::error!(%build_id, error = %err, "render build job failed");
            // 失敗理由を成果物のログにも 1 行残す（UI/CI から追える）。
            service::build_logs::append(
                &state.db,
                build_id,
                LogLevel::Error,
                format!("render failed: {}", truncate(&err.to_string(), 2000)),
            )
            .await
            .map_err(|e| -> BoxDynError { format!("append render failure log: {e}").into() })?;
            let build = service::builds::get_build(&state.db, build_id)
                .await
                .map_err(|e| -> BoxDynError { format!("reload build {build_id}: {e}").into() })?;
            service::builds::mark_failed(&state.db, build, truncate(&err.to_string(), 2000))
                .await
                .map_err(|e| -> BoxDynError {
                    format!("mark build {build_id} failed: {e}").into()
                })?;

            // レンダリング失敗もビルドの終端なので GitHub にステータスを返す。
            crate::github_status::enqueue_best_effort(&state.github_status_storage, build_id).await;

            // 終端（failed）に落ちたので保持数超過分を掃除する（ベストエフォート）。
            if let Ok(build) = service::builds::get_build(&state.db, build_id).await {
                service::builds::prune_project_builds_best_effort(
                    &state.db,
                    &state.storage,
                    build.project_id,
                )
                .await;
            }
            Ok(())
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

async fn run(
    build_id: Uuid,
    only_story_ids: Option<Vec<String>>,
    state: &JobState,
) -> Result<(), anyhow::Error> {
    let db = &state.db;

    let build = service::builds::get_build(db, build_id).await?;
    if build.status != BuildStatus::Rendering {
        tracing::info!(%build_id, status = ?build.status, "skipping render job for non-rendering build");
        return Ok(());
    }
    if build.mode != BuildMode::Storybook {
        anyhow::bail!("build {build_id} is not a storybook-mode build");
    }

    let storybook_key = build
        .storybook_key
        .clone()
        .ok_or_else(|| anyhow::anyhow!("build {build_id} has no storybook bundle"))?;

    let project = service::projects::get_project(db, build.project_id).await?;

    let chromium_path = state
        .settings
        .chromium_path
        .clone()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("storybook rendering is not configured (CHROMIUM_PATH is unset)")
        })?;

    // リトライ安全性: 前回の途中結果を捨ててからやり直す。
    // （`(build_id, name)` の UNIQUE にぶつかると 2 回目以降が必ず落ちる。）
    screenshots::Entity::delete_many()
        .filter(screenshots::Column::BuildId.eq(build_id))
        .exec(db)
        .await?;

    let bytes = service::render::download_bundle(&state.storage, &storybook_key).await?;

    // 一時ディレクトリは TempDir が drop されるときに必ず消える
    // （成功・失敗・panic のいずれでも）。
    let workdir = tempfile::Builder::new()
        .prefix("vrt-storybook-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("create temp dir: {e}"))?;

    let bundle = {
        let dest = workdir.path().to_path_buf();
        // 展開は同期 IO + 解凍で CPU バウンド。ワーカーのランタイムを塞がない。
        tokio::task::spawn_blocking(move || service::render::extract_and_index(&bytes, &dest))
            .await
            .map_err(|e| anyhow::anyhow!("bundle extraction task join: {e}"))??
    };

    if bundle.stories.is_empty() {
        anyhow::bail!("storybook bundle contains no stories (only docs entries?)");
    }

    // バンドル展開が終わり、撮影対象のストーリー数が確定した時点で開始行を残す。
    service::build_logs::append(
        db,
        build_id,
        LogLevel::Info,
        format!("render started: {} stories", bundle.stories.len()),
    )
    .await?;

    // `only_story_ids` が来ているときだけ baseline から name→entry の流用テーブルを作る。
    // baseline は finalize が照合のうえ固定したもの（`builds.baseline_id`）を使う。
    // ここで最新を引き直すと、finalize〜レンダリングの間に別ビルドが承認された場合に
    // クライアントが差分計画の起点にした baseline と違うものを流用してしまう
    // （compare_build と同じ理由）。固定が無ければ空になり、結果的に全撮影になる。
    let baseline_entries: HashMap<String, baseline_entries::Model> = match &only_story_ids {
        Some(_) => {
            let baseline = match build.baseline_id {
                Some(id) => Some(service::baselines::get_baseline(db, id).await?),
                None => None,
            };
            let entries = match &baseline {
                Some(b) => service::baselines::entries(db, b.id).await?,
                None => Vec::new(),
            };
            entries.into_iter().map(|e| (e.name.clone(), e)).collect()
        }
        None => HashMap::new(),
    };

    tracing::info!(
        %build_id,
        stories = bundle.stories.len(),
        skip_mode = only_story_ids.is_some(),
        baseline_entries = baseline_entries.len(),
        "rendering storybook bundle"
    );

    let server = StaticServer::start(&bundle.root).await?;
    let base_url = server.base_url();

    let options = RenderOptions::new(
        chromium_path,
        project.viewport_width.max(1) as u32,
        project.viewport_height.max(1) as u32,
    );
    let renderer = StoryRenderer::launch(options).await?;

    // ブラウザは成功・失敗どちらでも必ず閉じる（`?` で早期 return しない）。
    let outcome = render_all(
        state,
        &project,
        &build,
        &renderer,
        &base_url,
        &bundle,
        only_story_ids.as_deref(),
        &baseline_entries,
    )
    .await;
    renderer.close().await;
    drop(server);
    outcome?;

    let build = service::builds::get_build(db, build_id).await?;
    let build = service::builds::transition(db, build, BuildStatus::Processing).await?;

    // レンダリングが済んだので既存の比較パイプラインへ引き渡す。
    // `github_status` を compare_build が投入するのと同じチェーンパターン。
    crate::compare_build::enqueue(
        &state.compare_build_storage,
        crate::CompareBuildJob { build_id },
    )
    .await?;

    tracing::info!(%build_id, number = build.number, "storybook render finished; compare job enqueued");

    Ok(())
}

/// ストーリー 1 件をどう処理するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoryAction {
    /// Chromium で撮影する。
    Render,
    /// baseline のスクリーンショットを流用する。
    Reuse,
}

/// `only_story_ids` モードでの 1 ストーリーの処理方針を決める純関数。
///
/// - `only_story_ids` に含まれる → 撮影（変更があったと分かっているもの）
/// - 含まれず、baseline に同名エントリがある → 流用
/// - 含まれず、baseline にも無い（新規ストーリー） → 撮影（見逃し防止）
///
/// baseline が存在しないケースは `baseline_names` が空になるので、
/// すべて「新規扱い」で撮影に倒れる（= 全撮影）。
fn decide_story_action(
    story_id: &str,
    screenshot_name: &str,
    only_story_ids: &HashSet<&str>,
    baseline_names: &HashSet<&str>,
) -> StoryAction {
    if only_story_ids.contains(story_id) {
        StoryAction::Render
    } else if baseline_names.contains(screenshot_name) {
        StoryAction::Reuse
    } else {
        StoryAction::Render
    }
}

/// 全ストーリーを逐次処理して保存する。
///
/// `only_story_ids` が `None` のときは全ストーリーを撮影する（従来どおり。
/// metadata に `reused` は付けない）。`Some` のときは [`decide_story_action`]
/// に従い、撮影対象外のストーリーを baseline のスクリーンショットで流用する。
///
/// ## 失敗の粒度（story 単位の隔離）
///
/// story に固有の失敗では**その story だけをエラーとし、残りの story は
/// 撮り続ける**。全 story を処理し終えてから、失敗した story を列挙して
/// ビルドを失敗させる。最初の 1 件で `?` 中断すると、静止できない story が
/// 1 つあるだけで他の全 story のスクリーンショットとログまで巻き添えで
/// 失われ、利用者は「1 回のビルドで 1 件ずつ」しか失敗を発見できない。
/// story 固有と分類するのは:
///
/// - レンダリングの失敗（[`is_story_scoped`] — `storyErrored` 等の
///   [`RenderError::Story`] と、静止不能・READY 不達・freeze evaluate の
///   CDP エラー期限切れの [`RenderError::Timeout`]）
/// - スクリーンショット名の規則違反（[`parse_screenshot_name`] — story の
///   title / name に起因し、他の story の名前とは独立）
///
/// ビルド自体は fail-closed のまま `failed` に落とす——失敗した story の
/// スクリーンショットが欠けたまま `processing` へ進めると、比較結果が
/// 「全 story 緑」に見えて偽 PASS の口になる（撮れなかった story を
/// `comparisons` の error 行としてレビュー UI に出す形は後続で扱う）。
///
/// 環境・インフラ側の失敗（Chromium/CDP の異常 [`RenderError::Cdp`]、
/// ストレージ・DB の失敗）は従来どおり即中断する——次の story も同じ理由で
/// 落ちる公算が大きく、続行は同じエラーの羅列に story_timeout×N の時間を
/// 費やすだけである。分類の全経路は `service::render::browser` モジュール
/// 先頭の「story 固有の失敗と環境の失敗」表を参照。
///
/// 停止性: 隔離によって所要時間の上限は変わらない——成功する story も
/// もともと 1 件あたり最大 `story_timeout` かけてよい契約であり、上限は
/// 従来と同じ `stories × story_timeout` のままである。
#[allow(clippy::too_many_arguments)]
async fn render_all(
    state: &JobState,
    project: &entity::projects::Model,
    build: &entity::builds::Model,
    renderer: &StoryRenderer,
    base_url: &str,
    bundle: &service::render::ExtractedBundle,
    only_story_ids: Option<&[String]>,
    baseline_entries: &HashMap<String, baseline_entries::Model>,
) -> Result<(), anyhow::Error> {
    // 撮影対象 ID と baseline 名を借用のハッシュ集合にしておく（ループ内で使い回す）。
    let only_set: Option<HashSet<&str>> =
        only_story_ids.map(|ids| ids.iter().map(String::as_str).collect());
    let baseline_names: HashSet<&str> = baseline_entries.keys().map(String::as_str).collect();

    let mut rendered = 0usize;
    let mut reused = 0usize;
    let mut story_failures: Vec<StoryFailure> = Vec::new();
    let total = bundle.stories.len();

    for (idx, story) in bundle.stories.iter().enumerate() {
        let position = idx + 1;
        // 名前規則（ScreenshotName——アップロード経路と同一）。storybook の
        // title / name から生成した名前が規則に合わない場合は、黙って加工せず
        // 失敗させて story 側の修正を促す（加工すると baseline 名との
        // 突き合わせがずれる）。違反は story の title / name に起因する
        // **story 固有の失敗**なので、レンダリング失敗と同じく story 単位で
        // 隔離する——複数 story が違反していても 1 回のビルドで全件が
        // 列挙され、「直しては次の 1 件」の反復にならない。名前は撮影にも
        // baseline 流用にも要るため、隔離は Render / Reuse の分岐より前で行う。
        let screenshot_name = match parse_screenshot_name(&story.id, &story.screenshot_name()) {
            Ok(name) => name,
            Err(failure) => {
                service::build_logs::append(
                    &state.db,
                    build.id,
                    LogLevel::Error,
                    format!(
                        "story failed {position}/{total} {}: {}",
                        story.id, failure.message
                    ),
                )
                .await?;
                story_failures.push(failure);
                continue;
            }
        };
        let screenshot_name_str = screenshot_name.as_str().to_string();

        // `only_story_ids` 無しは常に撮影（後方互換）。
        let action = match &only_set {
            Some(ids) => decide_story_action(&story.id, &screenshot_name_str, ids, &baseline_names),
            None => StoryAction::Render,
        };

        match action {
            StoryAction::Render => {
                let png = match renderer.render_story(base_url, &story.id).await {
                    Ok(png) => png,
                    // story 固有の失敗はその story だけをエラーにし、残りを
                    // 撮り続ける（ビルドの成否はループ後にまとめて判定）。
                    Err(e) if is_story_scoped(&e) => {
                        service::build_logs::append(
                            &state.db,
                            build.id,
                            LogLevel::Error,
                            format!("story failed {position}/{total} {}: {e}", story.id),
                        )
                        .await?;
                        story_failures.push(StoryFailure {
                            story_id: story.id.clone(),
                            message: e.to_string(),
                        });
                        continue;
                    }
                    // 環境側の失敗は従来どおり即中断（続けても同じ理由で落ちる）。
                    Err(e) => {
                        return Err(anyhow::anyhow!("render story `{}`: {e}", story.id));
                    }
                };

                // `only_story_ids` モードのときだけ reused を明示する
                // （`None` の従来経路は metadata を変えない）。
                let metadata = if only_set.is_some() {
                    serde_json::json!({
                        "story_id": story.id,
                        "title": story.title,
                        "reused": false,
                    })
                } else {
                    serde_json::json!({
                        "story_id": story.id,
                        "title": story.title,
                    })
                };

                service::screenshots::store_screenshot_with_metadata(
                    &state.db,
                    &state.storage,
                    project.tenant_id,
                    project.id,
                    build.id,
                    screenshot_name,
                    bytes::Bytes::from(png),
                    Some(metadata),
                )
                .await
                .map_err(|e| anyhow::anyhow!("store screenshot for story `{}`: {e}", story.id))?;
                rendered += 1;
                service::build_logs::append(
                    &state.db,
                    build.id,
                    LogLevel::Info,
                    format!("rendered {position}/{total} {}", story.id),
                )
                .await?;
            }
            StoryAction::Reuse => {
                // decide_story_action が Reuse を返す時点で必ず存在する。
                let entry = baseline_entries.get(&screenshot_name_str).ok_or_else(|| {
                    anyhow::anyhow!(
                        "baseline entry for `{screenshot_name}` disappeared during reuse"
                    )
                })?;

                // baseline の PNG バイト列をそのまま今回のスクリーンショットとして保存する。
                // バイト列が同一なので、後段の compare_build が unchanged と判定する。
                let png = service::screenshots::read_all(&state.storage, &entry.storage_key)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("download baseline for `{screenshot_name}`: {e}")
                    })?;

                let metadata = serde_json::json!({
                    "story_id": story.id,
                    "title": story.title,
                    "reused": true,
                });

                service::screenshots::store_screenshot_with_metadata(
                    &state.db,
                    &state.storage,
                    project.tenant_id,
                    project.id,
                    build.id,
                    screenshot_name,
                    bytes::Bytes::from(png),
                    Some(metadata),
                )
                .await
                .map_err(|e| {
                    anyhow::anyhow!("store reused screenshot for story `{}`: {e}", story.id)
                })?;
                reused += 1;
                service::build_logs::append(
                    &state.db,
                    build.id,
                    LogLevel::Info,
                    format!("reused {position}/{total} {}", story.id),
                )
                .await?;
            }
        }
    }

    // 完了サマリ。撮影・流用・失敗の内訳を 1 行で残す。
    service::build_logs::append(
        &state.db,
        build.id,
        LogLevel::Info,
        format!(
            "render complete: rendered {rendered} reused {reused} failed {}",
            story_failures.len()
        ),
    )
    .await?;

    tracing::info!(
        build_id = %build.id,
        rendered,
        reused,
        failed = story_failures.len(),
        "storybook stories processed"
    );

    // 失敗した story があればビルドは fail-closed で失敗させる。ここまで
    // 残りの story は撮り終えているので、error_message とログには全失敗
    // story が一度に列挙される——「1 回のビルドで 1 件ずつ」にならない。
    if !story_failures.is_empty() {
        return Err(anyhow::anyhow!(
            "{}",
            summarize_story_failures(&story_failures, total)
        ));
    }

    Ok(())
}

/// 撮影に失敗した 1 story の記録。
struct StoryFailure {
    story_id: String,
    message: String,
}

/// storybook の title / name から生成したスクリーンショット名を検証する。
///
/// 違反は story の内容（title / name）に起因する **story 固有の失敗**として
/// [`StoryFailure`] で返す——呼び出し側（`render_all`）はレンダリング失敗と
/// 同じく隔離して残りの story を処理し続け、ループ後にまとめてビルドを
/// 失敗させる。修正前はここの `?` 相当の即中断が「1 ビルドで違反 1 件ずつ」
/// しか報告できなかった。
fn parse_screenshot_name(
    story_id: &str,
    raw: &str,
) -> Result<common::validation::ScreenshotName, StoryFailure> {
    common::validation::ScreenshotName::parse(raw).map_err(|e| StoryFailure {
        story_id: story_id.to_string(),
        message: format!("screenshot name {raw:?} is invalid: {e}"),
    })
}

/// この失敗は story に固有か（= 他の story は撮り続けてよいか）。
///
/// - [`RenderError::Story`]: Storybook 自身のエラー通知（`storyErrored` 等）、
///   静止の失敗・静止結果の解析不能——いずれもその story の内容に起因する
/// - [`RenderError::Timeout`]: その story の描画・静止が時間予算内に
///   終わらなかった——これも story の内容に起因する。freeze evaluate の
///   CDP エラー（story 側の navigation / reload・rAF コールバック捨てによる
///   pending promise の GC 回収）もレンダラが deadline までリトライした上で
///   この分類に倒れてくる
/// - [`RenderError::Cdp`] / [`RenderError::Launch`] / [`RenderError::Server`]:
///   ブラウザ・配信側の異常。次の story も同じ理由で落ちる公算が大きく、
///   隔離せず即中断する（Cdp がここへ届くのは `new_page` / hook 注入 /
///   `goto` / スクリーンショットの、story のスクリプトを待たない CDP 往復
///   だけ——分類の全経路は `service::render::browser` モジュール先頭の表）
fn is_story_scoped(err: &RenderError) -> bool {
    matches!(err, RenderError::Story { .. } | RenderError::Timeout { .. })
}

/// 失敗 story の一覧をビルドの `error_message` 用に 1 行へまとめる。
///
/// 呼び出し側（`process`）が 2000 文字へ truncate するため、詳細は先頭
/// 10 件までにして残りは件数だけ伝える（10 件を超える失敗で先頭が
/// 切り落とされて「何件失敗したか」まで消えるのを防ぐ）。
fn summarize_story_failures(failures: &[StoryFailure], total: usize) -> String {
    const DETAIL_LIMIT: usize = 10;
    let details = failures
        .iter()
        .take(DETAIL_LIMIT)
        .map(|f| format!("story `{}`: {}", f.story_id, f.message))
        .collect::<Vec<_>>()
        .join("; ");
    let more = if failures.len() > DETAIL_LIMIT {
        format!(
            " (and {} more, see the build log)",
            failures.len() - DETAIL_LIMIT
        )
    } else {
        String::new()
    };
    format!(
        "{} of {} stories failed to render; the remaining stories were captured: {details}{more}",
        failures.len(),
        total
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_limits_error_messages() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate(&"x".repeat(50), 10).len(), 10);
    }

    #[test]
    fn queue_name_is_stable() {
        // ワーカー名は `{queue}-worker-{uuid}` で組み立てられる（server.rs 参照）。
        assert_eq!(QUEUE_NAME, "render_build");
    }

    fn set<'a>(items: &[&'a str]) -> HashSet<&'a str> {
        items.iter().copied().collect()
    }

    #[test]
    fn requested_story_is_rendered() {
        let only = set(&["button--primary"]);
        let baseline = set(&["Button/Primary"]);
        // 撮影対象に入っていれば baseline にあっても撮影する。
        assert_eq!(
            decide_story_action("button--primary", "Button/Primary", &only, &baseline),
            StoryAction::Render
        );
    }

    #[test]
    fn unrequested_story_with_baseline_is_reused() {
        let only = set(&["button--primary"]);
        let baseline = set(&["Card/Default"]);
        // 撮影対象外だが baseline に同名がある → 流用。
        assert_eq!(
            decide_story_action("card--default", "Card/Default", &only, &baseline),
            StoryAction::Reuse
        );
    }

    #[test]
    fn new_story_without_baseline_is_rendered() {
        let only = set(&["button--primary"]);
        let baseline = set(&["Card/Default"]);
        // 撮影対象外で baseline にも無い（新規） → 見逃さず撮影。
        assert_eq!(
            decide_story_action("badge--new", "Badge/New", &only, &baseline),
            StoryAction::Render
        );
    }

    #[test]
    fn empty_baseline_falls_back_to_render() {
        let only = set(&["button--primary"]);
        let baseline: HashSet<&str> = HashSet::new();
        // baseline が無ければ撮影対象外でも撮影に倒れる（= 全撮影）。
        assert_eq!(
            decide_story_action("card--default", "Card/Default", &only, &baseline),
            StoryAction::Render
        );
    }

    /// story 固有の失敗（Story / Timeout）だけが隔離され、環境側の失敗
    /// （Launch / Server / Cdp）は即中断のままであること。
    ///
    /// 証明する: [`is_story_scoped`] の分類のみ。証明しない: render_all の
    /// ループが実際に continue すること（DB を要する統合経路。分類を通る
    /// 分岐は match の構造で保証される）。
    #[test]
    fn story_scoped_failures_are_isolated_and_infrastructure_failures_are_not() {
        use std::time::Duration;

        let story = RenderError::Story {
            story_id: "a--b".into(),
            message: "freeze failed: 1 animation(s) still running".into(),
        };
        assert!(is_story_scoped(&story));

        let timeout = RenderError::Timeout {
            story_id: "a--b".into(),
            timeout: Duration::from_secs(30),
            phase: "the freeze did not finish",
        };
        assert!(is_story_scoped(&timeout));

        let server = RenderError::Server("bind failed".into());
        assert!(!is_story_scoped(&server));
        // Launch / Cdp は chromiumoxide のエラー値が要るためここでは構築しない。
        // is_story_scoped は Story / Timeout を列挙するホワイトリストなので、
        // 新しい variant が増えても既定は「中断」側へ倒れる（fail-closed）。
    }

    /// 名前規則違反が story 固有の失敗（[`StoryFailure`]）として返り、
    /// 正当な名前は通ること。
    #[test]
    fn screenshot_name_violation_becomes_a_story_failure() {
        let ok = parse_screenshot_name("button--primary", "Button/Primary");
        assert!(ok.is_ok(), "a rule-abiding name must parse");

        let failure = parse_screenshot_name("button--primary", "Button/Primary ")
            .expect_err("a name with trailing whitespace violates the rule");
        assert_eq!(failure.story_id, "button--primary");
        assert!(
            failure.message.contains("invalid") && failure.message.contains("Button/Primary "),
            "the failure must name the offending screenshot name and say it is \
             invalid, got {:?}",
            failure.message
        );
    }

    /// **複数の名前規則違反が 1 回のビルドで全件報告される**こと。
    ///
    /// 修正前は最初の違反で `?` 即中断し（`anyhow!("screenshot name for story
    /// `{}` is invalid: ...")` を return）、違反が 3 件あっても利用者は
    /// 「直しては次の 1 件」を 3 ビルド繰り返すしかなかった。修正後は
    /// [`parse_screenshot_name`] が違反を [`StoryFailure`] として返し、
    /// `render_all` がレンダリング失敗と同じ経路で収集する——ここでは
    /// その収集と要約の合成を、違反 2 件 + 正当 1 件で固定する。
    ///
    /// 証明する: 違反の全件が [`StoryFailure`] として集まり、要約に両方の
    /// story と理由が載ること。証明しない: `render_all` のループが実際に
    /// continue すること（DB を要する統合経路。分岐は match の構造で保証——
    /// 上の `story_scoped_failures_are_isolated_...` の但し書きと同じ）。
    #[test]
    fn multiple_name_violations_are_all_reported_in_one_build() {
        let stories = [
            ("a--bad-tail", "Bad/Tail "),
            ("b--good", "Good/Name"),
            ("c--bad-control", "Bad/Con\ttrol"),
        ];
        let mut failures = Vec::new();
        let mut parsed = 0usize;
        for (story_id, raw) in stories {
            match parse_screenshot_name(story_id, raw) {
                Ok(_) => parsed += 1,
                Err(failure) => failures.push(failure),
            }
        }
        assert_eq!(parsed, 1, "the valid story must still be processed");
        assert_eq!(
            failures.len(),
            2,
            "every violation must be collected instead of stopping at the first"
        );
        let summary = summarize_story_failures(&failures, stories.len());
        assert!(
            summary.contains("2 of 3 stories failed")
                && summary.contains("a--bad-tail")
                && summary.contains("c--bad-control"),
            "one build must report every violating story at once, got {summary:?}"
        );
    }

    /// 失敗一覧の要約が「何件中何件か」「残りは撮れたこと」「各 story の理由」を
    /// 含み、11 件以上では先頭 10 件 + 残数へ丸められること。
    #[test]
    fn story_failure_summary_names_each_story_and_caps_the_details() {
        let failures = vec![
            StoryFailure {
                story_id: "a--x".into(),
                message: "freeze failed: 2 animation(s) still running".into(),
            },
            StoryFailure {
                story_id: "b--y".into(),
                message: "storyErrored: kaboom".into(),
            },
        ];
        let summary = summarize_story_failures(&failures, 40);
        assert!(
            summary.contains("2 of 40 stories failed")
                && summary.contains("the remaining stories were captured")
                && summary.contains("story `a--x`: freeze failed")
                && summary.contains("story `b--y`: storyErrored"),
            "summary must name every failed story with its reason, got {summary:?}"
        );

        let many: Vec<StoryFailure> = (0..12)
            .map(|i| StoryFailure {
                story_id: format!("s--{i}"),
                message: "freeze failed".into(),
            })
            .collect();
        let summary = summarize_story_failures(&many, 50);
        assert!(
            summary.contains("12 of 50 stories failed")
                && summary.contains("s--9")
                && !summary.contains("s--10")
                && summary.contains("and 2 more"),
            "past 10 failures the summary must keep the count and drop the tail, got {summary:?}"
        );
    }
}
