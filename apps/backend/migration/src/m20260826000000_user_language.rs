use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 画面表示に使う言語のユーザー単位の設定。NULL は「未設定」——
        // その場合はクライアントが `Accept-Language` / ブラウザ設定から決める。
        // 既定値を 'en' で埋めない理由は、既存ユーザーの表示を勝手に固定
        // しないため（未設定のままなら日本語ブラウザでは日本語で出る）。
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE users ADD COLUMN language VARCHAR(16)")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE users DROP COLUMN IF EXISTS language")
            .await?;
        Ok(())
    }
}
