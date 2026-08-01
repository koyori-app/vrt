//! `vrt` CLI。CI から 1 コマンドで Storybook バンドルをアップロードし、
//! （任意で）変更ストーリーだけを撮り直させる。
//!
//! `screenshots` モードはサーバーがレンダリングしないため、撮影は CI 側の
//! テストランナーが行う。その場合は `vrt plan` で「撮る story」の選択計画だけを
//! JSON で受け取り、CI がその集合を自身のテスト選択形式へ翻訳して撮る。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use vrt_cli::api::{BuildResponse, Client, NewBuild};
use vrt_cli::bundle;
use vrt_cli::git;
use vrt_cli::plan::{
    self, CorruptInput, PlanCoordinates, PlanDocument, Selection, SelectionInputs,
};

#[derive(Parser)]
#[command(name = "vrt", version, about = "VRT CI クライアント")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Storybook バンドルをアップロードしてビルドを作成・finalize する。
    Upload(UploadArgs),

    /// `screenshots` モード向けに「撮る story」の選択計画を JSON で出力する。
    ///
    /// 撮影は行わない。CI ランナーがこの JSON を読み、`plan = "only"` のときだけ
    /// 列挙された story を撮る。
    Plan(PlanArgs),
}

#[derive(Parser)]
struct UploadArgs {
    /// VRT のベース URL。
    #[arg(long, env = "VRT_URL")]
    url: String,

    /// CI トークン（PAT）。ログには出力しない。
    #[arg(long, env = "VRT_TOKEN", hide_env_values = true)]
    token: String,

    /// 対象プロジェクトを `tenant-slug/project-slug` で指定する。
    ///
    /// create_build のパスが tenant/project の slug を要求するため必須。
    /// プロジェクト画面の CI usage タブに出る値をそのまま渡す。
    #[arg(long, env = "VRT_PROJECT")]
    project: String,

    /// アップロードする storybook-static ディレクトリ。
    #[arg(long, default_value = "./storybook-static")]
    dir: PathBuf,

    /// ブランチ名。省略時は git から取得する。
    #[arg(long)]
    branch: Option<String>,

    /// commit SHA。省略時は git から取得する。
    #[arg(long)]
    commit: Option<String>,

    /// 変更されたストーリーだけ撮り直す（Chromatic の --only-changed 相当）。
    #[arg(long)]
    only_changed: bool,

    /// webpack stats JSON のパス。省略時は `<dir>/preview-stats.json`。
    #[arg(long)]
    stats_json: Option<PathBuf>,

    /// ビルドが終端状態になるまでポーリングして待つ。
    #[arg(long)]
    wait: bool,

    /// 機械可読な結果を stdout へ JSON で 1 行出力する。
    #[arg(long)]
    json: bool,
}

#[derive(Parser)]
struct PlanArgs {
    /// VRT のベース URL。`--baseline-commit` を渡す場合は不要。
    #[arg(long, env = "VRT_URL")]
    url: Option<String>,

    /// CI トークン（PAT）。ログには出力しない。`--baseline-commit` を渡す場合は不要。
    #[arg(long, env = "VRT_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// 対象プロジェクトを `tenant-slug/project-slug` で指定する。
    /// `--baseline-commit` を渡す場合は不要。
    #[arg(long, env = "VRT_PROJECT")]
    project: Option<String>,

    /// stats / index を探すディレクトリ。
    #[arg(long, default_value = "./storybook-static")]
    dir: PathBuf,

    /// ブランチ名。省略時は git から取得する。
    #[arg(long)]
    branch: Option<String>,

    /// HEAD の commit SHA。省略時は git から取得する。
    #[arg(long)]
    commit: Option<String>,

    /// 差分の起点となる baseline commit SHA。
    ///
    /// 省略時は `screenshots` モードのビルドを作成し、その作成レスポンスから
    /// baseline を受け取る（撮影前に起点を知るための唯一の経路）。作成した
    /// ビルド ID は計画の `build_id` に載るので、CI はそのビルドへ撮影結果を送る。
    #[arg(long)]
    baseline_commit: Option<String>,

    /// webpack stats JSON のパス。省略時は `<dir>/preview-stats.json`。
    #[arg(long)]
    stats_json: Option<PathBuf>,

    /// Storybook index JSON のパス。省略時は `<dir>/index.json`。
    #[arg(long)]
    index_json: Option<PathBuf>,

    /// 計画 JSON の書き出し先。指定しても stdout へは常に出す。
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Upload(args) => {
            // ログは stderr。stdout は結果（`--json` の 1 行 JSON や key=value）専用。
            init_logging();
            finish(run_upload(args).await)
        }
        Command::Plan(args) => {
            // stdout は計画 JSON 専用。ログが混ざると CI 側の解析が壊れる。
            init_logging();
            finish(run_plan(args).await)
        }
    }
}

/// RUST_LOG で調整可能。既定は info。トークンはどのログにも載せない。
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// 失敗は anyhow のチェーンをまとめて 1 行で出し、終了コード 2 にする。
fn finish(result: Result<ExitCode>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::from(2)
        }
    }
}

/// `tenant-slug/project-slug` を分解する。
fn split_project(project: &str) -> Result<(&str, &str)> {
    project
        .split_once('/')
        .filter(|(t, p)| !t.is_empty() && !p.is_empty())
        .context("--project must be in the form `tenant-slug/project-slug`")
}

async fn run_upload(args: UploadArgs) -> Result<ExitCode> {
    let (tenant_slug, project_slug) = split_project(&args.project)?;

    // 1. branch / commit を解決（フラグ優先、無ければ git）。
    let branch = match args.branch {
        Some(b) => b,
        None => git::current_branch().context("failed to resolve branch from git")?,
    };
    let commit = match args.commit {
        Some(c) => git::resolve_commit(&c).context("failed to resolve --commit")?,
        None => git::head_commit().context("failed to resolve commit from git")?,
    };
    tracing::info!(%branch, commit = %commit, "resolved build coordinates");

    let client = Client::new(args.url, args.token)?;

    // 2. ビルド作成（mode=storybook）。baseline_commit_sha を差分撮影に使う。
    let build = client
        .create_build(&NewBuild {
            tenant_slug,
            project_slug,
            branch: &branch,
            commit_sha: &commit,
            commit_message: None,
            pull_request_number: None,
            mode: "storybook",
        })
        .await?;
    tracing::info!(build_id = %build.id, "created build");

    // 3. zip 化（index.json 検証・200MB 検査は bundle 側）。
    let zip_bytes = bundle::zip_dir(&args.dir)?;
    tracing::info!(bytes = zip_bytes.len(), "zipped storybook bundle");

    // 4. アップロード。
    client.upload_storybook(&build.id, zip_bytes).await?;
    tracing::info!("uploaded storybook bundle");

    // 5. finalize（--only-changed なら影響ストーリーだけ）。
    let only_story_ids = if args.only_changed {
        resolve_only_story_ids(&args.dir, args.stats_json.as_deref(), &build, &commit)?
    } else {
        None
    };
    if let Some(ids) = &only_story_ids {
        tracing::info!(count = ids.len(), "finalizing with only_changed story set");
    } else {
        tracing::info!("finalizing with a full capture");
    }
    // 差分撮影のときだけ、計画の起点にした baseline をサーバーの固定値と照合させる。
    // 全撮影は baseline がどれでも結果が変わらないため添えない。
    let expected_baseline = if only_story_ids.is_some() {
        build.baseline_commit_sha.as_deref()
    } else {
        None
    };
    let finalized = client
        .finalize(&build.id, only_story_ids, expected_baseline)
        .await?;

    // 6. 結果を出す。--json のときは stdout を JSON 1 行専用にするため、
    //    人間向けの `key=value` サマリは出さない（ログは既に stderr）。
    if !args.json {
        println!("build_id={}", finalized.id);
        println!("status={}", finalized.status);
    }

    if !args.wait {
        // --wait 無し: finalize 直後の状態を返し、終了コードは常に 0（既存挙動）。
        if args.json {
            print_json_result(&finalized, tenant_slug, project_slug, 0, None);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // --wait: 終端になるまでポーリングして終了コードに反映する。
    let final_build = match poll_until_terminal(&client, &finalized.id, args.json).await {
        Ok(build) => build,
        Err(e) => {
            // finalize は成功済み。`--json` の契約は「finalize まで到達すれば JSON が
            // 1 行出る」なので、ここでポーリングが一時的な通信失敗やタイムアウトで
            // Err になっても、`?` で早期リターンして stdout を空にしてはいけない。
            // 呼び出し元が build_id すら取れなくなるため、既知の finalize 済み情報
            // （build_id / build_number / slug / finalize 直後の status）に exit_code=2
            // と失敗理由 `error` を添えて 1 行だけ出す。
            if args.json {
                print_json_result(
                    &finalized,
                    tenant_slug,
                    project_slug,
                    2,
                    Some(&format!("{e:#}")),
                );
                // ログは従来どおり stderr へも残す（main のハンドラは通らないため自前で）。
                tracing::error!("{e:#}");
                return Ok(ExitCode::from(2));
            }
            // `--json` でないときは従来どおり伝播し、main 側で error ログ + exit 2。
            return Err(e);
        }
    };
    let code = exit_code_for(&final_build.status);
    if !args.json {
        report(&final_build);
    } else {
        print_json_result(
            &final_build,
            tenant_slug,
            project_slug,
            code,
            final_build.error_message.as_deref(),
        );
    }
    Ok(ExitCode::from(code))
}

/// 機械可読な結果を stdout へ JSON で 1 行出力する。
fn print_json_result(
    build: &BuildResponse,
    tenant_slug: &str,
    project_slug: &str,
    exit_code: u8,
    error: Option<&str>,
) {
    let out = json_result_value(build, tenant_slug, project_slug, exit_code, error);
    println!("{out}");
}

/// `--json` の 1 行 JSON の値を組み立てる純関数。
///
/// `error` は失敗時のみ `Some` を渡す。`None` のときは成功パスと同じ形状
/// （`error` キーは出現しない）を保つ。
fn json_result_value(
    build: &BuildResponse,
    tenant_slug: &str,
    project_slug: &str,
    exit_code: u8,
    error: Option<&str>,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        "build_id": build.id,
        "build_number": build.number,
        "tenant_slug": tenant_slug,
        "project_slug": project_slug,
        "status": build.status,
        "exit_code": exit_code,
    });
    if let Some(message) = error {
        out["error"] = serde_json::Value::String(message.to_string());
    }
    out
}

/// `--only-changed` の影響ストーリーを算出する。
///
/// 差分撮影が成立しない条件（baseline 無し / git 履歴不足 / stats 欠落 /
/// グラフ外の変更）はすべて全撮影（`None`）へ倒し、理由を警告として出す。
/// 選択そのものは `screenshots` の `vrt plan` と同じ [`plan::select_stories`] を通す。
///
/// 入力が読めたが壊れていた場合は [`CorruptInput::Error`] でエラーへ倒す
/// （`storybook` モードの既存挙動を保つため。壊れた stats を黙って
/// 全撮影に読み替えると、設定ミスに気づけなくなる）。
fn resolve_only_story_ids(
    dir: &Path,
    stats_json: Option<&Path>,
    build: &BuildResponse,
    commit: &str,
) -> Result<Option<Vec<String>>> {
    // baseline が無ければ差分の起点が無い → 全撮影。
    let Some(baseline) = &build.baseline_commit_sha else {
        tracing::warn!("no baseline commit for this branch yet; capturing all stories");
        return Ok(None);
    };

    // git 差分。`--commit` で記録した終点と baseline の 2 点間を使う（worktree HEAD ではない）。
    // 履歴不足（shallow clone 等）は全撮影にフォールバック。
    let changed_files = match git::changed_files(baseline, commit) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!("could not diff against baseline ({e:#}); capturing all stories");
            return Ok(None);
        }
    };
    tracing::info!(count = changed_files.len(), "changed files since baseline");

    // 絞り込みの入力（stats / index）は worktree のファイル。worktree が
    // 記録した commit と一致していなければ、選別は別コミットの内容を見てしまう。
    // ここは全撮影へ倒さずエラーにする（設定ミスを黙って読み替えない）。
    git::verify_worktree_matches(commit)?;

    let repo_root = git::repo_root()?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;

    let selection = plan::select_stories(
        &SelectionInputs {
            dir,
            stats_json,
            index_json: None,
            repo_root: &repo_root,
            cwd: &cwd,
            changed_files: &changed_files,
        },
        CorruptInput::Error,
    )?;

    match selection {
        Selection::CaptureAll { reason, notes } => {
            warn_notes(&notes);
            tracing::warn!("{reason}; capturing all stories");
            Ok(None)
        }
        Selection::Only { story_ids, notes } => {
            warn_notes(&notes);
            Ok(Some(story_ids))
        }
    }
}

fn warn_notes(notes: &[String]) {
    for note in notes {
        tracing::warn!("{note}");
    }
}

/// `screenshots` モード向けの選択計画を組んで JSON で出す。撮影はしない。
///
/// 判断できない条件（baseline 無し / git 履歴不足 / stats 欠落・破損 /
/// グラフ外の変更 / 依存の更新）はすべて `plan = "capture_all"` へ倒す。
/// 撮らなかった story を「差分なし」と読み替えないのはサーバー側の責務なので、
/// ここでは「撮る集合」と「全撮影へ倒した理由」を落とさず出すことに徹する。
async fn run_plan(args: PlanArgs) -> Result<ExitCode> {
    let branch = match args.branch {
        Some(b) => b,
        None => git::current_branch().context("failed to resolve branch from git")?,
    };
    let head_commit_sha = match args.commit {
        Some(c) => git::resolve_commit(&c).context("failed to resolve --commit")?,
        None => git::head_commit().context("failed to resolve commit from git")?,
    };

    // baseline の入手経路。明示指定が優先。無ければ screenshots ビルドを作り、
    // その作成レスポンスから受け取る（撮影前に起点を知れる唯一の経路）。
    let explicit_baseline = args.baseline_commit.clone();
    let (baseline_commit_sha, build_id) = match args.baseline_commit {
        Some(sha) => {
            let oid = git::resolve_commit(&sha)
                .with_context(|| format!("baseline commit {sha} is not present locally"))?;
            (Some(oid), None)
        }
        None => {
            let (Some(url), Some(token), Some(project)) = (args.url, args.token, args.project)
            else {
                bail!(
                    "pass --baseline-commit, or --url/--token/--project so the baseline \
                     can be resolved from a new screenshots build"
                );
            };
            let (tenant_slug, project_slug) = split_project(&project)?;
            let client = Client::new(url, token)?;
            let build = client
                .create_build(&NewBuild {
                    tenant_slug,
                    project_slug,
                    branch: &branch,
                    commit_sha: &head_commit_sha,
                    commit_message: None,
                    pull_request_number: None,
                    mode: "screenshots",
                })
                .await?;
            tracing::info!(build_id = %build.id, "created screenshots build");
            (build.baseline_commit_sha, Some(build.id))
        }
    };

    let head_for_diff = head_commit_sha.clone();
    let coords = PlanCoordinates {
        branch,
        baseline_commit_sha: baseline_commit_sha.clone(),
        head_commit_sha,
        build_id,
    };

    let document = match &baseline_commit_sha {
        // baseline が無ければ差分の起点が無い → 全撮影。
        None => PlanDocument::capture_all(
            coords,
            "no baseline commit for this branch yet".to_string(),
            Vec::new(),
        ),
        Some(baseline) => match git::changed_files(baseline, &head_for_diff) {
            // 履歴不足（shallow clone 等）→ 全撮影。明示 baseline は上で存在確認済みなので
            // ここへ来るのはサーバー解決経路のみ。
            Err(e) if explicit_baseline.is_some() => {
                return Err(e).context("could not diff against baseline");
            }
            Err(e) => PlanDocument::capture_all(
                coords,
                format!("could not diff against baseline: {e:#}"),
                Vec::new(),
            ),
            Ok(changed_files) => {
                tracing::info!(count = changed_files.len(), "changed files since baseline");
                // 選別の入力（stats / index）は worktree 由来。worktree が計画の
                // 終点 commit と一致しない・追跡ファイルが汚れている状態で
                // 絞り込むと、別内容に対する計画になる。エラーで止める。
                git::verify_worktree_matches(&head_for_diff)?;
                let repo_root = git::repo_root()?;
                let cwd = std::env::current_dir().context("failed to read current directory")?;
                let selection = plan::select_stories(
                    &SelectionInputs {
                        dir: &args.dir,
                        stats_json: args.stats_json.as_deref(),
                        index_json: args.index_json.as_deref(),
                        repo_root: &repo_root,
                        cwd: &cwd,
                        changed_files: &changed_files,
                    },
                    CorruptInput::FailClosed,
                )?;
                PlanDocument::from_selection(coords, selection)
            }
        },
    };

    warn_notes(&document.notes);
    if let Some(reason) = &document.reason {
        tracing::warn!("{reason}; capturing all stories");
    } else if let Some(ids) = &document.story_ids {
        tracing::info!(count = ids.len(), "planned story set");
    }

    let json = document.to_json()?;
    println!("{json}");
    if let Some(path) = &args.output {
        std::fs::write(path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
        tracing::info!(path = %path.display(), "wrote the selection plan");
    }
    Ok(ExitCode::SUCCESS)
}

/// ビルドが終端（またはレビュー待ちの changes_detected）になるまでポーリングする。
///
/// 状態取得のたびに進捗ログも増分取得して流す（`--json` のときは stderr）。
/// 終端後にも 1 回引いて、最後の状態遷移と同時に書かれた行を取りこぼさない。
async fn poll_until_terminal(client: &Client, build_id: &str, json: bool) -> Result<BuildResponse> {
    const INTERVAL: Duration = Duration::from_secs(3);
    // ジョブが詰まったまま無限待機しないよう上限を設ける。
    const TIMEOUT: Duration = Duration::from_secs(30 * 60);

    let start = std::time::Instant::now();
    // 追尾済みログの末尾 id。ここより後の行だけを毎回引く。
    let mut log_cursor: i64 = 0;
    loop {
        log_cursor = flush_logs(client, build_id, log_cursor, json).await;

        let build = client.get_build(build_id).await?;
        if is_settled(&build.status) {
            // 終端遷移と同時に書かれた行（完了サマリ・失敗理由）を最後に流し切る。
            flush_logs(client, build_id, log_cursor, json).await;
            return Ok(build);
        }
        if start.elapsed() >= TIMEOUT {
            bail!(
                "timed out after {}s waiting for build {build_id} (last status: {})",
                TIMEOUT.as_secs(),
                build.status
            );
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

/// `cursor` より後のログ行を `[level] message` 形式で印字し、新しいカーソルを返す。
///
/// ログ取得の失敗はビルド待機の失敗にはしない（警告だけ出して次に進む）。
/// 進捗ログは補助情報であり、これで終了コードを狂わせたくない。
async fn flush_logs(client: &Client, build_id: &str, cursor: i64, json: bool) -> i64 {
    match client.get_build_logs(build_id, cursor).await {
        Ok(logs) => {
            for entry in &logs.entries {
                // --json のときは stdout を JSON 専用にするため進捗ログは stderr へ回す。
                if json {
                    eprintln!("[{}] {}", entry.level, entry.message);
                } else {
                    println!("[{}] {}", entry.level, entry.message);
                }
            }
            logs.last_id
        }
        Err(e) => {
            tracing::warn!("could not fetch build logs ({e:#}); continuing");
            cursor
        }
    }
}

/// CI として待つのを止めてよい状態か。
///
/// `changes_detected` はサーバー的には終端ではない（レビューで動く）が、
/// パイプラインとしては結果が出ているので CI は待たない。
fn is_settled(status: &str) -> bool {
    matches!(
        status,
        "passed" | "failed" | "changes_detected" | "approved" | "rejected"
    )
}

/// 最終状態を人間向けに stdout へ出す。
fn report(build: &BuildResponse) {
    println!("final_status={}", build.status);
    println!(
        "counts total={} changed={} added={} removed={}",
        build.total_count, build.changed_count, build.added_count, build.removed_count
    );
    if let Some(msg) = &build.error_message {
        println!("error_message={msg}");
    }
}

/// 最終状態から終了コードを決める。
///
/// passed/approved=0、changes_detected=1、failed/rejected=2。
fn exit_code_for(status: &str) -> u8 {
    match status {
        "passed" | "approved" => 0,
        "changes_detected" => 1,
        _ => 2, // failed / rejected / 想定外
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static REPO_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 終了コードは CI の合否判定そのものなので、8 状態すべてを固定する。
    /// `changes_detected` は「差分あり = レビュー待ち」であって失敗ではないため、
    /// 1 に割り当てて失敗（2）と区別する。
    #[test]
    fn exit_code_covers_every_build_status() {
        assert_eq!(exit_code_for("passed"), 0);
        assert_eq!(exit_code_for("approved"), 0);
        assert_eq!(exit_code_for("changes_detected"), 1);
        assert_eq!(exit_code_for("failed"), 2);
        assert_eq!(exit_code_for("rejected"), 2);
        // 非終端状態がここへ来るのは想定外なので、成功に倒さず 2 にする。
        assert_eq!(exit_code_for("pending"), 2);
        assert_eq!(exit_code_for("rendering"), 2);
        assert_eq!(exit_code_for("processing"), 2);
    }

    /// サーバー側に未知の状態が増えても、黙って 0（成功）にしないことを固定する。
    #[test]
    fn unknown_status_is_not_treated_as_success() {
        assert_eq!(exit_code_for("some_future_status"), 2);
        assert_eq!(exit_code_for(""), 2);
    }

    /// `--json` 出力のフィールド検証用に、既知の値を持つ BuildResponse を作る。
    fn sample_build() -> BuildResponse {
        BuildResponse {
            id: "build-123".into(),
            number: 42,
            // finalize 直後の（＝最後に判明している）状態を模す。
            status: "processing".into(),
            baseline_commit_sha: None,
            total_count: 0,
            changed_count: 0,
            added_count: 0,
            removed_count: 0,
            error_message: None,
        }
    }

    /// 成功パスでは `error` キーが出現しないこと（JSON 形状を変えない）を固定する。
    #[test]
    fn json_result_omits_error_on_success() {
        let build = sample_build();
        let value = json_result_value(&build, "acme", "web", 0, None);

        assert_eq!(value["build_id"], "build-123");
        assert_eq!(value["build_number"], 42);
        assert_eq!(value["tenant_slug"], "acme");
        assert_eq!(value["project_slug"], "web");
        assert_eq!(value["status"], "processing");
        assert_eq!(value["exit_code"], 0);
        assert!(
            value.get("error").is_none(),
            "success output must not carry an error field"
        );
        // 1 行 JSON の契約: 改行を含まない。
        assert!(!value.to_string().contains('\n'));
    }

    /// ポーリング失敗時は finalize 済みの build_id 等 + exit_code=2 + error を出す。
    /// finalize 後の状態がそのまま status に出ること（poll 失敗でも既知値を返す）を固定。
    #[test]
    fn json_result_includes_error_and_exit_2_on_poll_failure() {
        let finalized = sample_build();
        let value = json_result_value(
            &finalized,
            "acme",
            "web",
            2,
            Some("timed out after 1800s waiting for build build-123 (last status: processing)"),
        );

        // build_id / build_number / slug / status は finalize 済みの既知値。
        assert_eq!(value["build_id"], "build-123");
        assert_eq!(value["build_number"], 42);
        assert_eq!(value["tenant_slug"], "acme");
        assert_eq!(value["project_slug"], "web");
        assert_eq!(value["status"], "processing");
        // 失敗を表す終了コードと理由。
        assert_eq!(value["exit_code"], 2);
        assert_eq!(
            value["error"],
            "timed out after 1800s waiting for build build-123 (last status: processing)"
        );
        assert!(!value.to_string().contains('\n'));
    }

    /// ビルドが failed / rejected で終わり、サーバーが error_message を返した場合、
    /// run_upload の終端パス（error_message.as_deref() 経由）で `error` キーに
    /// その理由が載ること、および exit_code=2 が付くことを固定する。
    #[test]
    fn json_result_carries_server_error_message_on_failed_build() {
        let mut build = sample_build();
        build.status = "failed".into();
        build.error_message = Some("render worker crashed for 3 stories".into());

        let code = exit_code_for(&build.status);
        let value = json_result_value(&build, "acme", "web", code, build.error_message.as_deref());

        assert_eq!(value["status"], "failed");
        assert_eq!(value["exit_code"], 2);
        assert_eq!(value["error"], "render worker crashed for 3 stories");
        assert!(!value.to_string().contains('\n'));
    }

    #[test]
    fn settled_statuses_stop_the_poll_loop() {
        for s in [
            "passed",
            "changes_detected",
            "failed",
            "approved",
            "rejected",
        ] {
            assert!(is_settled(s), "{s} should be terminal");
        }
        for s in ["pending", "rendering", "processing"] {
            assert!(!is_settled(s), "{s} should not be terminal");
        }
    }

    struct ChdirGuard {
        previous: PathBuf,
    }

    impl ChdirGuard {
        fn change_to(dir: &Path) -> Self {
            let previous = std::env::current_dir().expect("cwd");
            std::env::set_current_dir(dir).expect("chdir");
            Self { previous }
        }
    }

    impl Drop for ChdirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("spawn git");
        assert!(
            status.success(),
            "`git {}` failed in {}",
            args.join(" "),
            dir.display()
        );
    }

    fn git_output(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "`git {}` failed in {}",
            args.join(" "),
            dir.display()
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// upload の only-changed 用。c2 で A.tsx、c3 で B.tsx を変更する 3 段履歴。
    fn init_story_diff_repo() -> (tempfile::TempDir, String, String, String, PathBuf) {
        use std::fs;

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        git_in(root, &["init", "-b", "main"]);
        git_in(root, &["config", "user.email", "vrt@test.local"]);
        git_in(root, &["config", "user.name", "vrt test"]);

        let a_path = root.join("apps/frontend/src/A.tsx");
        let b_path = root.join("apps/frontend/src/B.tsx");
        fs::create_dir_all(a_path.parent().expect("parent")).expect("mkdir");
        fs::write(&a_path, "export const A = 1;\n").expect("write A");
        fs::write(&b_path, "export const B = 1;\n").expect("write B");
        git_in(root, &["add", "apps"]);
        git_in(root, &["commit", "-m", "c1"]);
        let c1 = git_output(root, &["rev-parse", "HEAD"]);

        fs::write(&a_path, "export const A = 2;\n").expect("write A");
        git_in(root, &["add", "apps/frontend/src/A.tsx"]);
        git_in(root, &["commit", "-m", "c2"]);
        let c2 = git_output(root, &["rev-parse", "HEAD"]);

        fs::write(&b_path, "export const B = 2;\n").expect("write B");
        git_in(root, &["add", "apps/frontend/src/B.tsx"]);
        git_in(root, &["commit", "-m", "c3"]);
        let c3 = git_output(root, &["rev-parse", "HEAD"]);

        let frontend = root.join("apps/frontend");
        (tmp, c1, c2, c3, frontend)
    }

    fn pending_build(baseline: String) -> BuildResponse {
        BuildResponse {
            id: "build-test".into(),
            number: 1,
            status: "pending".into(),
            baseline_commit_sha: Some(baseline),
            total_count: 0,
            changed_count: 0,
            added_count: 0,
            removed_count: 0,
            error_message: None,
        }
    }

    /// `--only-changed` の絞り込みは、worktree が記録した `--commit` と一致して
    /// いなければエラーにする。stats / index は worktree から読むため、終点だけを
    /// `--commit` に固定しても worktree が別コミットなら別内容に対する選別になる。
    /// positive control: 終点固定だけで成功していた旧実装ではここが Ok になり、このテストは落ちる。
    #[test]
    fn only_changed_uses_explicit_commit_not_worktree_head() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let build = pending_build(c1);
        let graph_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph");

        // worktree は c3 のまま --commit c2 → 前提不一致でエラー。
        let err = resolve_only_story_ids(&graph_fixture, None, &build, &c2)
            .expect_err("worktree/commit mismatch must not narrow silently");
        assert!(
            format!("{err:#}").contains("does not match the recorded commit"),
            "err={err:#}"
        );

        let _keep_tmp = tmp;
    }

    /// worktree を `--commit` に合わせてあれば絞り込みが成立する（過剰ブロックの回帰防止）。
    #[test]
    fn only_changed_narrows_when_worktree_matches_the_recorded_commit() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        git_in(tmp.path(), &["checkout", "--detach", &c2]);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let build = pending_build(c1);
        let graph_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph");

        let only = resolve_only_story_ids(&graph_fixture, None, &build, &c2).expect("resolve");
        let ids = only.expect("expected a narrowed story set");
        assert_eq!(
            ids,
            vec!["a--one".to_string(), "a--two".to_string()],
            "diff end is --commit (c2: A.tsx only), so only A stories are selected"
        );

        let _keep_tmp = tmp;
    }
}
