use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE screenshots ADD COLUMN content_hash TEXT;\
                 ALTER TABLE baseline_entries ADD COLUMN content_hash TEXT;\
                 ALTER TABLE baseline_entries ADD COLUMN verified_content_hash TEXT;\
                 ALTER TABLE builds ADD COLUMN content_hash_skipped_count INTEGER NOT NULL DEFAULT 0",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE builds DROP COLUMN IF EXISTS content_hash_skipped_count;\
                 ALTER TABLE baseline_entries DROP COLUMN IF EXISTS verified_content_hash;\
                 ALTER TABLE baseline_entries DROP COLUMN IF EXISTS content_hash;\
                 ALTER TABLE screenshots DROP COLUMN IF EXISTS content_hash",
            )
            .await?;
        Ok(())
    }
}
