use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // screenshots モードの部分アップロードで CI が finalize 時に宣言した
        // 「今回撮影したスクリーンショット名」の集合（JSON 配列）。
        // NULL は全撮影（従来どおり）。集合外の baseline エントリは
        // 比較ジョブが removed ではなく流用（carry-forward）として扱う。
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds ADD COLUMN captured_names JSONB")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE builds DROP COLUMN IF EXISTS captured_names")
            .await?;
        Ok(())
    }
}
