//! パーソナルアクセストークン (PAT) に付与する権限スコープ。
//!
//! VRT の MVP では 3 種類のみ:
//! - `read:project` — プロジェクト・ビルド一覧などの参照
//! - `write:build`  — CI からのビルド作成 / スクリーンショットアップロード
//! - `read:build`   — ビルド結果の参照（CI のポーリング等）

use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum Scope {
    #[serde(rename = "read:project")]
    ReadProject,
    #[serde(rename = "write:build")]
    WriteBuild,
    #[serde(rename = "read:build")]
    ReadBuild,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::ReadProject => "read:project",
            Scope::WriteBuild => "write:build",
            Scope::ReadBuild => "read:build",
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read:project" => Ok(Scope::ReadProject),
            "write:build" => Ok(Scope::WriteBuild),
            "read:build" => Ok(Scope::ReadBuild),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// PAT に付与する権限スコープのリスト（JSONB カラムとして保存）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult, ToSchema)]
#[serde(transparent)]
pub struct ScopeList(pub Vec<Scope>);

impl ScopeList {
    /// 指定スコープを保持しているか。書き込みスコープは同種の読み取りを含む。
    pub fn has_scope(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
            || (scope == Scope::ReadBuild && self.0.contains(&Scope::WriteBuild))
    }
}

impl From<Scope> for sea_orm::Value {
    fn from(source: Scope) -> Self {
        sea_orm::Value::String(Some(source.as_str().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_build_implies_read_build() {
        let scopes = ScopeList(vec![Scope::WriteBuild]);
        assert!(scopes.has_scope(Scope::WriteBuild));
        assert!(scopes.has_scope(Scope::ReadBuild));
        assert!(!scopes.has_scope(Scope::ReadProject));
    }

    #[test]
    fn read_build_does_not_imply_write_build() {
        let scopes = ScopeList(vec![Scope::ReadBuild]);
        assert!(!scopes.has_scope(Scope::WriteBuild));
    }

    #[test]
    fn scope_roundtrips_through_str() {
        for scope in [Scope::ReadProject, Scope::WriteBuild, Scope::ReadBuild] {
            assert_eq!(scope.as_str().parse::<Scope>(), Ok(scope));
        }
    }
}
