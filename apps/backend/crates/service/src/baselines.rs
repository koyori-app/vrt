//! baseline（承認済みスクリーンショット集合）の解決。

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, prelude::Uuid,
};

use common::db::with_transaction;
use common::error::AppError;
use entity::{baseline_entries, baselines, projects};

/// 比較に使う baseline を解決する。
///
/// 1. 同一ブランチの最新 baseline
/// 2. 無ければプロジェクトのデフォルトブランチの最新 baseline
/// 3. それも無ければ `None`（＝初回ビルド。全スクリーンショットが `added` になる）
///
/// merge 後の default branch では、PR の枝で承認した baseline が枝の名前の違いで
/// 見えなくなる。GitHub App が使えるときは、ビルドの作成時に merge 元 PR の枝の
/// baseline を default branch へ写しておく（[`inherit_from_branch`]）。写した行は
/// default branch の baseline なので、ここの解決は変えずに引き継いだものを掴む。
pub async fn latest_for<C: ConnectionTrait>(
    db: &C,
    project: &projects::Model,
    branch: &str,
) -> Result<Option<baselines::Model>, AppError> {
    if let Some(found) = latest_on_branch(db, project.id, branch).await? {
        return Ok(Some(found));
    }
    if branch != project.default_branch {
        return latest_on_branch(db, project.id, &project.default_branch).await;
    }
    Ok(None)
}

/// 指定ブランチの最新 baseline。
pub async fn latest_on_branch<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
    branch: &str,
) -> Result<Option<baselines::Model>, AppError> {
    Ok(baselines::Entity::find()
        .filter(baselines::Column::ProjectId.eq(project_id))
        .filter(baselines::Column::Branch.eq(branch))
        .order_by_desc(baselines::Column::CreatedAt)
        .order_by_desc(baselines::Column::Id)
        .one(db)
        .await?)
}

/// baseline のエントリ一覧（名前順）。
pub async fn entries<C: ConnectionTrait>(
    db: &C,
    baseline_id: Uuid,
) -> Result<Vec<baseline_entries::Model>, AppError> {
    Ok(baseline_entries::Entity::find()
        .filter(baseline_entries::Column::BaselineId.eq(baseline_id))
        .order_by_asc(baseline_entries::Column::Name)
        .all(db)
        .await?)
}

/// baseline エントリを ID で取得する。
pub async fn get_entry<C: ConnectionTrait>(
    db: &C,
    entry_id: Uuid,
) -> Result<baseline_entries::Model, AppError> {
    baseline_entries::Entity::find_by_id(entry_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// baseline を ID で取得する。
pub async fn get_baseline<C: ConnectionTrait>(
    db: &C,
    baseline_id: Uuid,
) -> Result<baselines::Model, AppError> {
    baselines::Entity::find_by_id(baseline_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// PR の枝の最新 baseline を、default branch の baseline として写す。
///
/// merge 後の default branch のビルドが、PR の枝で承認した baseline を引き継ぐための口。
/// [`latest_for`] は枝の名前でしか baseline を探さないため、merge で枝が変わると
/// PR の承認が見えなくなる。写した行は default branch の baseline になるので、以後の
/// 解決（比較・承認・plan 添付）はすべて `latest_for` のまま引き継いだものを掴む。
///
/// 写すのは、PR の枝の baseline が default branch の最新より新しいときだけ。
/// default branch がその後に承認されていれば、そちらを巻き戻さない。同じ承認を
/// 二度写さないよう、default branch の最新が既に同じ承認の写しなら何もしない。
///
/// `source_build_id` は元の承認ビルドを指したまま写す。baseline エントリは
/// そのビルドのスクリーンショットとストレージキーを共有しているので、
/// ビルドの刈り込み（参照元ビルドは消さない）が写しの実体も守る。
///
/// 承認と同じく project 行を排他ロックしてから読み直す（[`crate::review_lock`]）。
/// build 行は取らないので `build -> project` の順序は崩れない。
///
/// 写したときは写した baseline を、写さなかったときは `None` を返す。
pub async fn inherit_from_branch(
    db: &DatabaseConnection,
    project_id: Uuid,
    source_branch: &str,
) -> Result<Option<baselines::Model>, AppError> {
    let source_branch = source_branch.to_string();
    with_transaction(db, move |txn| {
        Box::pin(async move {
            let project = crate::review_lock::project(txn, project_id).await?;
            if source_branch == project.default_branch {
                return Ok(None);
            }
            let Some(source) = latest_on_branch(txn, project.id, &source_branch).await? else {
                return Ok(None);
            };
            if let Some(current) =
                latest_on_branch(txn, project.id, &project.default_branch).await?
            {
                let already_inherited = current.source_build_id.is_some()
                    && current.source_build_id == source.source_build_id;
                if already_inherited || current.created_at >= source.created_at {
                    return Ok(None);
                }
            }

            let inherited = baselines::ActiveModel {
                id: Set(Uuid::new_v4()),
                project_id: Set(project.id),
                branch: Set(project.default_branch.clone()),
                source_build_id: Set(source.source_build_id),
                created_at: Set(Utc::now().fixed_offset()),
            }
            .insert(txn)
            .await?;

            for entry in entries(txn, source.id).await? {
                baseline_entries::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    baseline_id: Set(inherited.id),
                    name: Set(entry.name),
                    storage_key: Set(entry.storage_key),
                    width: Set(entry.width),
                    height: Set(entry.height),
                    content_hash: Set(entry.content_hash),
                    verified_content_hash: Set(entry.verified_content_hash),
                }
                .insert(txn)
                .await?;
            }

            Ok(Some(inherited))
        })
    })
    .await
}
