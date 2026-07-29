use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // ジョブ（render_build / compare_build）が進捗を行単位で追記するログ。
        // id は BIGSERIAL のグローバル連番で、増分取得のカーソルにそのまま使う。
        // build ごとに分けず単一の連番にするのは、`?after=<id>` で「前回見た最後の行」
        // 以降だけを引くのに追加のソートやウィンドウ計算が要らないから。
        conn.execute_unprepared(
            r#"
            CREATE TABLE build_logs (
                id         BIGSERIAL PRIMARY KEY,
                build_id   UUID NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
                level      TEXT NOT NULL,
                message    TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;
        // (build_id, id) の複合インデックス。1 ビルドのログを id 昇順で
        // `id > after` で引くクエリがインデックスだけで完結する。
        conn.execute_unprepared("CREATE INDEX idx_build_logs_build_id ON build_logs(build_id, id)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS build_logs CASCADE")
            .await?;
        Ok(())
    }
}
