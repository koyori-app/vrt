//! ビルド関連の DTO。

use chrono::{DateTime, Utc};
use common::validation::ScreenshotName;
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
    /// CLI が「どのコミットとの差分か」を知り、撮り直しの必要なストーリーを
    /// 自分で絞り込めるように公開する。ビルド作成（`POST /v1/ci/builds`）の
    /// レスポンス（現時点の baseline。未固定）と、capture plan 添付
    /// （`POST /v1/ci/builds/{id}/plan`）のレスポンス（固定済み baseline）でのみ
    /// 埋まり、それ以外の経路（一覧・状態ポーリング等）では常に `None`。
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
    /// 危険を明示的に承知して承認した場合の証跡。
    #[schema(nullable)]
    pub approval_evidence: Option<serde_json::Value>,
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
            approval_evidence: model.approval_evidence,
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
    /// アップロードしたスクリーンショット名の集合（任意のクロスチェック）。
    ///
    /// 部分アップロードの「撮る集合」の出所は、撮影前に
    /// `POST /v1/ci/builds/{id}/plan` で保存された capture plan である。
    /// このフィールドを渡した場合、サーバーは保存済み計画との完全一致を検証し、
    /// ずれていれば 400 で拒否する。**計画が保存されていないビルドに渡すと 400**
    /// ——finalize 時の自己申告だけの部分アップロードは、撮影が全滅したときに
    /// 空の申告と空のアップロードが循環一致して偽 PASS になるため受け付けない。
    /// `null`・省略で計画ありのビルドは計画どおり、計画なしのビルドは全撮影。
    /// `storybook` モードで指定すると 400（サーバーが撮るので宣言が成立しない）。
    #[serde(default)]
    pub captured_names: Option<Vec<String>>,

    /// クライアントが撮影計画の起点にした baseline のコミット SHA。
    ///
    /// `screenshots` モード: capture plan 付きビルドで渡すと、計画添付時に
    /// 固定された baseline と照合し、一致しなければ 400。計画なしのビルドに
    /// 渡すと 400（照合すべき固定値が無い）。
    /// `storybook` モード: `only_story_ids` を渡すときは**必須**。サーバーは
    /// 現在の baseline と照合してから比較 baseline を固定する。ずれていれば
    /// 400 で拒否する（流用画像と比較対象が計画と別物になるのを防ぐ）。
    #[serde(default)]
    pub expected_baseline_commit_sha: Option<String>,
}

/// `POST /v1/ci/builds/{id}/plan` — screenshots モードの部分アップロード計画。
///
/// 撮影を始める**前**に「今回撮る名前」と「現時点で存在する全名前（現行 index）」を
/// ビルドへ固定する。finalize と比較ジョブの選択集合はこの保存値だけを使う。
/// 撮影後の自己申告（`captured_names`）を出所にしないのは、撮影が全滅したとき
/// 空の申告と空のアップロードが循環一致して偽 PASS になるためである。
#[derive(Debug, Deserialize, ToSchema)]
pub struct AttachCapturePlanRequest {
    /// 今回撮影してアップロードするスクリーンショット名（`manifest_names` の部分集合）。
    ///
    /// 空配列は「変更の影響を受けた story は無い」という選択結果で、baseline の
    /// 全エントリ（manifest に残っているもの）が流用される。
    pub selected_names: Vec<String>,
    /// 現時点で存在する全スクリーンショット名（現行 story index の写し）。
    ///
    /// baseline にあってここに無い名前は「story が消えた」とみなし、流用せず
    /// `removed` として報告される。ここを実際の index より狭く申告すると
    /// 消滅扱いが増える方向（差分が見える方向）にしか倒れない。
    pub manifest_names: Vec<String>,
    /// この計画の起点にした baseline のコミット SHA（ビルド作成レスポンスの
    /// `baseline_commit_sha`）。現在の baseline と一致しなければ 409 で拒否され、
    /// クライアントは再計画する。
    pub baseline_commit_sha: String,
}

impl AttachCapturePlanRequest {
    /// 各リストを名前規則で検証し、型付きの名前へ変換する。
    /// 要素数の上限と baseline SHA の妥当性もここで検査する。
    ///
    /// 名前規則は [`common::validation::ScreenshotName`] の一本だけ——
    /// アップロード・finalize の `captured_names` と同じ関数である。計画側だけ
    /// 緩いと「計画には載るのにアップロードできない名前」ができ、そのビルドは
    /// 永久に finalize できない。
    pub fn parse_lists(&self) -> Result<(Vec<ScreenshotName>, Vec<ScreenshotName>), String> {
        let selected = parse_name_list(&self.selected_names, "selected_names")?;
        let manifest = parse_name_list(&self.manifest_names, "manifest_names")?;
        if self.baseline_commit_sha.is_empty() || self.baseline_commit_sha.len() > 100 {
            return Err("baseline_commit_sha must be 1..=100 characters".into());
        }
        Ok((selected, manifest))
    }
}

/// `only_story_ids` / `captured_names` の要素数上限。DoS 対策の緩い上限。
pub const MAX_ONLY_STORY_IDS: usize = 10_000;
/// storybook モードの story ID 1 件あたりの最大長。
///
/// story ID はスクリーンショット**名**とは別の名前空間である（storybook モードの
/// スクリーンショット名は `{title}/{name}` で、保存時に
/// [`common::validation::ScreenshotName`] で改めて検証される）。screenshots
/// モードの名前リストにこの上限を使ってはならない——名前の規則は
/// `ScreenshotName` の一本だけである。
pub const MAX_STORY_ID_LEN: usize = 512;

impl FinalizeBuildRequest {
    /// `only_story_ids`（storybook モードの story ID）の要素数・各要素の長さを検証する。
    ///
    /// 違反時はエラーメッセージを返す（呼び出し側で 400 にする）。
    /// `captured_names` はスクリーンショット名なので、こちらではなく
    /// [`FinalizeBuildRequest::parse_captured_names`] で名前規則により検証する。
    pub fn validate_story_ids(&self) -> Result<(), String> {
        validate_id_list(self.only_story_ids.as_deref(), "only_story_ids")
    }

    /// `captured_names` を名前規則（[`common::validation::ScreenshotName`]——
    /// 計画・アップロードと同一）で検証し、型付きの名前へ変換する。
    pub fn parse_captured_names(&self) -> Result<Option<Vec<ScreenshotName>>, String> {
        self.captured_names
            .as_deref()
            .map(|list| parse_name_list(list, "captured_names"))
            .transpose()
    }
}

/// スクリーンショット名リストの共通検証。
///
/// 要素数の上限（[`MAX_ONLY_STORY_IDS`]）と、各要素の名前規則
/// （[`ScreenshotName::parse`]——plan・アップロード・finalize で同一）を適用する。
fn parse_name_list(list: &[String], field: &str) -> Result<Vec<ScreenshotName>, String> {
    if list.len() > MAX_ONLY_STORY_IDS {
        return Err(format!(
            "{field} must contain at most {MAX_ONLY_STORY_IDS} entries"
        ));
    }
    list.iter()
        .map(|name| {
            ScreenshotName::parse(name.clone()).map_err(|e| format!("{field}: `{name}`: {e}"))
        })
        .collect()
}

/// storybook モードの story ID リストの検証（要素数・空要素・長さ）。
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
    ///
    /// `removed`（story の消滅）と `failed`（比較失敗）は含まない。各専用フラグを
    /// 別途明示したときだけ承認される。
    #[serde(default)]
    pub force: bool,
    /// `true` にすると story の消滅（`removed`）も承認対象に含める。
    ///
    /// baseline から実体が消える不可逆操作なので `force` とは別のフラグにしてある。
    /// このフラグは `force: true` と併用した場合だけ効果があり、単独指定は no-op。
    #[serde(default)]
    pub accept_removals: bool,
    /// `true` にすると比較に失敗した結果（`failed`）も承認対象に含める。
    ///
    /// 破損画像などを baseline に焼き付ける危険があるため `force` とは別のフラグ。
    /// このフラグは `force: true` と併用した場合だけ効果があり、単独指定は no-op。
    #[serde(default)]
    pub accept_failures: bool,
    /// `true` にすると、現行 baseline より古いビルドへの意図的な巻き戻しを許可する。
    ///
    /// 巻き戻し対象である場合だけ効果がある。通常の再承認や baseline 移動の bypass には
    /// 使われず、巻き戻し元・先のビルド番号が承認証跡に保存される。
    #[serde(default)]
    pub accept_revert: bool,
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
    fn captured_names_follow_the_screenshot_name_rule() {
        use common::validation::SCREENSHOT_NAME_MAX_BYTES;

        assert!(req_names(None).parse_captured_names().is_ok());
        // 空配列は「今回撮った名前は無い」= 全流用の宣言として有効。
        assert!(req_names(Some(vec![])).parse_captured_names().is_ok());
        assert!(req_names(Some(vec!["home"])).parse_captured_names().is_ok());
        assert!(
            req_names(Some(vec!["ok", ""]))
                .parse_captured_names()
                .is_err()
        );
        // 前後空白はアップロード経路と同じ規則で拒否（trim して受けない）。
        assert!(
            req_names(Some(vec!["home "]))
                .parse_captured_names()
                .is_err()
        );
        // 上限もアップロード経路と同じ 255 **バイト**。story ID の 512 ではない。
        let long = "a".repeat(SCREENSHOT_NAME_MAX_BYTES + 1);
        assert!(
            req_names(Some(vec![long.as_str()]))
                .parse_captured_names()
                .is_err()
        );
        let ok = "a".repeat(SCREENSHOT_NAME_MAX_BYTES);
        assert!(
            req_names(Some(vec![ok.as_str()]))
                .parse_captured_names()
                .is_ok()
        );
    }

    fn plan_req(selected: Vec<&str>, manifest: Vec<&str>) -> AttachCapturePlanRequest {
        AttachCapturePlanRequest {
            selected_names: selected.into_iter().map(String::from).collect(),
            manifest_names: manifest.into_iter().map(String::from).collect(),
            baseline_commit_sha: "abc123".into(),
        }
    }

    #[test]
    fn plan_lists_follow_the_screenshot_name_rule() {
        use common::validation::SCREENSHOT_NAME_MAX_BYTES;

        assert!(
            plan_req(vec!["home"], vec!["home", "about"])
                .parse_lists()
                .is_ok()
        );
        // アップロードで通らない名前は計画にも載せられない（永久 finalize 不能の防止）。
        let long = "a".repeat(SCREENSHOT_NAME_MAX_BYTES + 1);
        assert!(
            plan_req(vec![long.as_str()], vec![long.as_str()])
                .parse_lists()
                .is_err()
        );
        assert!(
            plan_req(vec![" home"], vec![" home"])
                .parse_lists()
                .is_err()
        );
        assert!(plan_req(vec![], vec!["home "]).parse_lists().is_err());
        assert!(plan_req(vec![], vec![""]).parse_lists().is_err());
    }
}
