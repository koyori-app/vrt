//! ビルド関連の DTO。

use chrono::{DateTime, Utc};
use sea_orm::prelude::Uuid;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use entity::{build_logs, builds, builds::BuildMode, builds::BuildStatus, screenshots};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub project_id: Uuid,
    /// プロジェクト内で連番のビルド番号。
    pub number: i64,
    pub branch: String,
    pub commit_sha: String,
    #[schema(nullable)]
    pub commit_message: Option<String>,
    #[schema(nullable)]
    pub pull_request_number: Option<i32>,
    pub status: BuildStatus,
    /// 入力形式（`screenshots` = CI がアップロード / `storybook` = サーバーがレンダリング）。
    pub mode: BuildMode,
    /// storybook モードでバンドルがアップロード済みか。
    pub storybook_uploaded: bool,
    /// 比較に使った baseline（未確定なら null）。
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub baseline_id: Option<Uuid>,
    /// このビルドが比較する baseline のコミット SHA。
    ///
    /// 将来の CLI が「どのコミットとの差分か」を知り、
    /// 撮り直しの必要なストーリーを自分で絞り込めるように公開する。
    /// ビルド作成（`POST /v1/ci/builds`）のレスポンスでのみ埋まり、
    /// それ以外の経路（一覧・状態ポーリング等）では常に `None`。
    /// baseline が無い、または昇格元ビルドが削除済みなら `None`。
    #[schema(nullable)]
    pub baseline_commit_sha: Option<String>,
    pub total_count: i32,
    pub changed_count: i32,
    pub added_count: i32,
    pub removed_count: i32,
    pub unchanged_count: i32,
    #[schema(nullable)]
    pub error_message: Option<String>,
    #[schema(value_type = Option<String>, format = "uuid", nullable)]
    pub approved_by: Option<Uuid>,
    #[schema(value_type = Option<String>, format = "date-time", nullable)]
    pub approved_at: Option<DateTime<Utc>>,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
    #[schema(value_type = Option<String>, format = "date-time", nullable)]
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<builds::Model> for BuildResponse {
    fn from(model: builds::Model) -> Self {
        Self {
            id: model.id,
            project_id: model.project_id,
            number: model.number,
            branch: model.branch,
            commit_sha: model.commit_sha,
            commit_message: model.commit_message,
            pull_request_number: model.pull_request_number,
            status: model.status,
            mode: model.mode,
            // ストレージキー自体は内部情報なので露出させず、有無だけ返す。
            storybook_uploaded: model.storybook_key.is_some(),
            baseline_id: model.baseline_id,
            // baseline のコミット SHA は DB の追加参照が必要なので From では解決しない。
            // 必要な経路（create_build）が組み立て後に明示的に埋める。
            baseline_commit_sha: None,
            total_count: model.total_count,
            changed_count: model.changed_count,
            added_count: model.added_count,
            removed_count: model.removed_count,
            unchanged_count: model.unchanged_count,
            error_message: model.error_message,
            approved_by: model.approved_by,
            approved_at: model.approved_at.map(|t| t.with_timezone(&Utc)),
            created_at: model.created_at.with_timezone(&Utc),
            completed_at: model.completed_at.map(|t| t.with_timezone(&Utc)),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildListResponse {
    pub builds: Vec<BuildResponse>,
    pub total: u64,
}

#[derive(Validate, Debug, Deserialize, ToSchema)]
pub struct CreateBuildRequest {
    /// 対象ブランチ。baseline の解決キーになる。
    #[validate(length(min = 1, max = 255))]
    pub branch: String,
    #[validate(length(min = 1, max = 100))]
    pub commit_sha: String,
    #[validate(length(max = 4000))]
    pub commit_message: Option<String>,
    /// PR 番号（Phase 6 の GitHub ステータス連携で使う）。
    pub pull_request_number: Option<i32>,
    /// 入力形式。省略時は `screenshots`（従来どおり CI が PNG をアップロードする）。
    /// `storybook` を指定すると `POST /v1/ci/builds/{id}/storybook` でバンドルを送る形になる。
    #[serde(default)]
    pub mode: Option<BuildMode>,
}

/// finalize（アップロード締め）リクエスト。
///
/// ボディは任意。無し・空・`only_story_ids: null` はすべて「全ストーリー撮影」
/// を意味し、従来どおりの挙動になる（後方互換）。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct FinalizeBuildRequest {
    /// 実際に撮影が必要なストーリー ID のリスト（TurboSnap 相当）。
    ///
    /// ここに載っていないストーリーは baseline のスクリーンショットを流用する
    /// （baseline に該当が無い新規ストーリーは見逃さないよう撮影する）。
    /// `storybook` モードのビルドでのみ意味を持つ。`screenshots` モードで
    /// 指定すると 400 になる（サーバーがレンダリングしないため、ストーリー ID を
    /// スクリーンショット名へ写像できない。代わりに `captured_names` を使う）。
    #[serde(default)]
    pub only_story_ids: Option<Vec<String>>,

    /// `screenshots` モードの部分アップロードで、今回 CI が撮影して
    /// アップロードしたスクリーンショット名の集合。
    ///
    /// 宣言するとサーバーは「宣言 == 実際にアップロードされた名前」を検証し、
    /// 一致しなければ 400 で拒否する（撮るつもりだった名前が欠けたまま
    /// baseline を流用してしまう事故を防ぐ）。宣言に無い baseline エントリは
    /// removed ではなく前回の baseline を流用（carry-forward）する。
    /// `null`・省略は全撮影（従来どおり。baseline に無い名前は added、
    /// アップロードされなかった baseline エントリは removed）。
    /// `storybook` モードで指定すると 400（サーバーが撮るので宣言が成立しない）。
    #[serde(default)]
    pub captured_names: Option<Vec<String>>,

    /// クライアントが撮影計画の起点にした baseline のコミット SHA。
    ///
    /// 渡すとサーバーは「このビルドに固定された baseline の昇格元コミット」と
    /// 照合し、一致しなければ 400 で拒否する。計画と比較が別の baseline を
    /// 見てしまう取り違えを finalize 時点で検出するための任意フィールド。
    #[serde(default)]
    pub expected_baseline_commit_sha: Option<String>,
}

/// `only_story_ids` / `captured_names` の要素数上限。DoS 対策の緩い上限。
pub const MAX_ONLY_STORY_IDS: usize = 10_000;
/// ストーリー ID / スクリーンショット名 1 件あたりの最大長。
pub const MAX_STORY_ID_LEN: usize = 512;

impl FinalizeBuildRequest {
    /// `only_story_ids` / `captured_names` の要素数・各要素の長さを検証する。
    ///
    /// 違反時はエラーメッセージを返す（呼び出し側で 400 にする）。
    pub fn validate_story_ids(&self) -> Result<(), String> {
        validate_id_list(self.only_story_ids.as_deref(), "only_story_ids")?;
        validate_id_list(self.captured_names.as_deref(), "captured_names")?;
        Ok(())
    }
}

/// ID / 名前リストの共通検証（要素数・空要素・長さ）。
fn validate_id_list(list: Option<&[String]>, field: &str) -> Result<(), String> {
    let Some(ids) = list else {
        return Ok(());
    };
    if ids.len() > MAX_ONLY_STORY_IDS {
        return Err(format!(
            "{field} must contain at most {MAX_ONLY_STORY_IDS} entries"
        ));
    }
    for id in ids {
        if id.is_empty() {
            return Err(format!("{field} must not contain empty entries"));
        }
        if id.len() > MAX_STORY_ID_LEN {
            return Err(format!(
                "each {field} entry must be {MAX_STORY_ID_LEN} characters or fewer"
            ));
        }
    }
    Ok(())
}

/// Storybook バンドルのアップロード結果。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StorybookBundleResponse {
    #[schema(value_type = String, format = "uuid")]
    pub build_id: Uuid,
    /// 受け取った zip のバイト数。
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScreenshotResponse {
    #[schema(value_type = String, format = "uuid")]
    pub id: Uuid,
    #[schema(value_type = String, format = "uuid")]
    pub build_id: Uuid,
    pub name: String,
    pub width: i32,
    pub height: i32,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<screenshots::Model> for ScreenshotResponse {
    fn from(model: screenshots::Model) -> Self {
        Self {
            id: model.id,
            build_id: model.build_id,
            name: model.name,
            width: model.width,
            height: model.height,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

/// ビルド承認リクエスト。
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ApproveBuildRequest {
    /// `true` にすると未レビューの比較もまとめて承認する（一括承認）。
    #[serde(default)]
    pub force: bool,
}

/// ビルド一覧のページネーションパラメータ。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildListQuery {
    /// 取得件数（1〜100、既定 30）。
    pub limit: Option<u64>,
    /// スキップ件数（既定 0）。
    pub offset: Option<u64>,
}

/// ビルド進捗ログの 1 行。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildLogEntry {
    /// グローバル連番。増分取得のカーソルにそのまま使う。
    pub id: i64,
    /// `info` | `warn` | `error`。
    pub level: String,
    pub message: String,
    #[schema(value_type = String, format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<build_logs::Model> for BuildLogEntry {
    fn from(model: build_logs::Model) -> Self {
        Self {
            id: model.id,
            level: model.level,
            message: model.message,
            created_at: model.created_at.with_timezone(&Utc),
        }
    }
}

/// ビルド進捗ログの増分取得レスポンス。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildLogsResponse {
    /// `after` より後の行（id 昇順）。
    pub entries: Vec<BuildLogEntry>,
    /// クライアントが次のポーリングで `after` に渡す値。
    /// 行が無ければリクエストされた `after` が据え置かれる。
    pub last_id: i64,
}

/// ビルド進捗ログの増分取得パラメータ。
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BuildLogsQuery {
    /// この id より後の行だけを返す（省略時は 0 = 先頭から）。
    pub after: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(ids: Option<Vec<&str>>) -> FinalizeBuildRequest {
        FinalizeBuildRequest {
            only_story_ids: ids.map(|v| v.into_iter().map(String::from).collect()),
            ..Default::default()
        }
    }

    fn req_names(names: Option<Vec<&str>>) -> FinalizeBuildRequest {
        FinalizeBuildRequest {
            captured_names: names.map(|v| v.into_iter().map(String::from).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn none_and_empty_are_valid() {
        assert!(req(None).validate_story_ids().is_ok());
        assert!(req(Some(vec![])).validate_story_ids().is_ok());
    }

    #[test]
    fn normal_ids_are_valid() {
        assert!(
            req(Some(vec!["button--primary", "card--default"]))
                .validate_story_ids()
                .is_ok()
        );
    }

    #[test]
    fn empty_id_is_rejected() {
        assert!(req(Some(vec!["ok", ""])).validate_story_ids().is_err());
    }

    #[test]
    fn too_long_id_is_rejected() {
        let long = "a".repeat(MAX_STORY_ID_LEN + 1);
        assert!(req(Some(vec![long.as_str()])).validate_story_ids().is_err());
        let ok = "a".repeat(MAX_STORY_ID_LEN);
        assert!(req(Some(vec![ok.as_str()])).validate_story_ids().is_ok());
    }

    #[test]
    fn too_many_ids_are_rejected() {
        let ids: Vec<String> = (0..=MAX_ONLY_STORY_IDS).map(|i| i.to_string()).collect();
        let r = FinalizeBuildRequest {
            only_story_ids: Some(ids),
            ..Default::default()
        };
        assert!(r.validate_story_ids().is_err());
    }

    #[test]
    fn captured_names_share_the_same_limits() {
        assert!(req_names(None).validate_story_ids().is_ok());
        // 空配列は「今回撮った名前は無い」= 全流用の宣言として有効。
        assert!(req_names(Some(vec![])).validate_story_ids().is_ok());
        assert!(req_names(Some(vec!["home"])).validate_story_ids().is_ok());
        assert!(
            req_names(Some(vec!["ok", ""]))
                .validate_story_ids()
                .is_err()
        );
        let long = "a".repeat(MAX_STORY_ID_LEN + 1);
        assert!(
            req_names(Some(vec![long.as_str()]))
                .validate_story_ids()
                .is_err()
        );
    }
}
