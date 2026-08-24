use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE builds
                    ADD COLUMN failure_origin VARCHAR(255),
                    ADD COLUMN failure_code VARCHAR(255),
                    ADD CONSTRAINT builds_failure_origin_check
                        CHECK (failure_origin IS NULL OR failure_origin IN ('test', 'vrt'))",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE builds
                    DROP CONSTRAINT IF EXISTS builds_failure_origin_check,
                    DROP COLUMN IF EXISTS failure_code,
                    DROP COLUMN IF EXISTS failure_origin",
            )
            .await?;
        Ok(())
    }
}
