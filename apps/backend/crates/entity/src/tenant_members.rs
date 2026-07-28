//! Tenant members entity — schema-first with hand-written `DeriveActiveEnum`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// テナント内のロール。強さは `member < admin < owner`。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(255))")]
#[serde(rename_all = "lowercase")]
pub enum TenantRole {
    #[sea_orm(string_value = "owner")]
    Owner,
    #[sea_orm(string_value = "admin")]
    Admin,
    #[sea_orm(string_value = "member")]
    Member,
}

impl TenantRole {
    /// 権限の強さ。大きいほど強い。
    pub fn rank(self) -> u8 {
        match self {
            TenantRole::Member => 0,
            TenantRole::Admin => 1,
            TenantRole::Owner => 2,
        }
    }

    /// `self` が `min_role` 以上の権限を持つか。
    pub fn at_least(self, min_role: TenantRole) -> bool {
        self.rank() >= min_role.rank()
    }
}

pub use super::_generated::tenant_members::*;

#[cfg(test)]
mod tests {
    use super::TenantRole;

    #[test]
    fn role_ordering_is_member_admin_owner() {
        assert!(TenantRole::Owner.at_least(TenantRole::Admin));
        assert!(TenantRole::Owner.at_least(TenantRole::Owner));
        assert!(TenantRole::Admin.at_least(TenantRole::Member));
        assert!(!TenantRole::Admin.at_least(TenantRole::Owner));
        assert!(!TenantRole::Member.at_least(TenantRole::Admin));
    }
}
