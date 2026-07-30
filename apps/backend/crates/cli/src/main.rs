//! `vrt` CLI。CI から 1 コマンドで Storybook バンドルをアップロードし、
//! （任意で）変更ストーリーだけを撮り直させる。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use vrt_cli::api::{BuildResponse, Client};
use vrt_cli::bundle;
use vrt_cli::git;
use vrt_cli::turbosnap::{self, Plan, WebpackStats};

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

#[tokio::main]
async fn main() -> ExitCode {
    // RUST_LOG で調整可能。既定は info。トークンはどのログにも載せない。
    // ログは常に stderr へ出す。stdout は結果（`--json` の 1 行 JSON など）専用に
    // 空けておき、呼び出し元がログと混ざらずに機械可読出力だけをパースできるようにする。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Upload(args) => match run_upload(args).await {
            Ok(code) => code,
            Err(e) => {
                // anyhow のチェーンをまとめて 1 行で出す。
                tracing::error!("{e:#}");
                ExitCode::from(2)
            }
        },
    }
}

async fn run_upload(args: UploadArgs) -> Result<ExitCode> {
    let (tenant_slug, project_slug) = args
        .project
        .split_once('/')
        .filter(|(t, p)| !t.is_empty() && !p.is_empty())
        .context("--project must be in the form `tenant-slug/project-slug`")?;

    // 1. branch / commit を解決（フラグ優先、無ければ git）。
    let branch = match args.branch {
        Some(b) => b,
        None => git::current_branch().context("failed to resolve branch from git")?,
    };
    let commit = match args.commit {
        Some(c) => c,
        None => git::head_commit().context("failed to resolve commit from git")?,
    };
    tracing::info!(%branch, commit = %commit, "resolved build coordinates");

    let client = Client::new(args.url, args.token)?;

    // 2. ビルド作成（mode=storybook）。baseline_commit_sha を差分撮影に使う。
    let build = client
        .create_build(tenant_slug, project_slug, &branch, &commit, None, None)
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
        resolve_only_story_ids(&args.dir, args.stats_json.as_ref(), &build)?
    } else {
        None
    };
    if let Some(ids) = &only_story_ids {
        tracing::info!(count = ids.len(), "finalizing with only_changed story set");
    } else {
        tracing::info!("finalizing with a full capture");
    }
    let finalized = client.finalize(&build.id, only_story_ids).await?;

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
        print_json_result(&final_build, tenant_slug, project_slug, code, None);
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
fn resolve_only_story_ids(
    dir: &Path,
    stats_json: Option<&PathBuf>,
    build: &BuildResponse,
) -> Result<Option<Vec<String>>> {
    // baseline が無ければ差分の起点が無い → 全撮影。
    let Some(baseline) = &build.baseline_commit_sha else {
        tracing::warn!("no baseline commit for this branch yet; capturing all stories");
        return Ok(None);
    };

    // git 差分。履歴不足（shallow clone 等）は全撮影にフォールバック。
    let changed_files = match git::changed_files(baseline) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!("could not diff against baseline ({e:#}); capturing all stories");
            return Ok(None);
        }
    };
    tracing::info!(count = changed_files.len(), "changed files since baseline");

    // stats 欠落 → 全撮影（差分撮影の有効化方法を案内）。
    let stats_path = stats_json
        .cloned()
        .unwrap_or_else(|| dir.join("preview-stats.json"));
    if !stats_path.is_file() {
        tracing::warn!(
            "stats file {} not found; capturing all stories. \
             Run `storybook build --stats-json` to enable per-story capture",
            stats_path.display()
        );
        return Ok(None);
    }
    let stats_raw = std::fs::read_to_string(&stats_path)
        .with_context(|| format!("failed to read {}", stats_path.display()))?;
    let stats = WebpackStats::parse(&stats_raw)
        .with_context(|| format!("failed to parse {}", stats_path.display()))?;

    // index.json（撮影対象ストーリーの列挙）。
    let index_path = dir.join("index.json");
    let index_raw = std::fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read {}", index_path.display()))?;
    let stories = turbosnap::parse_index(&index_raw)
        .with_context(|| format!("failed to parse {}", index_path.display()))?;

    let repo_root = git::repo_root()?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;

    let outcome =
        turbosnap::compute_affected_stories(&repo_root, &cwd, &changed_files, &stats, &stories);
    for note in &outcome.notes {
        tracing::warn!("{note}");
    }
    match outcome.plan {
        Plan::CaptureAll(reason) => {
            tracing::warn!("{reason}; capturing all stories");
            Ok(None)
        }
        Plan::Only(ids) => Ok(Some(ids)),
    }
}

/// ビルドが終端（またはレビュー待ちの changes_detected）になるまでポーリングする。
///
/// 状態取得のたびに進捗ログも増分取得して流す（出力先は通常 stdout、`--json` の
/// ときは stdout を JSON 1 行専用に空けるため stderr）。終端後にも 1 回引いて、
/// 最後の状態遷移と同時に書かれた行を取りこぼさないようにする。
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

/// `cursor` より後のログ行を `[level] message` 形式で stdout に印字し、新しいカーソルを返す。
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
}
