//! ビルドのライフサイクル管理。
//!
//! 状態遷移は [`transition`] に一本化し、不正な遷移は必ず [`AppError::Conflict`] にする。
//! 承認 ([`approve_build`]) はプロジェクト行を `SELECT ... FOR UPDATE` で直列化してから
//! baseline を作るため、同一プロジェクトの並行承認でも baseline が競合しない。

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DbBackend, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    prelude::Uuid,
};

use common::db::with_transaction;
use common::error::AppError;
use entity::{
    baseline_entries, baselines, builds, builds::BuildMode, builds::BuildStatus, comparisons,
    comparisons::ComparisonStatus, comparisons::ReviewStatus, projects, screenshots,
};

use crate::approval::{self, ApproveOptions, ComparisonFacts};
use crate::storage::StorageBackend;

/// エラーメッセージに並べる story 名の上限。超過分は件数だけ示す。
const MAX_REPORTED_NAMES: usize = 10;

/// ビルド一覧のデフォルト件数。
pub const DEFAULT_LIST_LIMIT: u64 = 30;
/// ビルド一覧の最大件数。
pub const MAX_LIST_LIMIT: u64 = 100;

/// 集計済みの比較結果カウント。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BuildCounts {
    pub total: i32,
    pub changed: i32,
    pub added: i32,
    pub removed: i32,
    pub unchanged: i32,
}

impl BuildCounts {
    /// 差分（changed / added / removed）が 1 件でもあるか。
    pub fn has_differences(self) -> bool {
        self.changed > 0 || self.added > 0 || self.removed > 0
    }
}

/// プロジェクト内で欠番のないビルド番号を払い出す。
///
/// task の `project_task_counters` と同じ upsert パターン。
/// `INSERT ... ON CONFLICT DO UPDATE SET counter = counter + 1 RETURNING counter` は
/// 1 ステートメントで行ロックまで完結するため、並行 INSERT でも番号が飛ばない。
pub async fn next_build_number<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
) -> Result<i64, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO project_build_counters (project_id, counter)
            VALUES ($1, 1)
            ON CONFLICT (project_id) DO UPDATE
                SET counter = project_build_counters.counter + 1
            RETURNING counter
            "#,
            vec![project_id.into()],
        ))
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("build counter upsert returned no row"))
        })?;

    Ok(row.try_get_by_index::<i64>(0)?)
}

/// 新しいビルドを `pending` で作成する。
///
/// `mode` は入力形式。`storybook` のときは screenshot のアップロードを受け付けず、
/// 代わりに `POST /v1/ci/builds/{id}/storybook` でバンドルを受け取る。
///
/// ## baseline はここでは固定しない
///
/// `baseline_id` は作成時には常に NULL のまま。作成時に固定すると、作成から
/// finalize まで時間の空いたビルド（例: 他ブランチの fallback baseline を使う
/// feature ビルド）が、その間に前進した最新 baseline と比較できなくなる。
/// 固定は部分撮影の計画が確定した時点——screenshots モードは
/// [`attach_capture_plan`]、storybook モードは finalize の `only_story_ids`——
/// でだけ行い、それ以外は従来どおり比較ジョブが比較時点の最新を解決する。
#[allow(clippy::too_many_arguments)]
pub async fn create_build<C: ConnectionTrait>(
    db: &C,
    project: &projects::Model,
    branch: String,
    commit_sha: String,
    commit_message: Option<String>,
    pull_request_number: Option<i32>,
    mode: BuildMode,
) -> Result<builds::Model, AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::BadRequestDetail("branch is required".into()));
    }
    if commit_sha.trim().is_empty() {
        return Err(AppError::BadRequestDetail("commit_sha is required".into()));
    }

    let number = next_build_number(db, project.id).await?;

    Ok(builds::ActiveModel {
        id: Set(Uuid::new_v4()),
        project_id: Set(project.id),
        number: Set(number),
        branch: Set(branch),
        commit_sha: Set(commit_sha),
        commit_message: Set(commit_message),
        pull_request_number: Set(pull_request_number),
        status: Set(BuildStatus::Pending),
        mode: Set(mode),
        storybook_key: Set(None),
        baseline_id: Set(None),
        capture_plan: Set(None),
        total_count: Set(0),
        changed_count: Set(0),
        added_count: Set(0),
        removed_count: Set(0),
        unchanged_count: Set(0),
        error_message: Set(None),
        approval_evidence: Set(None),
        approved_by: Set(None),
        approved_at: Set(None),
        created_at: Set(Utc::now().fixed_offset()),
        completed_at: Set(None),
    }
    .insert(db)
    .await?)
}

/// ビルドを ID で取得する。
pub async fn get_build<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<builds::Model, AppError> {
    builds::Entity::find_by_id(build_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// プロジェクトのビルド一覧（新しい順）。
pub async fn list_builds<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    limit: u64,
    offset: u64,
) -> Result<Vec<builds::Model>, AppError> {
    Ok(builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .order_by_desc(builds::Column::Number)
        .limit(limit.clamp(1, MAX_LIST_LIMIT))
        .offset(offset)
        .all(db)
        .await?)
}

/// プロジェクト内のビルド番号でビルドを取得する。
///
/// `(project_id, number)` は一意。UI の `/builds/{number}` 表示が一覧を舐めずに済むように使う。
pub async fn get_build_by_number<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    number: i64,
) -> Result<builds::Model, AppError> {
    builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .filter(builds::Column::Number.eq(number))
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// プロジェクトのビルド総数（ページネーション用）。
pub async fn count_builds<C: ConnectionTrait>(db: &C, project_id: Uuid) -> Result<u64, AppError> {
    Ok(builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .count(db)
        .await?)
}

/// 状態遷移。許可されていない遷移は [`AppError::Conflict`]。
///
/// パイプラインが完走した状態（passed / changes_detected / failed）に入るときに
/// `completed_at` を打つ。承認・却下では触らない
/// （セマンティクスは [`BuildStatus::completes_pipeline`] 参照）。
pub async fn transition<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    to: BuildStatus,
) -> Result<builds::Model, AppError> {
    if !build.status.can_transition_to(to) {
        return Err(AppError::Conflict);
    }

    let mut active: builds::ActiveModel = build.into();
    active.status = Set(to);
    if to.completes_pipeline() {
        active.completed_at = Set(Some(Utc::now().fixed_offset()));
    }
    Ok(active.update(db).await?)
}

/// finalize: `pending → processing`。ジョブ投入は呼び出し側（ハンドラ）が行う。
pub async fn finalize<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
) -> Result<builds::Model, AppError> {
    transition(db, build, BuildStatus::Processing).await
}

/// screenshots モードの部分アップロード計画。
///
/// [`attach_capture_plan`] が撮影開始前に `builds.capture_plan` へ書き込む。
/// finalize と比較ジョブの「今回撮る集合」は必ずこの保存値から来る——
/// finalize 時の自己申告（`captured_names`）を出所にすると、撮影が全滅した
/// ときに空の申告と空のアップロードが循環一致して偽 PASS になるためである。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CapturePlan {
    /// 今回撮影（アップロード）するスクリーンショット名。
    pub selected_names: Vec<String>,
    /// 現時点で存在する全スクリーンショット名（現行 index の写し）。
    /// baseline にあってここに無い名前は「消滅した」とみなし、流用せず
    /// `removed` として報告する。
    pub manifest_names: Vec<String>,
}

impl CapturePlan {
    pub fn selected_set(&self) -> HashSet<&str> {
        self.selected_names.iter().map(String::as_str).collect()
    }

    pub fn manifest_set(&self) -> HashSet<&str> {
        self.manifest_names.iter().map(String::as_str).collect()
    }
}

/// ビルドに保存された部分アップロード計画を取り出す。
///
/// `None` は計画なし（全撮影）。サーバー自身が書いた値なので、壊れた形は
/// データ破損として即エラーにする（黙って全撮影に読み替えると、計画外の
/// baseline エントリが removed になり誤承認で消える）。
pub fn capture_plan(build: &builds::Model) -> Result<Option<CapturePlan>, AppError> {
    let Some(value) = &build.capture_plan else {
        return Ok(None);
    };
    let plan: CapturePlan = serde_json::from_value(value.clone()).map_err(|e| {
        AppError::Internal(anyhow::anyhow!(
            "build {} has a malformed capture_plan ({e})",
            build.id
        ))
    })?;
    Ok(Some(plan))
}

/// 部分アップロード計画をビルドへ保存し、比較 baseline を固定する。
///
/// 循環（撮影結果から計画を逆算する偽 PASS）を断つため、計画は
/// **スクリーンショットを 1 枚もアップロードしていない pending ビルド**にしか
/// 添付できない。また計画の起点にした baseline（`planned_baseline_commit_sha`）が
/// いまも最新であることを確認してから `baseline_id` に固定する。作成〜計画の間に
/// baseline が動いていた場合は 409 で拒否し、クライアントに再計画させる。
pub async fn attach_capture_plan<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    project: &projects::Model,
    selected_names: Vec<String>,
    manifest_names: Vec<String>,
    planned_baseline_commit_sha: &str,
) -> Result<builds::Model, AppError> {
    if build.mode != BuildMode::Screenshots {
        return Err(AppError::BadRequestDetail(
            "a capture plan applies to screenshots-mode builds only; \
             storybook-mode builds narrow the capture set via only_story_ids at finalize"
                .into(),
        ));
    }
    if build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }
    if build.capture_plan.is_some() {
        return Err(AppError::ConflictDetail(
            "a capture plan is already attached to this build".into(),
        ));
    }
    if !crate::screenshots::list_for_build(db, build.id)
        .await?
        .is_empty()
    {
        return Err(AppError::ConflictDetail(
            "screenshots were already uploaded; the capture plan must be attached \
             before any upload so the selection cannot be derived from what happened \
             to be captured"
                .into(),
        ));
    }

    let selected: std::collections::BTreeSet<String> = selected_names.into_iter().collect();
    let manifest: std::collections::BTreeSet<String> = manifest_names.into_iter().collect();
    let outside: Vec<&String> = selected.difference(&manifest).take(10).collect();
    if !outside.is_empty() {
        return Err(AppError::BadRequestDetail(format!(
            "selected_names must be a subset of manifest_names \
             (selected but not in the manifest: {outside:?})"
        )));
    }

    // 計画の起点 baseline を検証してから固定する。
    let Some(baseline) = crate::baselines::latest_for(db, project, &build.branch).await? else {
        return Err(AppError::ConflictDetail(
            "no baseline exists for this branch yet; there is nothing to carry forward. \
             capture and upload all stories instead of attaching a plan"
                .into(),
        ));
    };
    let current_sha = baseline_source_commit_sha(db, &baseline).await?;
    if current_sha.as_deref() != Some(planned_baseline_commit_sha) {
        return Err(AppError::ConflictDetail(format!(
            "the baseline moved after this plan was computed \
             (planned against {planned_baseline_commit_sha}, current {}). \
             re-run the plan against the current baseline.",
            current_sha.as_deref().unwrap_or("none")
        )));
    }

    let mut active: builds::ActiveModel = build.into();
    active.baseline_id = Set(Some(baseline.id));
    active.capture_plan = Set(Some(
        serde_json::to_value(CapturePlan {
            selected_names: selected.into_iter().collect(),
            manifest_names: manifest.into_iter().collect(),
        })
        .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize capture plan: {e}")))?,
    ));
    Ok(active.update(db).await?)
}

/// screenshots モードの finalize。
///
/// 部分アップロードの「今回撮る集合」は finalize の申告ではなく、撮影前に
/// [`attach_capture_plan`] で保存された計画から取る。`captured_names` は
/// 後方互換のための任意のクロスチェックで、渡された場合は保存済み計画と
/// 完全一致しなければ拒否する。計画なしのビルドに `captured_names` を渡すのも
/// 拒否する——申告だけの部分アップロードは、撮影が全滅したときに
/// 「空の申告 == 空のアップロード」が成立して偽 PASS になるためである。
pub async fn finalize_screenshots<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    captured_names: Option<Vec<String>>,
) -> Result<builds::Model, AppError> {
    let plan = capture_plan(&build)?;
    match &plan {
        None => {
            if captured_names.is_some() {
                return Err(AppError::BadRequestDetail(
                    "captured_names requires a capture plan attached via \
                     POST /v1/ci/builds/{id}/plan before uploading; a partial upload \
                     declared only at finalize cannot be trusted (an empty declaration \
                     would match an empty upload even when every capture failed)"
                        .into(),
                ));
            }
        }
        Some(plan) => {
            let selected: std::collections::BTreeSet<String> =
                plan.selected_names.iter().cloned().collect();

            // 任意のクロスチェック: 申告が来たら保存済み計画と一致すること。
            if let Some(names) = captured_names {
                let declared: std::collections::BTreeSet<String> = names.into_iter().collect();
                if declared != selected {
                    let missing: Vec<&String> = selected.difference(&declared).take(10).collect();
                    let extra: Vec<&String> = declared.difference(&selected).take(10).collect();
                    return Err(AppError::BadRequestDetail(format!(
                        "captured_names does not match the capture plan attached to this build \
                         (planned but not declared: {missing:?}; declared but not planned: {extra:?})"
                    )));
                }
            }

            // 計画した集合が 1 枚残らずアップロードされていること。欠けを黙って
            // baseline 流用に回すと、撮影の失敗が「差分なし」に化ける。
            let uploaded: std::collections::BTreeSet<String> =
                crate::screenshots::list_for_build(db, build.id)
                    .await?
                    .into_iter()
                    .map(|s| s.name)
                    .collect();
            if uploaded != selected {
                let missing: Vec<&String> = selected.difference(&uploaded).take(10).collect();
                let extra: Vec<&String> = uploaded.difference(&selected).take(10).collect();
                return Err(AppError::BadRequestDetail(format!(
                    "the uploaded screenshots do not match the capture plan \
                     (planned but not uploaded: {missing:?}; uploaded but not planned: {extra:?})"
                )));
            }
        }
    }
    transition(db, build, BuildStatus::Processing).await
}

/// baseline の「昇格元ビルドのコミット SHA」を解決する。
///
/// 昇格元ビルドが記録されていない・削除済みなら `None`。
pub async fn baseline_source_commit_sha<C: ConnectionTrait>(
    db: &C,
    baseline: &baselines::Model,
) -> Result<Option<String>, AppError> {
    let Some(source_build_id) = baseline.source_build_id else {
        return Ok(None);
    };
    match get_build(db, source_build_id).await {
        Ok(source) => Ok(Some(source.commit_sha)),
        Err(AppError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// ビルドに固定された baseline の「昇格元ビルドのコミット SHA」を解決する。
///
/// baseline が固定されていない、または昇格元ビルドが削除済みなら `None`。
pub async fn pinned_baseline_commit_sha<C: ConnectionTrait>(
    db: &C,
    build: &builds::Model,
) -> Result<Option<String>, AppError> {
    let Some(baseline_id) = build.baseline_id else {
        return Ok(None);
    };
    let baseline = crate::baselines::get_baseline(db, baseline_id).await?;
    baseline_source_commit_sha(db, &baseline).await
}

/// このブランチの「いまの」baseline のコミット SHA（固定はしない）。
///
/// ビルド作成レスポンスの `baseline_commit_sha` 用。クライアントはこれを起点に
/// 撮り直し範囲を計画し、計画を固定するとき（capture plan の添付 /
/// storybook の only_story_ids finalize）に同じ値を渡して照合させる。
pub async fn current_baseline_commit_sha<C: ConnectionTrait>(
    db: &C,
    project: &projects::Model,
    branch: &str,
) -> Result<Option<String>, AppError> {
    let Some(baseline) = crate::baselines::latest_for(db, project, branch).await? else {
        return Ok(None);
    };
    baseline_source_commit_sha(db, &baseline).await
}

/// storybook の部分レンダリング（`only_story_ids`）向けに比較 baseline を固定する。
///
/// クライアントが差分計画の起点にした baseline（`expected_baseline_commit_sha`）が
/// いまも最新であることを確認してから `baseline_id` に固定する。ずれていれば
/// 400 で拒否し、再計画させる（流用画像と比較対象が計画と別の baseline に
/// なるのを finalize 時点で断つ）。baseline が無いのに部分レンダリングを
/// 要求された場合も拒否する（流用元が無い）。
pub async fn pin_baseline_for_partial_render<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    project: &projects::Model,
    expected_baseline_commit_sha: &str,
) -> Result<builds::Model, AppError> {
    let Some(baseline) = crate::baselines::latest_for(db, project, &build.branch).await? else {
        return Err(AppError::BadRequestDetail(
            "only_story_ids was provided but no baseline exists for this branch; \
             there is nothing to reuse. finalize without only_story_ids to capture \
             all stories"
                .into(),
        ));
    };
    let current_sha = baseline_source_commit_sha(db, &baseline).await?;
    if current_sha.as_deref() != Some(expected_baseline_commit_sha) {
        return Err(AppError::BadRequestDetail(format!(
            "expected_baseline_commit_sha does not match the current baseline \
             (expected {expected_baseline_commit_sha}, current {}). \
             re-plan against the current baseline.",
            current_sha.as_deref().unwrap_or("none")
        )));
    }
    let mut active: builds::ActiveModel = build.into();
    active.baseline_id = Set(Some(baseline.id));
    Ok(active.update(db).await?)
}

/// storybook モードの finalize: `pending → rendering`。
///
/// バンドルが未アップロードなら 409（`RenderBuildJob` が拾うものが無いため）。
/// ジョブ投入は呼び出し側（ハンドラ）が行う。
pub async fn finalize_storybook<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
) -> Result<builds::Model, AppError> {
    if build.storybook_key.is_none() {
        return Err(AppError::BadRequestDetail(
            "storybook bundle has not been uploaded for this build".into(),
        ));
    }
    transition(db, build, BuildStatus::Rendering).await
}

/// アップロードされた Storybook バンドルのストレージキーを記録する。
///
/// 1 ビルドにつき 1 本だけ。既に記録済みなら [`AppError::Conflict`]。
pub async fn attach_storybook_bundle<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    key: String,
) -> Result<builds::Model, AppError> {
    if build.mode != BuildMode::Storybook {
        return Err(AppError::Conflict);
    }
    if build.status != BuildStatus::Pending {
        return Err(AppError::Conflict);
    }
    if build.storybook_key.is_some() {
        return Err(AppError::Conflict);
    }

    let mut active: builds::ActiveModel = build.into();
    active.storybook_key = Set(Some(key));
    Ok(active.update(db).await?)
}

/// 比較結果のカウントを集計して build に書き戻す。
pub async fn apply_counts<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    counts: BuildCounts,
    baseline_id: Option<Uuid>,
) -> Result<builds::Model, AppError> {
    let mut active: builds::ActiveModel = build.into();
    active.total_count = Set(counts.total);
    active.changed_count = Set(counts.changed);
    active.added_count = Set(counts.added);
    active.removed_count = Set(counts.removed);
    active.unchanged_count = Set(counts.unchanged);
    active.baseline_id = Set(baseline_id);
    Ok(active.update(db).await?)
}

/// ジョブが回復不能なエラーで落ちたときの終着点。
pub async fn mark_failed<C: ConnectionTrait>(
    db: &C,
    build: builds::Model,
    message: String,
) -> Result<builds::Model, AppError> {
    // 既に終端状態なら何もしない（リトライ時の二重書き込み防止）。
    if build.status.is_terminal() {
        return Ok(build);
    }
    let mut active: builds::ActiveModel = build.into();
    active.status = Set(BuildStatus::Failed);
    active.error_message = Set(Some(message));
    active.completed_at = Set(Some(Utc::now().fixed_offset()));
    Ok(active.update(db).await?)
}

/// レビュー待ち（`review_status = pending` かつ人手判断が要る）の比較件数。
pub async fn pending_review_count<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<u64, AppError> {
    Ok(comparisons::Entity::find()
        .filter(comparisons::Column::BuildId.eq(build_id))
        .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
        .filter(comparisons::Column::Status.is_not_in([ComparisonStatus::Unchanged]))
        .count(db)
        .await?)
}

/// ビルドを承認し、そのビルドの全スクリーンショットを新しい baseline に昇格する。
///
/// 承認ガード（すべて [`AppError::ConflictDetail`] で 409 を返す）:
///
/// 1. **却下の焼き付き防止**: 却下された比較が 1 件でも残っていれば承認しない。
///    却下したスクリーンショットを baseline に昇格させると、以降そのズレが「正」になる
/// 2. **baseline の巻き戻り防止**: 比較に使った baseline がいまも最新であることを確認する。
///    古いビルドを後追いで承認すると、新しい baseline が古い画像で上書きされる。
///    意図的な巻き戻しだけは `accept_revert` で別経路として許可し、証跡を残す
/// 3. **消滅の一括承認防止**: `force` は `removed` を巻き込まない。story の消滅は
///    `accept_removals` を明示したときだけ通し、さらに現行 baseline の
///    manifest と今回のスクリーンショットを突き合わせて、説明のつかない欠落を止める
/// 4. **比較失敗の焼き付き防止**: `force` は `failed` を巻き込まない。画像破損などで
///    比較できなかった結果は `accept_failures` を明示したときだけ通す
///
/// `options.force` が無いときは、レビュー待ちの比較が残っているだけで 409。
///
/// トランザクション内でプロジェクト行を `SELECT ... FOR UPDATE` してから baseline を作る。
/// これにより同一プロジェクトの並行承認が直列化され、`baselines` の
/// `(project_id, branch, created_at DESC)` 先頭が確定する。
pub async fn approve_build(
    db: &DatabaseConnection,
    build: builds::Model,
    reviewer_id: Uuid,
    options: ApproveOptions,
) -> Result<builds::Model, AppError> {
    if !(build.status.can_transition_to(BuildStatus::Approved)
        || options.accept_revert && build.status == BuildStatus::Approved)
    {
        return Err(AppError::ConflictDetail(format!(
            "cannot approve: build #{} has status {:?}, which cannot transition to approved; \
             an already approved older build may be restored with accept_revert; otherwise wait \
             for processing to finish or create a new build.",
            build.number, build.status
        )));
    }

    with_transaction(db, move |txn| {
        Box::pin(async move {
            // 三つのレビュー判断経路で共通の順序（build -> project）を使う。
            // build は同一 build の比較レビュー/承認/却下を直列化し、project は異なる
            // build の baseline 昇格だけを直列化する。規約は review_lock に集約してある。
            let build = crate::review_lock::build(txn, build.id).await?;
            if !(build.status.can_transition_to(BuildStatus::Approved)
                || options.accept_revert && build.status == BuildStatus::Approved)
            {
                return Err(AppError::ConflictDetail(format!(
                    "cannot approve: build #{} now has status {:?}, which cannot transition to \
                     approved; refresh the build before retrying.",
                    build.number, build.status
                )));
            }
            let project = crate::review_lock::project(txn, build.project_id).await?;

            // 現行 baseline より古いビルドの承認を拒否する。
            // 比較ジョブが使ったのと同じ解決規則（同一ブランチ → デフォルトブランチ）で
            // 「いまの baseline」を引き直し、比較時点のものと一致するか確かめる。
            // ロックを取ったあとに読むので、並行承認との間に隙間は無い。
            let current = crate::baselines::latest_for(txn, &project, &build.branch).await?;

            let mut reverted_from_build = None;
            let mut baseline_source_missing = false;
            if let Some(baseline) = &current
                && baseline.branch == build.branch
            {
                if let Some(source_build_id) = baseline.source_build_id {
                    if source_build_id != build.id {
                        let source_number = match get_build(txn, source_build_id).await {
                            Ok(source) => Some(source.number),
                            Err(AppError::NotFound) => {
                                baseline_source_missing = true;
                                None
                            }
                            Err(error) => return Err(error),
                        };
                        if let Some(source_number) = source_number
                            && approval::is_older_than_baseline_source(
                                build.number,
                                Some(source_number),
                            )
                        {
                            if options.accept_revert {
                                reverted_from_build = Some(source_number);
                            } else {
                                return Err(AppError::ConflictDetail(format!(
                                    "cannot approve: build #{} is older than the current baseline \
                                     (created from build #{}); to intentionally restore this build, \
                                     retry with accept_revert; otherwise re-run it against the current \
                                     baseline.",
                                    build.number, source_number
                                )));
                            }
                        }
                    }
                } else {
                    // Retention や旧データで source が無い baseline は、通常承認には使えるが
                    // 世代の前後関係を証明できないため revert 判定には使わない。
                    baseline_source_missing = true;
                }
            }

            if options.accept_revert && reverted_from_build.is_none() {
                if baseline_source_missing {
                    return Err(AppError::ConflictDetail(
                        "cannot approve: accept_revert was provided, but the current baseline \
                         source build is no longer retained, so the revert cannot be verified; \
                         re-run this build against the current baseline."
                            .to_string(),
                    ));
                }
                return Err(AppError::ConflictDetail(
                    "cannot approve: accept_revert was provided, but this approval is not a \
                     revert to an older build; retry without accept_revert."
                        .to_string(),
                ));
            }

            if reverted_from_build.is_none()
                && !approval::baseline_is_current(
                    build.baseline_id,
                    current.as_ref().map(|b| b.id),
                )
            {
                if baseline_source_missing {
                    return Err(AppError::ConflictDetail(
                        "cannot approve: the baseline moved and its source build is no longer \
                         retained, so the build ordering cannot be verified; re-run this build \
                         against the current baseline."
                            .to_string(),
                    ));
                }
                return Err(AppError::ConflictDetail(
                    "cannot approve: the baseline moved after this build was compared; \
                     re-run the build so it is compared against the current baseline."
                        .to_string(),
                ));
            }

            let mut facts = load_comparison_facts(txn, build.id).await?;

            let accepted_removals = if options.force && options.accept_removals {
                approval::pending_names_with_status(&facts, ComparisonStatus::Removed)
            } else {
                Vec::new()
            };
            let accepted_failures = if options.force && options.accept_failures {
                approval::pending_names_with_status(&facts, ComparisonStatus::Failed)
            } else {
                Vec::new()
            };

            // 却下された比較があれば baseline を更新しない。
            let rejected = approval::rejected_names(&facts);
            if !rejected.is_empty() {
                return Err(AppError::ConflictDetail(format!(
                    "cannot approve: {} comparison(s) are rejected: {}; \
                     reject the build, or re-review these comparisons as approved.",
                    rejected.len(),
                    approval::summarize_names(&rejected, MAX_REPORTED_NAMES)
                )));
            }

            // 未レビューの削除・比較失敗を一括承認の対象から除く。
            let blocking = approval::blocking_pending_names(&facts, options);
            if !blocking.is_empty() {
                let hint = if options.force {
                    "story removals require accept_removals; failed comparisons require accept_failures"
                } else {
                    "review them, or set force to bulk-approve"
                };
                return Err(AppError::ConflictDetail(format!(
                    "cannot approve: {} comparison(s) are still awaiting review: {}; {hint}.",
                    blocking.len(),
                    approval::summarize_names(&blocking, MAX_REPORTED_NAMES)
                )));
            }

            if options.force {
                approve_all_pending(txn, build.id, reviewer_id, options).await?;
                facts = load_comparison_facts(txn, build.id).await?;
            }

            let now = Utc::now().fixed_offset();

            // このビルドの全スクリーンショットを新 baseline のエントリにする。
            let shots = screenshots::Entity::find()
                .filter(screenshots::Column::BuildId.eq(build.id))
                .order_by_asc(screenshots::Column::Name)
                .all(txn)
                .await?;

            // 現行 baseline から予期せず欠落した story を検出する。
            // 「消えてよい」と承認された story 以外が今回のビルドから欠けていたら、
            // 撮影漏れ・アップロード失敗と区別がつかないので承認しない。
            let mut removed_by_revert = Vec::new();
            if let Some(baseline) = &current {
                let baseline_names: Vec<String> = crate::baselines::entries(txn, baseline.id)
                    .await?
                    .into_iter()
                    .map(|entry| entry.name)
                    .collect();
                let shot_names: HashSet<String> =
                    shots.iter().map(|shot| shot.name.clone()).collect();

                // 実際の世代巻き戻しに限り、新しい baseline で追加された story は
                // 古い build の比較に removal として存在し得ないため、専用の証跡で許可する。
                if reverted_from_build.is_some() {
                    removed_by_revert = baseline_names
                        .iter()
                        .filter(|name| !shot_names.contains(*name))
                        .cloned()
                        .collect();
                    removed_by_revert.sort();
                }
                let mut allowed_missing = approval::approved_removal_names(&facts);
                allowed_missing.extend(removed_by_revert.iter().cloned());
                let missing = approval::unexpected_missing_names(
                    &baseline_names,
                    &shot_names,
                    &allowed_missing,
                );
                if !missing.is_empty() {
                    return Err(AppError::ConflictDetail(format!(
                        "cannot approve: {} story/stories in the current baseline are missing \
                         from this build without an approved removal: {}; \
                         re-run the build, review the removals explicitly, or use force with \
                         accept_removals after confirming every removal.",
                        missing.len(),
                        approval::summarize_names(&missing, MAX_REPORTED_NAMES)
                    )));
                }
            }

            let baseline = baselines::ActiveModel {
                id: Set(Uuid::new_v4()),
                project_id: Set(build.project_id),
                branch: Set(build.branch.clone()),
                source_build_id: Set(Some(build.id)),
                created_at: Set(now),
            }
            .insert(txn)
            .await?;

            for shot in shots {
                baseline_entries::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    baseline_id: Set(baseline.id),
                    name: Set(shot.name),
                    storage_key: Set(shot.storage_key),
                    width: Set(shot.width),
                    height: Set(shot.height),
                }
                .insert(txn)
                .await?;
            }

            // 承認時刻は `approved_at`。`completed_at`（自動処理が終わった時刻）は
            // 比較フェーズで打ったものを保持する。未設定の古い行だけ埋める。
            let backfill = build.completed_at.is_none();
            let build_number = build.number;
            let previous_approval_evidence = build.approval_evidence.clone();
            let superseded_approved_by = build.approved_by;
            let superseded_approved_at = build.approved_at;
            let mut active: builds::ActiveModel = build.into();
            active.status = Set(BuildStatus::Approved);
            active.approved_by = Set(Some(reviewer_id));
            active.approved_at = Set(Some(now));
            // 通常承認は専用列だけを更新し、evidence を作らない。明示的な例外操作
            // （revert/removal/failure の受容）だけを監査履歴へ追記する。
            if reverted_from_build.is_some()
                || !accepted_removals.is_empty()
                || !accepted_failures.is_empty()
            {
                let mut evidence = serde_json::json!({
                    "accepted_removals": accepted_removals,
                    "accepted_failures": accepted_failures,
                    "removed_by_revert": removed_by_revert,
                    "reverted_from_build": reverted_from_build,
                    // 対象番号は巻き戻し時だけ意味を持つ。map で暗黙に流用しない。
                    "reverted_to_build": if reverted_from_build.is_some() {
                        Some(build_number)
                    } else {
                        None
                    },
                });
                if reverted_from_build.is_some()
                    && let Some(fields) = evidence.as_object_mut()
                {
                    // Approved build の再承認で上書きされる元の承認情報を、同じ
                    // revert イベントから復元できるようにする。
                    fields.insert(
                        "superseded_approved_by".to_string(),
                        serde_json::json!(superseded_approved_by),
                    );
                    fields.insert(
                        "superseded_approved_at".to_string(),
                        serde_json::json!(superseded_approved_at),
                    );
                }
                active.approval_evidence = Set(Some(append_approval_evidence(
                    previous_approval_evidence,
                    evidence,
                )));
            }
            if backfill {
                active.completed_at = Set(Some(now));
            }
            Ok(active.update(txn).await?)
        })
    })
    .await
}

/// 承認証跡は操作ごとの配列として保持し、再承認で過去の判断を失わない。
/// 単一 object で保存済みの行も、次回更新時に履歴の先頭要素として移行する。
fn append_approval_evidence(
    previous: Option<serde_json::Value>,
    evidence: serde_json::Value,
) -> serde_json::Value {
    match previous {
        None => serde_json::Value::Array(vec![evidence]),
        Some(serde_json::Value::Array(mut entries)) => {
            entries.push(evidence);
            serde_json::Value::Array(entries)
        }
        Some(legacy) => serde_json::Value::Array(vec![legacy, evidence]),
    }
}

/// 承認判定に使う比較の情報を読み込む。
async fn load_comparison_facts<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
) -> Result<Vec<ComparisonFacts>, AppError> {
    Ok(crate::comparisons::list_for_build(db, build_id)
        .await?
        .iter()
        .map(ComparisonFacts::from)
        .collect())
}

/// 未レビューの比較をまとめて approved にする（一括承認）。
///
/// `removed` / `failed` は各専用フラグを明示したときだけ対象に含める。
async fn approve_all_pending<C: ConnectionTrait>(
    db: &C,
    build_id: Uuid,
    reviewer_id: Uuid,
    options: ApproveOptions,
) -> Result<(), AppError> {
    let bulk_statuses: Vec<ComparisonStatus> = [
        ComparisonStatus::Changed,
        ComparisonStatus::Added,
        ComparisonStatus::Removed,
        ComparisonStatus::Failed,
    ]
    .into_iter()
    .filter(|status| approval::is_bulk_approvable(*status, options))
    .collect();
    if bulk_statuses.is_empty() {
        return Ok(());
    }

    let now = Utc::now().fixed_offset();
    comparisons::Entity::update_many()
        .filter(comparisons::Column::Status.is_in(bulk_statuses))
        .col_expr(
            comparisons::Column::ReviewStatus,
            sea_orm::sea_query::Expr::value(ReviewStatus::Approved),
        )
        .col_expr(
            comparisons::Column::ReviewedBy,
            sea_orm::sea_query::Expr::value(reviewer_id),
        )
        .col_expr(
            comparisons::Column::ReviewedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            comparisons::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(comparisons::Column::BuildId.eq(build_id))
        .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
        .exec(db)
        .await?;
    Ok(())
}

/// ビルドを却下する（baseline は更新しない）。
pub async fn reject_build(
    db: &DatabaseConnection,
    build: builds::Model,
    reviewer_id: Uuid,
) -> Result<builds::Model, AppError> {
    if !build.status.can_transition_to(BuildStatus::Rejected) {
        return Err(AppError::ConflictDetail(format!(
            "cannot reject: build #{} has status {:?}, which cannot transition to rejected. \
             Only a build with detected changes can be rejected.",
            build.number, build.status
        )));
    }
    with_transaction(db, move |txn| {
        Box::pin(async move {
            // comparison review / approve と同じ build 行を最初にロックし、状態を読み直す。
            let build = crate::review_lock::build(txn, build.id).await?;
            if !build.status.can_transition_to(BuildStatus::Rejected) {
                return Err(AppError::Conflict);
            }

            let now = Utc::now().fixed_offset();
            let build_id = build.id;

            // 却下は比較フェーズの完了時刻を動かさない（承認と同じ方針）。
            // 却下の時刻は比較ごとの `reviewed_at` に残る。
            let backfill = build.completed_at.is_none();
            let mut active: builds::ActiveModel = build.into();
            active.status = Set(BuildStatus::Rejected);
            if backfill {
                active.completed_at = Set(Some(now));
            }
            let updated = active.update(txn).await?;

            // 未レビューの比較は同じ transaction 内で rejected に倒す。
            comparisons::Entity::update_many()
                .col_expr(
                    comparisons::Column::ReviewStatus,
                    sea_orm::sea_query::Expr::value(ReviewStatus::Rejected),
                )
                .col_expr(
                    comparisons::Column::ReviewedBy,
                    sea_orm::sea_query::Expr::value(reviewer_id),
                )
                .col_expr(
                    comparisons::Column::ReviewedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .col_expr(
                    comparisons::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(comparisons::Column::BuildId.eq(build_id))
                .filter(comparisons::Column::ReviewStatus.eq(ReviewStatus::Pending))
                .exec(txn)
                .await?;

            Ok(updated)
        })
    })
    .await
}

/// 保持数の設定に従って古い完了ビルドを掃除する（ベストエフォート）。
///
/// プロジェクトの `build_retention_limit` が NULL（無制限）なら何もしない。
/// エラーはログに残すだけで呼び出し側の処理は失敗させないため、ビルド完了処理や
/// 設定更新の後処理からそのまま呼べる。
pub async fn prune_project_builds_best_effort(
    db: &DatabaseConnection,
    storage: &Arc<dyn StorageBackend>,
    project_id: Uuid,
) {
    let project = match projects::Entity::find_by_id(project_id).one(db).await {
        Ok(Some(project)) => project,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(%project_id, error = %e, "build pruning: failed to load project");
            return;
        }
    };
    let Some(limit) = project.build_retention_limit else {
        return;
    };
    match prune_old_builds(db, storage, project_id, limit).await {
        Ok(0) => {}
        Ok(deleted) => tracing::info!(%project_id, deleted, "pruned old builds"),
        Err(e) => tracing::warn!(%project_id, error = %e, "build pruning failed"),
    }
}

/// 完了（terminal 状態）ビルドを新しい順に `limit` 件残し、超過した古いものを削除する。
///
/// 削除対象からの除外:
///
/// - 現行 baseline の参照元ビルド（`baselines.source_build_id`）。baseline エントリは
///   ビルドのスクリーンショットと**同じストレージキーを共有**するため、参照元を消すと
///   baseline の実体まで失われる
/// - 進行中（非 terminal）のビルド。数えも消しもしない
///
/// 削除順序は「先に DB 行 → その後ストレージ」。DB は builds を消せば screenshots /
/// comparisons / build_logs が FK cascade で消える。ストレージ削除はベストエフォートで、
/// 失敗しても警告ログを残して続行する（既存の削除方針に合わせる）。
///
/// 戻り値は削除したビルド数。
pub async fn prune_old_builds(
    db: &DatabaseConnection,
    storage: &Arc<dyn StorageBackend>,
    project_id: Uuid,
    limit: i32,
) -> Result<u64, AppError> {
    if limit < 1 {
        return Ok(0);
    }

    // terminal 状態のビルドを新しい順に取得する。changes_detected は含めない
    // （レビュー待ちでパイプラインは終わっていないため、is_terminal と揃える）。
    let terminal = [
        BuildStatus::Passed,
        BuildStatus::Failed,
        BuildStatus::Approved,
        BuildStatus::Rejected,
    ];
    let builds = builds::Entity::find()
        .filter(builds::Column::ProjectId.eq(project_id))
        .filter(builds::Column::Status.is_in(terminal))
        .order_by_desc(builds::Column::Number)
        .all(db)
        .await?;

    if builds.len() <= limit as usize {
        return Ok(0);
    }

    // baseline に参照されているビルドは保護する。
    let protected: HashSet<Uuid> = baselines::Entity::find()
        .filter(baselines::Column::ProjectId.eq(project_id))
        .filter(baselines::Column::SourceBuildId.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .filter_map(|baseline| baseline.source_build_id)
        .collect();

    let mut deleted = 0u64;
    for build in builds.into_iter().skip(limit as usize) {
        if protected.contains(&build.id) {
            continue;
        }

        // ストレージキーは DB 削除で cascade 消去される前に集めておく。
        let shots = screenshots::Entity::find()
            .filter(screenshots::Column::BuildId.eq(build.id))
            .all(db)
            .await?;
        let diff_keys: Vec<String> = comparisons::Entity::find()
            .filter(comparisons::Column::BuildId.eq(build.id))
            .all(db)
            .await?
            .into_iter()
            .filter_map(|comparison| comparison.diff_storage_key)
            .collect();
        let storybook_key = build.storybook_key.clone();

        // 先に DB 行を消す（screenshots / comparisons / build_logs は FK cascade）。
        builds::Entity::delete_by_id(build.id).exec(db).await?;

        // ストレージ削除はベストエフォート。失敗は警告ログのみで無視する。
        for shot in &shots {
            if let Err(e) = storage.delete(&shot.storage_key).await {
                tracing::warn!(
                    build_id = %build.id,
                    key = %shot.storage_key,
                    error = %e,
                    "failed to delete pruned screenshot object"
                );
            }
        }
        for key in &diff_keys {
            if let Err(e) = storage.delete(key).await {
                tracing::warn!(
                    build_id = %build.id,
                    key = %key,
                    error = %e,
                    "failed to delete pruned diff object"
                );
            }
        }
        if let Some(key) = &storybook_key
            && let Err(e) = storage.delete(key).await
        {
            tracing::warn!(
                build_id = %build.id,
                key = %key,
                error = %e,
                "failed to delete pruned storybook bundle"
            );
        }

        deleted += 1;
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_detect_differences() {
        assert!(!BuildCounts::default().has_differences());
        assert!(
            !BuildCounts {
                total: 3,
                unchanged: 3,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 3,
                changed: 1,
                unchanged: 2,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 1,
                added: 1,
                ..Default::default()
            }
            .has_differences()
        );
        assert!(
            BuildCounts {
                total: 1,
                removed: 1,
                ..Default::default()
            }
            .has_differences()
        );
    }

    #[test]
    fn approval_evidence_appends_without_losing_existing_entries() {
        let first = serde_json::json!({ "accepted_removals": ["old"] });
        let second = serde_json::json!({ "removed_by_revert": ["new"] });
        let third = serde_json::json!({ "accepted_failures": ["broken"] });

        let history = append_approval_evidence(None, first.clone());
        let history = append_approval_evidence(Some(history), second.clone());
        assert_eq!(history, serde_json::json!([first, second]));

        let migrated =
            append_approval_evidence(Some(serde_json::json!({ "legacy": true })), third.clone());
        assert_eq!(migrated, serde_json::json!([{ "legacy": true }, third]));
    }
}
