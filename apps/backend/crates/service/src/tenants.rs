//! テナントとメンバーシップのビジネスロジック。
//!
//! ロールの強さは `member < admin < owner`（[`TenantRole::rank`]）。
//! 「最後の owner」は降格も削除もできない（[`AppError::Conflict`]）。

use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, prelude::Uuid,
};

use common::db::with_transaction;
use common::error::AppError;
use common::validation::check_slug;
use entity::{tenant_members, tenant_members::TenantRole, tenants, users};

/// slug を検証し、不正なら 400 にする。
pub fn validate_slug(slug: &str) -> Result<(), AppError> {
    check_slug(slug).map_err(|e| AppError::BadRequestDetail(e.to_string()))
}

/// 指定ユーザーのメンバーシップ（無ければ `None`）。
pub async fn find_membership<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<Option<tenant_members::Model>, AppError> {
    Ok(tenant_members::Entity::find()
        .filter(tenant_members::Column::TenantId.eq(tenant_id))
        .filter(tenant_members::Column::UserId.eq(user_id))
        .one(db)
        .await?)
}

/// `min_role` 以上の権限を持つメンバーであることを要求する。
///
/// 非メンバー（存在しないテナントを含む）は一律 403 にして、テナントの存在有無を漏らさない。
pub async fn require_role<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Uuid,
    min_role: TenantRole,
) -> Result<tenant_members::Model, AppError> {
    let member = find_membership(db, tenant_id, user_id)
        .await?
        .ok_or(AppError::Forbidden)?;
    if member.role.at_least(min_role) {
        Ok(member)
    } else {
        Err(AppError::Forbidden)
    }
}

/// テナント本体を取得する。
pub async fn get_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<tenants::Model, AppError> {
    tenants::Entity::find_by_id(tenant_id)
        .one(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// ユーザーが所属するテナント一覧（作成日の昇順）。各テナントに自分のロールを添える。
pub async fn list_tenants_for_user<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
) -> Result<Vec<(tenants::Model, TenantRole)>, AppError> {
    let roles: HashMap<Uuid, TenantRole> = tenant_members::Entity::find()
        .filter(tenant_members::Column::UserId.eq(user_id))
        .all(db)
        .await?
        .into_iter()
        .map(|m| (m.tenant_id, m.role))
        .collect();

    if roles.is_empty() {
        return Ok(Vec::new());
    }

    let tenants = tenants::Entity::find()
        .filter(tenants::Column::Id.is_in(roles.keys().copied().collect::<Vec<_>>()))
        .order_by_asc(tenants::Column::CreatedAt)
        .all(db)
        .await?;

    Ok(tenants
        .into_iter()
        .filter_map(|t| roles.get(&t.id).copied().map(|role| (t, role)))
        .collect())
}

/// テナントを作成する。作成者は同一トランザクションで owner として登録される。
pub async fn create_tenant(
    db: &DatabaseConnection,
    creator_id: Uuid,
    name: String,
    slug: String,
    avatar_url: Option<String>,
) -> Result<tenants::Model, AppError> {
    validate_slug(&slug)?;
    let now = Utc::now().fixed_offset();

    with_transaction(db, move |txn| {
        Box::pin(async move {
            let tenant = tenants::ActiveModel {
                id: Set(Uuid::new_v4()),
                name: Set(name),
                slug: Set(slug),
                avatar_url: Set(avatar_url),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(txn)
            .await?;

            tenant_members::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant.id),
                user_id: Set(creator_id),
                role: Set(TenantRole::Owner),
                created_at: Set(now),
            }
            .insert(txn)
            .await?;

            Ok(tenant)
        })
    })
    .await
}

/// テナント設定の更新。`avatar_url` は `Some(None)` で明示的にクリアできる。
pub async fn update_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    name: Option<String>,
    avatar_url: Option<Option<String>>,
) -> Result<tenants::Model, AppError> {
    let tenant = get_tenant(db, tenant_id).await?;
    let mut active: tenants::ActiveModel = tenant.into();
    if let Some(name) = name {
        active.name = Set(name);
    }
    if let Some(avatar_url) = avatar_url {
        active.avatar_url = Set(avatar_url);
    }
    active.updated_at = Set(Utc::now().fixed_offset());
    Ok(active.update(db).await?)
}

/// テナントを削除する（メンバー・プロジェクトは FK の ON DELETE CASCADE で消える）。
pub async fn delete_tenant<C: ConnectionTrait>(db: &C, tenant_id: Uuid) -> Result<(), AppError> {
    get_tenant(db, tenant_id).await?;
    tenants::Entity::delete_by_id(tenant_id).exec(db).await?;
    Ok(())
}

/// テナントのメンバー一覧（参加順）。表示名のために `users` を join して返す。
///
/// `user_id` は NOT NULL の FK なので実際には常に `Some` だが、join の型に忠実に
/// `Option` のまま返し、表示側で欠損を許容する。
pub async fn list_members<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
) -> Result<Vec<(tenant_members::Model, Option<users::Model>)>, AppError> {
    Ok(tenant_members::Entity::find()
        .filter(tenant_members::Column::TenantId.eq(tenant_id))
        .order_by_asc(tenant_members::Column::CreatedAt)
        .find_also_related(users::Entity)
        .all(db)
        .await?)
}

/// テナント内の owner 数。
pub async fn count_owners<C: ConnectionTrait>(db: &C, tenant_id: Uuid) -> Result<u64, AppError> {
    Ok(tenant_members::Entity::find()
        .filter(tenant_members::Column::TenantId.eq(tenant_id))
        .filter(tenant_members::Column::Role.eq(TenantRole::Owner))
        .count(db)
        .await?)
}

/// `user_id` か `username` のどちらか一方でユーザーを解決する。
pub async fn resolve_user<C: ConnectionTrait>(
    db: &C,
    user_id: Option<Uuid>,
    username: Option<String>,
) -> Result<users::Model, AppError> {
    let found = match (user_id, username) {
        (Some(id), None) => users::Entity::find_by_id(id).one(db).await?,
        (None, Some(username)) => {
            users::Entity::find()
                .filter(users::Column::Username.eq(username))
                .one(db)
                .await?
        }
        _ => {
            return Err(AppError::BadRequestDetail(
                "exactly one of user_id or username is required".into(),
            ));
        }
    };
    found.ok_or(AppError::NotFound)
}

/// メンバーを追加する。既に所属していれば 409。
pub async fn add_member<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    username: Option<String>,
    role: TenantRole,
) -> Result<tenant_members::Model, AppError> {
    let user = resolve_user(db, user_id, username).await?;

    if find_membership(db, tenant_id, user.id).await?.is_some() {
        return Err(AppError::Conflict);
    }

    Ok(tenant_members::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user.id),
        role: Set(role),
        created_at: Set(Utc::now().fixed_offset()),
    }
    .insert(db)
    .await?)
}

/// メンバーのロールを変更する。最後の owner の降格は 409。
pub async fn update_member_role<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    target_user_id: Uuid,
    role: TenantRole,
) -> Result<tenant_members::Model, AppError> {
    let current = find_membership(db, tenant_id, target_user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if current.role == TenantRole::Owner
        && role != TenantRole::Owner
        && count_owners(db, tenant_id).await? <= 1
    {
        return Err(AppError::Conflict);
    }

    let mut active: tenant_members::ActiveModel = current.into();
    active.role = Set(role);
    Ok(active.update(db).await?)
}

/// メンバーを削除する。最後の owner の削除は 409。
pub async fn remove_member<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    target_user_id: Uuid,
) -> Result<(), AppError> {
    let member = find_membership(db, tenant_id, target_user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if member.role == TenantRole::Owner && count_owners(db, tenant_id).await? <= 1 {
        return Err(AppError::Conflict);
    }

    tenant_members::Entity::delete_by_id(member.id)
        .exec(db)
        .await?;
    Ok(())
}
