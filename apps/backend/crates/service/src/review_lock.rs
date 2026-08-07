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
//! ## CI 取り込み経路（screenshots / storybook 両モード）
//!
//! 取り込み経路も build 行ロックを最初の排他ロックとして取り、同じ build の
//! 添付・挿入・finalize を直列化する。project 行まで取るかは経路ごとに異なる。
//!
//! - スクリーンショットの DB 挿入（`screenshots::store_ci_screenshot`）と、
//!   通常の finalize（`builds::finalize_screenshots`、および全撮影＝
//!   `only_story_ids` 無しの `builds::finalize_storybook`）は build 行だけを
//!   排他ロックする。これにより
//!   - finalize の「計画 == アップロード」検査と `processing` 遷移の間に、
//!     計画外ショットが紛れ込めない
//!
//!   を保証する。project 行は読むだけで `FOR UPDATE` しない。
//!
//! - capture plan の添付（`builds::attach_capture_plan`）と、部分レンダリング
//!   （`only_story_ids`）で baseline を固定する `builds::finalize_storybook` は、
//!   承認と同じ `build -> project` の順で project 行も排他ロックする。起点
//!   baseline を検証してから `baseline_id` に固定するまでの間に、別 build の
//!   承認（project 行をロックして baseline を進める）が割り込めないようにする
//!   ため。これにより
//!   - 添付の「アップロード済みなら 409」検査と計画書き込みの間に、並行
//!     アップロードが割り込めない（撮影結果から計画を逆算する経路の封鎖）
//!   - storybook の部分レンダリングでは「pending 再確認 → 起点 baseline の
//!     SHA 照合 → `baseline_id` の固定 → `rendering` 遷移」が 1 トランザクション
//!     になり、固定する baseline が計画の起点からずれない
//!
//!   を保証する。
//!
//! project 行まで取る経路（承認・plan 添付・部分 storybook finalize）はいずれも
//! build を先に取るので、全経路の排他ロック取得順は
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
