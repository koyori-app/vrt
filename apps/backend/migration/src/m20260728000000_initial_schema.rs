use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // ── base tables ──────────────────────────────────────────────────────

        conn.execute_unprepared(
            r#"
            CREATE TABLE users (
                id                  UUID PRIMARY KEY,
                username            VARCHAR NOT NULL UNIQUE,
                display_name        VARCHAR NOT NULL,
                avatar_url          TEXT,
                email               VARCHAR UNIQUE,
                sessions_revoked_at TIMESTAMPTZ,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;

        // ── auth ─────────────────────────────────────────────────────────────

        conn.execute_unprepared(
            r#"
            CREATE TABLE oauth_connections (
                id               UUID PRIMARY KEY,
                user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                provider         VARCHAR NOT NULL,
                provider_user_id VARCHAR NOT NULL,
                access_token_enc TEXT,
                created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
                UNIQUE (provider, provider_user_id)
            )
        "#,
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_oauth_connections_user ON oauth_connections(user_id)",
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE personal_tokens (
                id              UUID PRIMARY KEY,
                name            VARCHAR NOT NULL,
                token_last_four VARCHAR NOT NULL,
                token_hash      VARCHAR NOT NULL,
                expires_at      TIMESTAMPTZ,
                last_used_at    TIMESTAMPTZ,
                revoked         BOOLEAN NOT NULL DEFAULT false,
                user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                scopes          JSONB NOT NULL DEFAULT '[]'::jsonb,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_personal_tokens_token_hash ON personal_tokens(token_hash)",
        )
        .await?;

        // ── multi-tenancy ────────────────────────────────────────────────────

        conn.execute_unprepared(
            r#"
            CREATE TABLE tenants (
                id         UUID PRIMARY KEY,
                name       VARCHAR NOT NULL,
                slug       VARCHAR NOT NULL UNIQUE,
                avatar_url TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE tenant_members (
                id         UUID PRIMARY KEY,
                tenant_id  UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
                user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role       VARCHAR(255) NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                CONSTRAINT tenant_members_tenant_id_user_id_key UNIQUE (tenant_id, user_id)
            )
        "#,
        )
        .await?;
        conn.execute_unprepared("CREATE INDEX idx_tenant_members_user ON tenant_members(user_id)")
            .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE projects (
                id                     UUID PRIMARY KEY,
                tenant_id              UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
                name                   VARCHAR NOT NULL,
                slug                   VARCHAR NOT NULL,
                default_branch         VARCHAR NOT NULL DEFAULT 'main',
                diff_threshold         DOUBLE PRECISION NOT NULL DEFAULT 0.1,
                diff_ratio_fail        DOUBLE PRECISION NOT NULL DEFAULT 0.0,
                -- github_installations.installation_id を指すが FK は張らない。
                -- installation は論理削除（deleted_at）で行が残る運用のため FK に
                -- 守らせるものが無く、テスト DB を作る entity のスキーマ同期
                -- （`get_schema_registry("entity::*").sync()`）にも FK が出ないため、
                -- マイグレーションと entity の定義を一致させることを優先する。
                github_installation_id BIGINT,
                github_repo            TEXT,
                created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
                CONSTRAINT projects_tenant_id_slug_key UNIQUE (tenant_id, slug)
            )
        "#,
        )
        .await?;
        conn.execute_unprepared("CREATE INDEX idx_projects_tenant ON projects(tenant_id)")
            .await?;

        // ── GitHub App ───────────────────────────────────────────────────────

        // installation の行を作るのは webhook (`installation.created`) だけ。
        // tenant_id は claim されるまで NULL、アンインストールは deleted_at の論理削除。
        conn.execute_unprepared(
            r#"
            CREATE TABLE github_installations (
                id              UUID PRIMARY KEY,
                tenant_id       UUID REFERENCES tenants(id) ON DELETE SET NULL,
                installation_id BIGINT NOT NULL UNIQUE,
                account_login   TEXT NOT NULL,
                account_type    TEXT NOT NULL,
                suspended_at    TIMESTAMPTZ,
                deleted_at      TIMESTAMPTZ,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_github_installations_tenant ON github_installations(tenant_id)",
        )
        .await?;

        // ── VRT core ─────────────────────────────────────────────────────────

        // ビルド番号の採番用カウンタ。`INSERT ... ON CONFLICT DO UPDATE ... RETURNING`
        // で原子的に加算するため、番号に欠番が出ない。
        conn.execute_unprepared(
            r#"
            CREATE TABLE project_build_counters (
                project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                counter    BIGINT NOT NULL DEFAULT 0
            )
        "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE builds (
                id                  UUID PRIMARY KEY,
                project_id          UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                number              BIGINT NOT NULL,
                branch              VARCHAR NOT NULL,
                commit_sha          VARCHAR NOT NULL,
                commit_message      TEXT,
                pull_request_number INTEGER,
                status              VARCHAR(255) NOT NULL,
                -- baselines.source_build_id との循環参照を避けるため FK は張らない。
                baseline_id         UUID,
                total_count         INTEGER NOT NULL DEFAULT 0,
                changed_count       INTEGER NOT NULL DEFAULT 0,
                added_count         INTEGER NOT NULL DEFAULT 0,
                removed_count       INTEGER NOT NULL DEFAULT 0,
                unchanged_count     INTEGER NOT NULL DEFAULT 0,
                error_message       TEXT,
                approved_by         UUID REFERENCES users(id) ON DELETE SET NULL,
                approved_at         TIMESTAMPTZ,
                created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
                completed_at        TIMESTAMPTZ,
                CONSTRAINT builds_project_id_number_key UNIQUE (project_id, number)
            )
        "#,
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_builds_project_created ON builds(project_id, created_at DESC)",
        )
        .await?;
        conn.execute_unprepared("CREATE INDEX idx_builds_branch ON builds(project_id, branch)")
            .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE screenshots (
                id          UUID PRIMARY KEY,
                build_id    UUID NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
                name        VARCHAR NOT NULL,
                storage_key TEXT NOT NULL,
                width       INTEGER NOT NULL,
                height      INTEGER NOT NULL,
                metadata    JSONB,
                created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
                CONSTRAINT screenshots_build_id_name_key UNIQUE (build_id, name)
            )
        "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE baselines (
                id              UUID PRIMARY KEY,
                project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                branch          VARCHAR NOT NULL,
                source_build_id UUID REFERENCES builds(id) ON DELETE SET NULL,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX idx_baselines_project_branch_created
             ON baselines(project_id, branch, created_at DESC)",
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE baseline_entries (
                id          UUID PRIMARY KEY,
                baseline_id UUID NOT NULL REFERENCES baselines(id) ON DELETE CASCADE,
                name        VARCHAR NOT NULL,
                storage_key TEXT NOT NULL,
                width       INTEGER NOT NULL,
                height      INTEGER NOT NULL,
                CONSTRAINT baseline_entries_baseline_id_name_key UNIQUE (baseline_id, name)
            )
        "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            CREATE TABLE comparisons (
                id                UUID PRIMARY KEY,
                build_id          UUID NOT NULL REFERENCES builds(id) ON DELETE CASCADE,
                name              VARCHAR NOT NULL,
                screenshot_id     UUID REFERENCES screenshots(id) ON DELETE SET NULL,
                baseline_entry_id UUID REFERENCES baseline_entries(id) ON DELETE SET NULL,
                status            VARCHAR(255) NOT NULL,
                review_status     VARCHAR(255) NOT NULL,
                diff_storage_key  TEXT,
                diff_pixel_count  BIGINT,
                diff_ratio        DOUBLE PRECISION,
                error_message     TEXT,
                reviewed_by       UUID REFERENCES users(id) ON DELETE SET NULL,
                reviewed_at       TIMESTAMPTZ,
                created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
                CONSTRAINT comparisons_build_id_name_key UNIQUE (build_id, name)
            )
        "#,
        )
        .await?;
        conn.execute_unprepared("CREATE INDEX idx_comparisons_build ON comparisons(build_id)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let tables = [
            "comparisons",
            "baseline_entries",
            "baselines",
            "screenshots",
            "builds",
            "project_build_counters",
            "github_installations",
            "projects",
            "tenant_members",
            "tenants",
            "personal_tokens",
            "oauth_connections",
            "users",
        ];
        for table in tables {
            conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table} CASCADE"))
                .await?;
        }
        Ok(())
    }
}
