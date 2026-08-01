//! レビュー判断と CI 取り込みを変更する経路のロック規約。
//!
//! ロック順は常に次の順序とし、後ろから前へは取得しない。
//!
//! 1. 対象 `builds` 行
//! 2. baseline を変更する承認だけ対象 `projects` 行
//! 3. 単一比較を変更するレビューだけ対象 `comparisons` 行
//!
//! build 行を三経路（比較レビュー・build 承認・build 却下）共通の mutex にすることで、
//! 同じ build の判断は直列化する。一方、別 build の比較レビューは並行できる。
//! 承認だけ project 行も取るのは、異なる build の承認による baseline 昇格を project
//! 単位で直列化するため。すべて build を先に取るので、別 build の承認同士にも
//! `build -> project` と `project -> build` の循環は生じない。
//!
//! ## CI 取り込み経路（screenshots モード）
//!
//! capture plan の添付（`builds::attach_capture_plan`）・スクリーンショットの
//! DB 挿入（`screenshots::store_ci_screenshot`）・finalize
//! （`builds::finalize_screenshots`）も、同じ build 行ロックを**最初かつ唯一の**
//! 排他ロックとして取る。これにより
//!
//! - 添付の「アップロード済みなら 409」検査と計画書き込みの間に、並行
//!   アップロードが割り込めない（撮影結果から計画を逆算する経路の封鎖）
//! - finalize の「計画 == アップロード」検査と `processing` 遷移の間に、
//!   計画外ショットが紛れ込めない
//!
//! を保証する。取り込み経路は 2 個目の排他ロックを取らない（project 行は
//! 読むだけで `FOR UPDATE` しない）ため、全経路の排他ロック取得順は
//! `build -> project -> comparison` の一方向のまま——循環は構造的に生じない。

use sea_orm::{ConnectionTrait, EntityTrait, QuerySelect, prelude::Uuid};

use common::error::AppError;
use entity::{builds, comparisons, projects};

/// レビュー判断の共通 mutex となる build 行を取得する（ロック順 1）。
pub async fn build<C: ConnectionTrait>(db: &C, build_id: Uuid) -> Result<builds::Model, AppError> {
    builds::Entity::find_by_id(build_id)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// baseline を変更する承認用の project 行を取得する（ロック順 2）。
pub async fn project<C: ConnectionTrait>(
    db: &C,
    project_id: Uuid,
) -> Result<projects::Model, AppError> {
    projects::Entity::find_by_id(project_id)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// 単一比較の更新対象を読み直す（ロック順 3）。
pub async fn comparison<C: ConnectionTrait>(
    db: &C,
    comparison_id: Uuid,
) -> Result<comparisons::Model, AppError> {
    comparisons::Entity::find_by_id(comparison_id)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}
