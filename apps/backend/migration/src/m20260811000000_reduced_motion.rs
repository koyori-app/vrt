use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // storybook モードの撮影時に `prefers-reduced-motion: reduce` を
        // エミュレートするかのプロジェクト単位スイッチ。既定 OFF——有効にすると
        // 撮る絵が変わり、そのプロジェクトの baseline が一度入れ替わるため、
        // 利用者が明示的に選んだときにだけ変える。
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE projects ADD COLUMN emulate_reduced_motion BOOLEAN NOT NULL DEFAULT FALSE",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE projects DROP COLUMN IF EXISTS emulate_reduced_motion")
            .await?;
        Ok(())
    }
}
