use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // プロジェクトごとに保持する完了ビルド数の上限。NULL は無制限（既定）。
        // 値は 1 以上（バリデーションはアプリ層の `service::projects::update_project`）。
        // 超過した古い完了ビルドは `service::builds::prune_old_builds` が自動削除する。
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects ADD COLUMN build_retention_limit INTEGER",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE projects DROP COLUMN IF EXISTS build_retention_limit")
            .await?;
        Ok(())
    }
}
