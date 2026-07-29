//! TurboSnap 相当の「影響ストーリー算出」。
//!
//! Chromatic の `--only-changed` に相当する処理をこの CLI 側に内蔵する。
//! webpack の stats（依存グラフ）と Storybook の `index.json`、git の差分から
//! 「撮り直しが必要なストーリー ID」を算出する。ネットワークにも git にも
//! 触らない純関数だけを置き、fixture ベースの単体テストで固める
//! （副作用のある git 呼び出しや HTTP は呼び出し側 `main.rs` が担う）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

/// 変更が波及したストーリーだけを撮るか、全部撮り直すかの判断結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 全ストーリー撮影（finalize はボディ無し）。文字列は理由（ログ用）。
    CaptureAll(String),
    /// 指定したストーリー ID だけ撮影（finalize に `only_story_ids` を渡す）。
    Only(Vec<String>),
}

/// 判断結果と、利用者に見せる警告・補足メッセージ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub plan: Plan,
    /// 途中で拾った警告や補足（capture-all に倒した理由の詳細など）。
    pub notes: Vec<String>,
}

// ── 入力 JSON のパース ───────────────────────────────────────────────────

/// webpack stats（`--stats-json` 出力）のうち、依存グラフ構築に要る部分だけ。
#[derive(Debug, Default, Deserialize)]
pub struct WebpackStats {
    #[serde(default)]
    modules: Vec<RawModule>,
}

#[derive(Debug, Default, Deserialize)]
struct RawModule {
    #[serde(default)]
    name: Option<String>,
    /// `name` が擬似名のとき（concatenated module 等）の実ファイルパス。
    #[serde(default, rename = "nameForCondition")]
    name_for_condition: Option<String>,
    /// このモジュールを import している側の一覧。
    #[serde(default)]
    reasons: Vec<RawReason>,
    /// ModuleConcatenationPlugin でまとめられた子モジュール。再帰的に展開する。
    #[serde(default)]
    modules: Vec<RawModule>,
}

#[derive(Debug, Default, Deserialize)]
struct RawReason {
    #[serde(default, rename = "moduleName")]
    module_name: Option<String>,
}

impl WebpackStats {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// `index.json`（Storybook 7/8/9 の v4/v5）のうち撮影対象の判定に要る部分。
#[derive(Debug, Default, Deserialize)]
struct StorybookIndex {
    #[serde(default)]
    entries: Option<std::collections::BTreeMap<String, RawEntry>>,
    #[serde(default)]
    stories: Option<std::collections::BTreeMap<String, RawEntry>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "importPath")]
    import_path: Option<String>,
    #[serde(default, rename = "type")]
    entry_type: Option<String>,
}

/// index.json から取り出したストーリー 1 件（`docs` 等は含まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryEntry {
    pub id: String,
    /// ストーリーファイルの相対パス（例 `./src/Button.stories.tsx`）。
    pub import_path: String,
}

/// index.json の JSON からストーリー一覧を取り出す。
///
/// `type == "story"`（または `type` 欠落）かつ `importPath` を持つものだけ。
pub fn parse_index(json: &str) -> Result<Vec<StoryEntry>, serde_json::Error> {
    let index: StorybookIndex = serde_json::from_str(json)?;
    let entries = index.entries.or(index.stories).unwrap_or_default();
    let mut out = Vec::new();
    for (key, entry) in entries {
        if entry.entry_type.as_deref().unwrap_or("story") != "story" {
            continue;
        }
        let Some(import_path) = entry.import_path else {
            continue;
        };
        out.push(StoryEntry {
            id: entry.id.unwrap_or(key),
            import_path,
        });
    }
    Ok(out)
}

// ── モジュールパスの正規化 ─────────────────────────────────────────────

/// webpack のモジュール名を突き合わせ用のキーに正規化する。
///
/// loader チェーン（`babel-loader!./src/x`）・クエリ（`?raw`）・
/// concatenated 名（`./src/a.js + 2 modules`）・先頭 `./`・Windows 区切りを
/// 落として `src/x` 形に揃える。git のパス（リポジトリルート相対）を
/// cwd 相対に直したものと同じ土俵で比較できるようにするのが目的。
pub fn normalize_module_path(raw: &str) -> String {
    let mut s = raw;
    // loader チェーンは最後の `!` 以降が実リソース。
    if let Some(idx) = s.rfind('!') {
        s = &s[idx + 1..];
    }
    // クエリ・ハッシュ suffix を落とす。
    if let Some(idx) = s.find('?') {
        s = &s[..idx];
    }
    // concatenated module の擬似名 "... + N modules" は先頭パスだけ見る。
    if let Some(idx) = s.find(" + ") {
        s = &s[..idx];
    }
    let s = s.trim();
    let s = s.strip_prefix("./").unwrap_or(s);
    s.replace('\\', "/")
}

/// git のパス（リポジトリルート相対）を、stats のモジュール名と同じ
/// 「cwd 相対」キーに変換する。
///
/// storybook build を回した cwd（= CLI 実行 cwd 想定）とリポジトリルートの差を
/// `git rev-parse --show-toplevel` 由来の `repo_root` で吸収する。cwd の外の
/// パス（モノレポの別パッケージ等）は `./` 形のモジュール名と突き合わせられない
/// ため `None` を返し、呼び出し側で「グラフ外」として安全側に倒させる。
pub fn changed_path_to_key(repo_root: &Path, cwd: &Path, repo_relative: &str) -> Option<String> {
    // repo_root と cwd の差分（例: "apps/frontend"）。同一なら差分なし。
    let prefix = cwd.strip_prefix(repo_root).ok()?;
    let prefix = prefix.to_string_lossy().replace('\\', "/");
    let rel = repo_relative.replace('\\', "/");
    let rel = if prefix.is_empty() {
        rel
    } else {
        let with_slash = format!("{prefix}/");
        rel.strip_prefix(&with_slash)?.to_string()
    };
    Some(normalize_module_path(&rel))
}

// ── 依存グラフ ─────────────────────────────────────────────────────────

/// stats から構築した「逆依存」グラフ。
#[derive(Debug, Default)]
pub struct DepGraph {
    /// グラフに現れる全モジュールキー（module 名 + reason 名）。
    nodes: HashSet<String>,
    /// module キー → それを import している側（依存元）の集合。
    /// 変更モジュールから上流（= ストーリー側）へ辿るのに使う。
    dependents: HashMap<String, HashSet<String>>,
}

impl DepGraph {
    /// stats のモジュール一覧から逆依存グラフを組む。
    pub fn build(stats: &WebpackStats) -> Self {
        let mut graph = DepGraph::default();
        for m in &stats.modules {
            graph.ingest(m);
        }
        graph
    }

    fn ingest(&mut self, m: &RawModule) {
        // concatenated の子モジュールも 1 つずつ節点として扱う。
        for child in &m.modules {
            self.ingest(child);
        }

        let name = m.name.as_deref().or(m.name_for_condition.as_deref());
        let Some(name) = name else {
            return;
        };
        let key = normalize_module_path(name);
        if key.is_empty() {
            return;
        }
        self.nodes.insert(key.clone());

        for reason in &m.reasons {
            let Some(importer) = reason.module_name.as_deref() else {
                continue;
            };
            let importer_key = normalize_module_path(importer);
            if importer_key.is_empty() {
                continue;
            }
            self.nodes.insert(importer_key.clone());
            // importer が key を import している → key の依存元に importer を積む。
            self.dependents
                .entry(key.clone())
                .or_default()
                .insert(importer_key);
        }
    }

    /// キーがグラフ上の節点か。
    pub fn contains(&self, key: &str) -> bool {
        self.nodes.contains(key)
    }

    /// 種モジュール群から逆依存を辿り、到達した全モジュール（種自身も含む）。
    pub fn reachable_from(&self, seeds: &[String]) -> HashSet<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = Vec::new();
        for s in seeds {
            if visited.insert(s.clone()) {
                stack.push(s.clone());
            }
        }
        while let Some(cur) = stack.pop() {
            if let Some(deps) = self.dependents.get(&cur) {
                for d in deps {
                    if visited.insert(d.clone()) {
                        stack.push(d.clone());
                    }
                }
            }
        }
        visited
    }
}

// ── 全撮影トリガー / 無視リスト ─────────────────────────────────────────

/// 変更されると差分撮影を諦めて全撮影に倒すべきファイルか。
///
/// 依存関係の解析では波及範囲を追いきれない類（依存の更新・Storybook 設定）を
/// 保守的に全撮影へ倒す。
fn is_full_capture_trigger(repo_relative: &str) -> bool {
    let path = repo_relative.replace('\\', "/");
    let file_name = path.rsplit('/').next().unwrap_or(&path);
    const LOCK_OR_MANIFEST: [&str; 4] = [
        "package.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
    ];
    if LOCK_OR_MANIFEST.contains(&file_name) {
        return true;
    }
    // `.storybook/` 配下（設定・プレビュー・マネージャ）はグラフに出ないことが多い。
    path.split('/').any(|seg| seg == ".storybook")
}

/// グラフ外でも「レンダリングに無関係」として無視してよいパスか。
///
/// 安全側（見逃さない側）に倒すため、無視リストは最小限に留める。
/// ここに載らないグラフ外の変更は全撮影のトリガーになる。
fn is_ignorable_outside_graph(repo_relative: &str) -> bool {
    let path = repo_relative.replace('\\', "/").to_ascii_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(&path);
    // Markdown ドキュメント（README 等）はレンダリング対象のストーリーに影響しない。
    file_name.ends_with(".md")
}

// ── 影響ストーリー算出（本体） ─────────────────────────────────────────

/// 変更ファイル一覧から、撮り直すべきストーリー ID を算出する。
///
/// 入力はすべて呼び出し側が集めたもの（git 差分・stats・index）。この関数自体は
/// 副作用を持たない。判断の優先順位:
///
/// 1. 全撮影トリガー（lockfile / package.json / `.storybook/`）が 1 つでもあれば全撮影
/// 2. 変更ファイルがグラフ外かつ無視リスト外なら、拾い漏れを避けて全撮影
/// 3. それ以外は、変更モジュールから逆依存を辿って到達したストーリーだけ撮影
///
/// 変更が 0 件なら「撮るべきストーリー無し」= 空リスト（新規ストーリーは
/// サーバー側が baseline 不在として撮るので取りこぼさない）。
pub fn compute_affected_stories(
    repo_root: &Path,
    cwd: &Path,
    changed_files: &[String],
    stats: &WebpackStats,
    stories: &[StoryEntry],
) -> Outcome {
    let mut notes = Vec::new();

    // 1. 全撮影トリガー。
    for f in changed_files {
        if is_full_capture_trigger(f) {
            return Outcome {
                plan: Plan::CaptureAll(format!(
                    "changed file `{f}` forces a full capture (dependency/Storybook config change)"
                )),
                notes,
            };
        }
    }

    let graph = DepGraph::build(stats);

    // 2. 変更ファイルを種モジュールキーへ。グラフ外は安全側に倒す。
    let mut seeds: Vec<String> = Vec::new();
    for f in changed_files {
        match changed_path_to_key(repo_root, cwd, f) {
            Some(key) if graph.contains(&key) => seeds.push(key),
            _ => {
                if is_ignorable_outside_graph(f) {
                    notes.push(format!("ignoring `{f}` (not render-relevant)"));
                    continue;
                }
                return Outcome {
                    plan: Plan::CaptureAll(format!(
                        "changed file `{f}` is not in the dependency graph; \
                         capturing everything to avoid missing an affected story"
                    )),
                    notes,
                };
            }
        }
    }

    // 3. 逆依存を辿って到達したストーリーの ID を集める。
    let reached = graph.reachable_from(&seeds);
    // importPath（正規化）→ そのファイルに属するストーリー ID 群。
    let mut by_import: HashMap<String, Vec<String>> = HashMap::new();
    for s in stories {
        by_import
            .entry(normalize_module_path(&s.import_path))
            .or_default()
            .push(s.id.clone());
    }

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for module in &reached {
        if let Some(story_ids) = by_import.get(module) {
            for id in story_ids {
                ids.insert(id.clone());
            }
        }
    }

    Outcome {
        plan: Plan::Only(ids.into_iter().collect()),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    // storybook build を apps/frontend で回した想定（cwd = repo/apps/frontend）。
    fn cwd() -> PathBuf {
        PathBuf::from("/repo/apps/frontend")
    }

    // Button.tsx を Button.stories.tsx が import し、stories が index に載る最小グラフ。
    const STATS: &str = r#"{
      "modules": [
        {
          "name": "./src/Button.tsx",
          "reasons": [
            { "moduleName": "./src/Button.stories.tsx" }
          ]
        },
        {
          "name": "./src/Button.stories.tsx",
          "reasons": []
        },
        {
          "name": "./src/Card.tsx",
          "reasons": [
            { "moduleName": "./src/Card.stories.tsx" }
          ]
        },
        {
          "name": "./src/Card.stories.tsx",
          "reasons": []
        }
      ]
    }"#;

    const INDEX: &str = r#"{
      "v": 5,
      "entries": {
        "button--primary": {
          "id": "button--primary",
          "type": "story",
          "importPath": "./src/Button.stories.tsx"
        },
        "button--secondary": {
          "id": "button--secondary",
          "type": "story",
          "importPath": "./src/Button.stories.tsx"
        },
        "card--default": {
          "id": "card--default",
          "type": "story",
          "importPath": "./src/Card.stories.tsx"
        },
        "intro--docs": {
          "id": "intro--docs",
          "type": "docs",
          "importPath": "./src/Intro.mdx"
        }
      }
    }"#;

    fn stats() -> WebpackStats {
        WebpackStats::parse(STATS).expect("parse stats")
    }

    fn stories() -> Vec<StoryEntry> {
        parse_index(INDEX).expect("parse index")
    }

    #[test]
    fn parse_index_skips_docs_and_missing_import_path() {
        let s = stories();
        // docs エントリ intro--docs は除外され、story 3 件だけ残る。
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|e| e.id != "intro--docs"));
    }

    #[test]
    fn change_reaches_only_affected_stories() {
        // Button.tsx を変更 → Button 系ストーリーだけ（Card は無関係）。
        let changed = vec!["apps/frontend/src/Button.tsx".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert_eq!(
            out.plan,
            Plan::Only(vec![
                "button--primary".to_string(),
                "button--secondary".to_string()
            ])
        );
    }

    #[test]
    fn unrelated_change_reaches_no_story() {
        // Card.tsx を変更 → Card ストーリーだけ、Button には届かない。
        let changed = vec!["apps/frontend/src/Card.tsx".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert_eq!(out.plan, Plan::Only(vec!["card--default".to_string()]));
    }

    #[test]
    fn changing_the_story_file_itself_captures_that_story() {
        let changed = vec!["apps/frontend/src/Button.stories.tsx".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert_eq!(
            out.plan,
            Plan::Only(vec![
                "button--primary".to_string(),
                "button--secondary".to_string()
            ])
        );
    }

    #[test]
    fn lockfile_change_triggers_full_capture() {
        let changed = vec!["pnpm-lock.yaml".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert!(matches!(out.plan, Plan::CaptureAll(_)));
    }

    #[test]
    fn storybook_config_change_triggers_full_capture() {
        let changed = vec!["apps/frontend/.storybook/preview.ts".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert!(matches!(out.plan, Plan::CaptureAll(_)));
    }

    #[test]
    fn package_json_change_triggers_full_capture() {
        let changed = vec!["apps/frontend/package.json".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert!(matches!(out.plan, Plan::CaptureAll(_)));
    }

    #[test]
    fn file_outside_graph_forces_full_capture() {
        // グラフに無い src ファイル（新規や解析漏れ）は安全側に倒す。
        let changed = vec!["apps/frontend/src/Unknown.tsx".to_string()];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert!(matches!(out.plan, Plan::CaptureAll(_)));
    }

    #[test]
    fn markdown_outside_graph_is_ignored() {
        // グラフ外でも .md は無視してよい。加えて Button 変更で Button 系は撮る。
        let changed = vec![
            "README.md".to_string(),
            "apps/frontend/src/Button.tsx".to_string(),
        ];
        let out = compute_affected_stories(&root(), &cwd(), &changed, &stats(), &stories());
        assert_eq!(
            out.plan,
            Plan::Only(vec![
                "button--primary".to_string(),
                "button--secondary".to_string()
            ])
        );
        assert!(out.notes.iter().any(|n| n.contains("README.md")));
    }

    #[test]
    fn no_changes_capture_nothing() {
        let out = compute_affected_stories(&root(), &cwd(), &[], &stats(), &stories());
        assert_eq!(out.plan, Plan::Only(vec![]));
    }

    #[test]
    fn normalize_strips_loaders_and_queries() {
        assert_eq!(
            normalize_module_path("babel-loader!./src/Button.tsx?raw"),
            "src/Button.tsx"
        );
        assert_eq!(normalize_module_path("./src/a.js + 2 modules"), "src/a.js");
    }

    #[test]
    fn changed_path_to_key_absorbs_repo_prefix() {
        assert_eq!(
            changed_path_to_key(&root(), &cwd(), "apps/frontend/src/Button.tsx"),
            Some("src/Button.tsx".to_string())
        );
        // cwd の外は突き合わせ不能なので None。
        assert_eq!(
            changed_path_to_key(&root(), &cwd(), "packages/ui/src/x.tsx"),
            None
        );
    }

    #[test]
    fn changed_path_to_key_when_run_at_repo_root() {
        // cwd == repo_root（単一パッケージ）なら差分なしでそのまま。
        assert_eq!(
            changed_path_to_key(&root(), &root(), "src/Button.tsx"),
            Some("src/Button.tsx".to_string())
        );
    }
}
