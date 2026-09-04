use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 再比較（`POST /v1/builds/{id}/recompare`）が queued へ戻した時刻。
        //
        // この列が要るのは、リセット後の行が新規 finalize 直後・retry 直後の
        // queued と**署名が完全に一致する**ため。baseline_id NULL / カウント 0 /
        // completed_at NULL / エラー無しはどの経路でも同じで、行の状態からは
        // 由来を区別できない。区別できないまま「queued のビルドも再比較を
        // 受け付ける」（ジョブ投入失敗やワーカー入れ替えで queued のまま
        // 止まったビルドの回収経路）を開くと、まだ一度もレンダリングして
        // いない storybook ビルドへ比較ジョブを撃ち込めてしまい、
        // 「screenshots 0 枚 vs baseline = 全 removed」を確定させたうえに
        // render ジョブが handoff 修復経路へ落ちて撮影が永久に走らなくなる。
        //
        // パイプラインが完走した時点（`service::builds::transition`）で NULL に戻す。
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE builds ADD COLUMN recompare_requested_at TIMESTAMPTZ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE builds DROP COLUMN IF EXISTS recompare_requested_at",
            )
            .await?;
        Ok(())
    }
}
