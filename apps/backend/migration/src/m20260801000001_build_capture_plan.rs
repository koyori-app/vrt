use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // screenshots モードの部分アップロード計画（JSON オブジェクト）。
        // 形は `{"selected_names": [...], "manifest_names": [...]}` で、
        // 撮影開始前に `POST /v1/ci/builds/{id}/plan` が書き込む。
        //
        // - `selected_names`: 今回撮影（アップロード）するスクリーンショット名
        // - `manifest_names`: 現時点で存在する全スクリーンショット名（現行 index）
        //
        // NULL は「計画なし = 全撮影」（従来どおり）。比較ジョブは selected 外かつ
        // manifest 内の baseline エントリだけを流用し、manifest から消えた名前は
        // removed として報告する（削除が流用で隠れない）。
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds ADD COLUMN capture_plan JSONB")
            .await?;

        // finalize 時の自己申告だった captured_names は撮影前に固定される
        // capture_plan に置き換える。宣言がアップロード実績と循環しうる
        // （全滅時に空集合同士で一致する）ため、選択集合の出所としては使わない。
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds DROP COLUMN IF EXISTS captured_names")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds ADD COLUMN captured_names JSONB")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds DROP COLUMN IF EXISTS capture_plan")
            .await?;
        Ok(())
    }
}
