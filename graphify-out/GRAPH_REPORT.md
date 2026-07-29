# Graph Report - vrt  (2026-07-29)

## Corpus Check
- 180 files · ~79,556 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 2037 nodes · 4379 edges · 141 communities (106 shown, 35 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 56 edges (avg confidence: 0.75)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `27360b74`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- vrt_flow_integration.rs
- render_flow_integration.rs
- AppError
- routeTree.gen.ts
- browser.rs
- Model
- src/auth.rs
- service/src/github.rs
- Settings
- JobState
- PersonalTokenResponse
- post-commit
- AppState
- post-checkout
- api.ts
- pixelmatch.rs
- handlers/github.rs
- bundle.rs
- cn
- service/src/screenshots.rs
- アーキテクチャ
- compare_build.rs
- BuildResponse
- devDependencies
- t.$tenantSlug.index.tsx
- S3StorageBackend
- compilerOptions
- dependencies
- AuthUser
- .new_with
- TestApp
- store_screenshot_with_metadata
- csrf.rs
- github_integration.rs
- components.json
- api.$.ts
- load_build_with_role
- e2e/package.json
- TenantResponse
- __root.tsx
- status-badge.tsx
- compilerOptions
- ComparisonResponse
- service/src/comparisons.rs
- server.rs
- upload_screenshot
- StorageError
- tenants_integration.rs
- button.tsx
- auth_oauth_integration.rs
- MockGithub
- validation.rs
- Response
- projects_integration.rs
- package.json
- Model
- personal_tokens_integration.rs
- scripts
- global-setup.ts
- Model
- Model
- review_comparison
- get_baseline_entry_content
- MeResponse
- Model
- Model
- Model
- Model
- Model
- Migration
- VRT frontend
- Model
- .migrations
- Model
- logging_middleware
- ensure-database.mjs
- entity/src/comparisons.rs
- Model
- TenantRole
- health.rs
- create_http_client
- main
- main
- frontend/package.json
- .prettierrc.json
- setup_storage
- seaorm_postprocess.sh
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- ActiveModel
- run-migration.sh
- lucide-react
- openapi-react-query
- radix-ui
- @radix-ui/react-dropdown-menu
- @radix-ui/react-select
- @radix-ui/react-slider
- @radix-ui/react-slot
- @radix-ui/react-tabs
- react-dom
- shadcn
- srvx
- tailwind-merge
- @tanstack/react-query
- server.mjs
- start-backend.sh
- seaorm_generate.sh
- t.$tenantSlug.p.$projectSlug.index.tsx

## God Nodes (most connected - your core abstractions)
1. `AppError` - 133 edges
2. `AppState` - 116 edges
3. `cn()` - 71 edges
4. `AuthUser` - 59 edges
5. `TestApp` - 55 edges
6. `JobState` - 25 edges
7. `AuthError` - 22 edges
8. `diff_images()` - 21 edges
9. `StorageBackend` - 21 edges
10. `load_build_with_role()` - 19 edges

## Surprising Connections (you probably didn't know these)
- `approve_build()` --calls--> `with_transaction()`  [INFERRED]
  apps/backend/crates/service/src/builds.rs → apps/backend/crates/common/src/db.rs
- `create_tenant()` --calls--> `with_transaction()`  [INFERRED]
  apps/backend/crates/service/src/tenants.rs → apps/backend/crates/common/src/db.rs
- `validate_slug()` --calls--> `check_slug()`  [INFERRED]
  apps/backend/crates/service/src/tenants.rs → apps/backend/crates/common/src/validation.rs
- `oauth_callback()` --calls--> `encrypt_oauth_token()`  [INFERRED]
  apps/backend/crates/handler/src/handlers/auth.rs → apps/backend/crates/service/src/auth.rs
- `oauth_callback()` --calls--> `upsert_oauth_user()`  [INFERRED]
  apps/backend/crates/handler/src/handlers/auth.rs → apps/backend/crates/service/src/auth.rs

## Import Cycles
- 2-file cycle: `apps/backend/crates/job/src/compare_build.rs -> apps/backend/crates/job/src/lib.rs -> apps/backend/crates/job/src/compare_build.rs`
- 2-file cycle: `apps/backend/crates/job/src/github_status.rs -> apps/backend/crates/job/src/lib.rs -> apps/backend/crates/job/src/github_status.rs`
- 2-file cycle: `apps/backend/crates/job/src/lib.rs -> apps/backend/crates/job/src/render_build.rs -> apps/backend/crates/job/src/lib.rs`
- 2-file cycle: `apps/backend/crates/job/src/github_webhook.rs -> apps/backend/crates/job/src/lib.rs -> apps/backend/crates/job/src/github_webhook.rs`

## Communities (141 total, 35 thin omitted)

### Community 0 - "vrt_flow_integration.rs"
Cohesion: 0.15
Nodes (28): assert_completed_at_is_stamped(), baseline_entry_count(), build_can_be_fetched_by_project_scoped_number(), build_id_of(), counts(), dump_apalis_state(), duplicate_screenshot_name_is_conflict(), encode() (+20 more)

### Community 1 - "render_flow_integration.rs"
Cohesion: 0.20
Nodes (20): a_bundle_without_an_index_fails_the_build_with_a_reason(), assert_completed_at_is_stamped(), build_id_of(), bundle_zip(), bundles_larger_than_the_default_body_limit_are_accepted(), chromium_or_skip(), Fixture, iframe_html() (+12 more)

### Community 2 - "AppError"
Cohesion: 0.08
Nodes (82): AppError, DbErr, From, IntoResponse, Self, entries(), get_baseline(), get_entry() (+74 more)

### Community 3 - "routeTree.gen.ts"
Cohesion: 0.07
Nodes (35): meQueryOptions(), getRouter(), Register, @tanstack/react-router, Route, Route, Route, Route (+27 more)

### Community 4 - "browser.rs"
Cohesion: 0.06
Nodes (52): internal_server_error(), Error, Json, Response, StatusCode, String, ServerError, register_schema() (+44 more)

### Community 5 - "Model"
Cohesion: 0.09
Nodes (21): BuildMode, BuildStatus, Model, BuildStatus, DateTimeWithTimeZone, Entity, HasMany, HasOne (+13 more)

### Community 6 - "src/auth.rs"
Cohesion: 0.05
Nodes (69): bind_sql(), column_exists(), connect_database(), db_max_connections(), execute_bound(), is_postgres_unique_violation(), query_one_bool(), C (+61 more)

### Community 7 - "service/src/github.rs"
Cohesion: 0.05
Nodes (62): RedisConnection, Error, Result, Self, build_storage(), build_storage_for_queue(), enqueue(), enqueue_best_effort() (+54 more)

### Community 8 - "Settings"
Cohesion: 0.05
Nodes (56): base_settings(), check(), default_allow_origin(), default_listen_addr(), default_local_upload_dir(), default_storage_backend(), default_storybook_cache_dir(), load_settings() (+48 more)

### Community 9 - "JobState"
Cohesion: 0.08
Nodes (64): build_storage(), build_storage_for_queue(), delete_installation(), enqueue(), GithubWebhookJob, process(), Arc, BoxDynError (+56 more)

### Community 10 - "PersonalTokenResponse"
Cohesion: 0.06
Nodes (45): Model, DateTimeWithTimeZone, Entity, HasOne, Option, String, Uuid, read_build_does_not_imply_write_build() (+37 more)

### Community 11 - "post-commit"
Cohesion: 0.40
Nodes (4): post-commit script, GRAPHIFY_CHANGED, GRAPHIFY_REBUILD_LOG, PYTHONHASHSEED

### Community 12 - "AppState"
Cohesion: 0.07
Nodes (33): AppState, Arc, Client, CompareBuildStorage, DatabaseConnection, GithubStatusStorage, GithubWebhookStorage, PgPool (+25 more)

### Community 13 - "post-checkout"
Cohesion: 0.50
Nodes (3): post-checkout script, GRAPHIFY_REBUILD_LOG, PYTHONHASHSEED

### Community 15 - "api.ts"
Cohesion: 0.10
Nodes (25): Build, client, GithubInstallation, Me, onRequest(), PersonalToken, Project, resolveBaseUrl (+17 more)

### Community 16 - "pixelmatch.rs"
Cohesion: 0.12
Nodes (33): alpha_difference_is_detected(), antialiased(), antialiasing_is_detected_and_not_counted(), background_is_dimmed_grayscale_of_baseline(), blend(), color_delta(), color_delta_is_zero_for_identical_pixels(), color_delta_sign_encodes_direction() (+25 more)

### Community 17 - "handlers/github.rs"
Cohesion: 0.06
Nodes (60): claim_installation(), github_webhook(), list_installations(), list_unclaimed_installations(), rejects_missing_prefix(), Bytes, HeaderMap, Json (+52 more)

### Community 18 - "bundle.rs"
Cohesion: 0.06
Nodes (66): BundleError, docs_only_index_yields_no_stories(), extract_and_index(), extract_zip(), extract_zip_with_limits(), ExtractedBundle, ExtractLimits, extracts_bundle_and_lists_stories() (+58 more)

### Community 19 - "cn"
Cohesion: 0.10
Nodes (26): ComparisonViewer(), Frame(), ComparisonStatusBadge(), ReviewStatusBadge(), TopNav(), Checkbox(), DropdownMenu(), DropdownMenuCheckboxItem() (+18 more)

### Community 20 - "service/src/screenshots.rs"
Cohesion: 0.18
Nodes (20): accepts_valid_png(), encode_png(), load_rgba(), one_shot_stream(), open_stream(), png_bytes(), png_roundtrips_through_encode(), read_all() (+12 more)

### Community 21 - "アーキテクチャ"
Cohesion: 0.05
Nodes (33): backend のクレート依存グラフ, OpenAPI パイプライン, VRT の状態機械, アーキテクチャ, ストレージ, ビルド, モノレポ構成, レンダリングジョブ（storybook モード） (+25 more)

### Community 22 - "compare_build.rs"
Cohesion: 0.13
Nodes (31): build_storage(), build_storage_for_queue(), compare_pair(), CompareBuildJob, enqueue(), entry(), full_outer_join_marks_added_and_removed(), join_by_name() (+23 more)

### Community 23 - "BuildResponse"
Cohesion: 0.18
Nodes (17): ApproveBuildRequest, BuildListQuery, BuildListResponse, BuildResponse, CreateBuildRequest, BuildStatus, DateTime, From (+9 more)

### Community 24 - "devDependencies"
Cohesion: 0.07
Nodes (27): devDependencies, openapi-typescript, prettier, tailwindcss, @tailwindcss/vite, @tanstack/react-query-devtools, @tanstack/react-router-devtools, tw-animate-css (+19 more)

### Community 25 - "t.$tenantSlug.index.tsx"
Cohesion: 0.23
Nodes (11): CreateTenantDialog(), Dialog(), DialogContent(), DialogDescription(), DialogFooter(), DialogHeader(), DialogOverlay(), DialogTitle() (+3 more)

### Community 26 - "S3StorageBackend"
Cohesion: 0.13
Nodes (17): delete_rejects_invalid_key(), dummy_backend(), get_stream_rejects_invalid_key(), mime_attributes(), Arc, ByteStream, Debug, Formatter (+9 more)

### Community 27 - "compilerOptions"
Cohesion: 0.08
Nodes (25): compilerOptions, esModuleInterop, isolatedModules, jsx, lib, module, moduleDetection, moduleResolution (+17 more)

### Community 28 - "dependencies"
Cohesion: 0.08
Nodes (25): dependencies, class-variance-authority, clsx, @fontsource-variable/geist, next-themes, openapi-fetch, @radix-ui/react-checkbox, @radix-ui/react-dialog (+17 more)

### Community 29 - "AuthUser"
Cohesion: 0.32
Nodes (21): AuthMethod, AuthUser, Uuid, add_member(), create_tenant(), delete_tenant(), get_tenant(), list_members() (+13 more)

### Community 30 - ".new_with"
Cohesion: 0.15
Nodes (15): ensure_schema(), ensure_test_env(), init_tracing(), is_redirect(), DatabaseConnection, Option, Self, StatusCode (+7 more)

### Community 31 - "TestApp"
Cohesion: 0.16
Nodes (8): Client, Model, Scope, Uuid, Vec, TestApp, Fn, Sender

### Community 32 - "store_screenshot_with_metadata"
Cohesion: 0.31
Nodes (13): diff_key(), get_screenshot(), list_for_build(), C, Model, Option, String, Uuid (+5 more)

### Community 33 - "csrf.rs"
Cohesion: 0.10
Nodes (11): csrf_origin_check(), has_bearer_token(), headers_with_authorization(), origin_allowed(), Body, HeaderMap, Next, Option (+3 more)

### Community 34 - "github_integration.rs"
Cohesion: 0.24
Nodes (21): build_flow_completes_without_github_app_configured(), build_lifecycle_posts_commit_statuses_to_github(), claim_flow_enforces_roles_and_single_tenant_ownership(), create_tenant_and_project(), installation_deleted_soft_deletes_row_and_unlinks_projects(), installation_payload(), installation_suspend_and_unsuspend_toggle_suspended_at(), png() (+13 more)

### Community 35 - "components.json"
Cohesion: 0.09
Nodes (21): aliases, components, hooks, lib, ui, utils, iconLibrary, menuAccent (+13 more)

### Community 36 - "api.$.ts"
Cohesion: 0.27
Nodes (10): BodyTooLargeError, buildBackendUrl(), copyHeaders(), handler(), HOP_BY_HOP, LimitedReadableStream, limitReadableStream(), maxBodyBytesForPath() (+2 more)

### Community 37 - "load_build_with_role"
Cohesion: 0.35
Nodes (20): approve_build(), get_build(), get_build_by_number(), list_builds(), list_comparisons(), load_baseline_entry_with_role(), load_build_with_role(), load_comparison_with_role() (+12 more)

### Community 38 - "e2e/package.json"
Cohesion: 0.10
Nodes (19): devDependencies, pg, @playwright/test, pngjs, @types/node, @types/pngjs, @types/node, name (+11 more)

### Community 39 - "TenantResponse"
Cohesion: 0.26
Nodes (15): AddMemberRequest, CreateTenantRequest, DateTime, From, Model, Option, Self, String (+7 more)

### Community 41 - "status-badge.tsx"
Cohesion: 0.13
Nodes (23): ComparisonFilter, ComparisonList(), filterComparisons(), FILTERS, useComparisonFilter(), BuildStatusBadge(), Badge(), badgeVariants (+15 more)

### Community 42 - "compilerOptions"
Cohesion: 0.11
Nodes (18): compilerOptions, allowJs, lib, module, moduleResolution, noEmit, skipLibCheck, strict (+10 more)

### Community 43 - "ComparisonResponse"
Cohesion: 0.15
Nodes (15): ComparisonListResponse, ComparisonResponse, ReviewActionRequest, ReviewComparisonRequest, ComparisonStatus, DateTime, From, Model (+7 more)

### Community 44 - "service/src/comparisons.rs"
Cohesion: 0.22
Nodes (13): delete_for_build(), get_comparison(), initial_review_status(), list_for_build(), review(), ReviewAction, C, ComparisonStatus (+5 more)

### Community 45 - "server.rs"
Cohesion: 0.37
Nodes (16): job_state_from(), Box, Error, JoinHandle, Result, String, run(), shutdown_signal_inner() (+8 more)

### Community 46 - "upload_screenshot"
Cohesion: 0.36
Nodes (15): CiPingResponse, create_build(), finalize_build(), get_build_status(), ping(), Json, Path, Result (+7 more)

### Community 47 - "StorageError"
Cohesion: 0.27
Nodes (10): LocalStorageBackend, ByteStream, Into, PathBuf, Result, Self, validate_key(), Error (+2 more)

### Community 49 - "tenants_integration.rs"
Cohesion: 0.27
Nodes (13): create_tenant(), last_owner_cannot_be_demoted_or_removed(), list_carries_my_role_and_members_carry_user_profiles(), member_role(), non_member_is_denied_and_sees_nothing(), role_matrix_governs_member_management(), Option, String (+5 more)

### Community 50 - "button.tsx"
Cohesion: 0.24
Nodes (12): Button(), buttonVariants, Card(), CardAction(), CardContent(), CardDescription(), CardFooter(), CardHeader() (+4 more)

### Community 51 - "auth_oauth_integration.rs"
Cohesion: 0.18
Nodes (4): callback_from_a_different_session_is_rejected(), login_redirect_carries_state_and_pkce_challenge(), replayed_state_is_rejected(), location_of()

### Community 52 - "MockGithub"
Cohesion: 0.21
Nodes (6): MockGithub, MockProvider, Duration, Request, String, MockServer

### Community 53 - "validation.rs"
Cohesion: 0.21
Nodes (6): check_slug(), is_reserved_slug(), Display, Formatter, Result, SlugError

### Community 55 - "projects_integration.rs"
Cohesion: 0.36
Nodes (11): create_project(), create_tenant(), cross_tenant_project_access_is_denied(), diff_threshold_out_of_range_is_rejected(), pat_read_project_scope_gates_project_reads(), project_crud_within_tenant(), project_slug_is_unique_per_tenant_and_validated(), project_writes_require_admin_or_owner() (+3 more)

### Community 56 - "package.json"
Cohesion: 0.18
Nodes (10): husky, lint-staged, devDependencies, husky, lint-staged, name, packageManager, private (+2 more)

### Community 57 - "Model"
Cohesion: 0.20
Nodes (9): Model, ComparisonStatus, DateTimeWithTimeZone, Entity, HasOne, Option, ReviewStatus, String (+1 more)

### Community 58 - "personal_tokens_integration.rs"
Cohesion: 0.31
Nodes (7): issue_token(), pat_authenticates_and_updates_last_used_at(), revoked_token_is_rejected(), String, Uuid, scope_gated_endpoint_requires_write_build(), token_listing_and_management_is_session_only()

### Community 59 - "scripts"
Cohesion: 0.20
Nodes (10): scripts, build, dev, fmt, fmt:check, openapi, openapi:export, openapi:generate (+2 more)

### Community 60 - "global-setup.ts"
Cohesion: 0.36
Nodes (6): BACKEND_PORT, FRONTEND_PORT, png(), testLogin(), unique(), waitForTerminalBuild()

### Community 61 - "Model"
Cohesion: 0.22
Nodes (8): Model, DateTimeWithTimeZone, Entity, HasMany, HasOne, Option, String, Uuid

### Community 62 - "Model"
Cohesion: 0.22
Nodes (8): Model, DateTimeWithTimeZone, Entity, HasOne, Json, Option, String, Uuid

### Community 63 - "review_comparison"
Cohesion: 0.36
Nodes (8): get_diff_content(), review_comparison(), Json, Path, Response, Result, State, Uuid

### Community 64 - "get_baseline_entry_content"
Cohesion: 0.47
Nodes (12): get_baseline_entry_content(), get_screenshot_content(), get_storybook_asset(), get_storybook_index(), png_response(), Path, Response, Result (+4 more)

### Community 65 - "MeResponse"
Cohesion: 0.18
Nodes (11): me(), Json, Result, State, MeResponse, From, Model, Option (+3 more)

### Community 66 - "Model"
Cohesion: 0.25
Nodes (7): Model, DateTimeWithTimeZone, Entity, HasOne, Option, String, Uuid

### Community 67 - "Model"
Cohesion: 0.25
Nodes (7): Model, DateTimeWithTimeZone, Entity, HasOne, Option, String, Uuid

### Community 68 - "Model"
Cohesion: 0.25
Nodes (7): Model, DateTimeWithTimeZone, Entity, HasOne, Option, String, Uuid

### Community 69 - "Model"
Cohesion: 0.25
Nodes (7): Model, DateTimeWithTimeZone, Entity, HasMany, Option, String, Uuid

### Community 70 - "Model"
Cohesion: 0.25
Nodes (7): Model, DateTimeWithTimeZone, Entity, HasMany, Option, String, Uuid

### Community 72 - "Migration"
Cohesion: 0.36
Nodes (5): Migration, DbErr, MigrationTrait, Result, SchemaManager

### Community 73 - "VRT frontend"
Cohesion: 0.25
Nodes (7): Backend environment (required for local dev), Development, Known backend gaps the UI works around, Notes on data access, Production, Requirements, VRT frontend

### Community 74 - "Model"
Cohesion: 0.29
Nodes (6): Model, DateTimeWithTimeZone, Entity, HasOne, TenantRole, Uuid

### Community 75 - ".migrations"
Cohesion: 0.29
Nodes (5): Migrator, Box, MigrationTrait, Vec, MigratorTrait

### Community 76 - "Model"
Cohesion: 0.33
Nodes (5): Model, Entity, HasOne, String, Uuid

### Community 77 - "logging_middleware"
Cohesion: 0.33
Nodes (5): logging_middleware(), Body, IntoResponse, Next, Request

### Community 78 - "ensure-database.mjs"
Cohesion: 0.33
Nodes (5): adminUrl, appDb, client, database, url

### Community 80 - "Model"
Cohesion: 0.40
Nodes (4): Model, Entity, HasOne, Uuid

### Community 82 - "health.rs"
Cohesion: 0.60
Nodes (4): health(), HealthResponse, Json, String

### Community 83 - "create_http_client"
Cohesion: 0.40
Nodes (4): create_http_client(), Client, Error, Result

### Community 84 - "main"
Cohesion: 0.40
Nodes (4): main(), Box, Error, Result

### Community 85 - "main"
Cohesion: 0.40
Nodes (4): main(), Box, Error, Result

### Community 86 - "frontend/package.json"
Cohesion: 0.40
Nodes (4): name, packageManager, private, type

### Community 87 - ".prettierrc.json"
Cohesion: 0.40
Nodes (4): printWidth, semi, singleQuote, trailingComma

### Community 88 - "setup_storage"
Cohesion: 0.67
Nodes (3): Arc, Result, setup_storage()

### Community 141 - "t.$tenantSlug.p.$projectSlug.index.tsx"
Cohesion: 0.09
Nodes (41): ToneBadge(), UserMenu(), Select(), SelectContent(), SelectGroup(), SelectItem(), SelectLabel(), SelectScrollDownButton() (+33 more)

## Knowledge Gaps
- **217 isolated node(s):** `ActiveModel`, `ActiveModel`, `ActiveModel`, `ActiveModel`, `ActiveModel` (+212 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **35 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `AppState` connect `AppState` to `get_baseline_entry_content`, `MeResponse`, `csrf.rs`, `load_build_with_role`, `src/auth.rs`, `service/src/github.rs`, `Settings`, `PersonalTokenResponse`, `server.rs`, `upload_screenshot`, `handlers/github.rs`, `service/src/screenshots.rs`, `TestApp`, `AuthUser`, `review_comparison`?**
  _High betweenness centrality (0.251) - this node is a cross-community bridge._
- **Why does `TestApp` connect `TestApp` to `vrt_flow_integration.rs`, `render_flow_integration.rs`, `github_integration.rs`, `Model`, `AppState`, `tenants_integration.rs`, `MockGithub`, `Response`, `projects_integration.rs`, `personal_tokens_integration.rs`, `.new_with`?**
  _High betweenness centrality (0.137) - this node is a cross-community bridge._
- **Why does `AppError` connect `AppError` to `get_baseline_entry_content`, `MeResponse`, `store_screenshot_with_metadata`, `browser.rs`, `load_build_with_role`, `src/auth.rs`, `service/src/github.rs`, `PersonalTokenResponse`, `service/src/comparisons.rs`, `upload_screenshot`, `handlers/github.rs`, `bundle.rs`, `service/src/screenshots.rs`, `AuthUser`, `review_comparison`?**
  _High betweenness centrality (0.136) - this node is a cross-community bridge._
- **What connects `ActiveModel`, `ActiveModel`, `ActiveModel` to the rest of the system?**
  _217 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `vrt_flow_integration.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.146218487394958 - nodes in this community are weakly interconnected._
- **Should `AppError` be split into smaller, more focused modules?**
  _Cohesion score 0.0766541822721598 - nodes in this community are weakly interconnected._
- **Should `routeTree.gen.ts` be split into smaller, more focused modules?**
  _Cohesion score 0.07152496626180836 - nodes in this community are weakly interconnected._