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
        //
        // IF NOT EXISTS / IF EXISTS を付けるのは、この migration が未出荷のまま
        // 「captured_names 追加 → capture_plan 追加 + captured_names drop」の
        // 2 本から一本化された経緯があるため。旧 2 本（旧名を含む）を適用済みの
        // 開発 DB に再適用しても落ちないようにする。captured_names は finalize 時の
        // 自己申告で、宣言がアップロード実績と循環しうる（全滅時に空集合同士で
        // 一致する）ため出荷前に廃した履歴の遺物である。
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds ADD COLUMN IF NOT EXISTS capture_plan JSONB")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds DROP COLUMN IF EXISTS captured_names")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds DROP COLUMN IF EXISTS capture_plan")
            .await?;
        Ok(())
    }
}
