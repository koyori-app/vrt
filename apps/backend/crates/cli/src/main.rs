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
}

#[tokio::main]
async fn main() -> ExitCode {
    // RUST_LOG で調整可能。既定は info。トークンはどのログにも載せない。
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
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

    // 6. 結果を stdout に出す。
    println!("build_id={}", finalized.id);
    println!("status={}", finalized.status);

    if !args.wait {
        return Ok(ExitCode::SUCCESS);
    }

    // --wait: 終端になるまでポーリングして終了コードに反映する。
    let final_build = poll_until_terminal(&client, &finalized.id).await?;
    report_and_exit(&final_build)
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
async fn poll_until_terminal(client: &Client, build_id: &str) -> Result<BuildResponse> {
    const INTERVAL: Duration = Duration::from_secs(3);
    // ジョブが詰まったまま無限待機しないよう上限を設ける。
    const TIMEOUT: Duration = Duration::from_secs(30 * 60);

    let start = std::time::Instant::now();
    loop {
        let build = client.get_build(build_id).await?;
        if is_settled(&build.status) {
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

/// 最終状態を stdout に出し、終了コードを決める。
///
/// passed/approved=0、changes_detected=1、failed/rejected=2。
fn report_and_exit(build: &BuildResponse) -> Result<ExitCode> {
    println!("final_status={}", build.status);
    println!(
        "counts total={} changed={} added={} removed={}",
        build.total_count, build.changed_count, build.added_count, build.removed_count
    );
    if let Some(msg) = &build.error_message {
        println!("error_message={msg}");
    }
    let code = match build.status.as_str() {
        "passed" | "approved" => 0,
        "changes_detected" => 1,
        _ => 2, // failed / rejected / 想定外
    };
    Ok(ExitCode::from(code))
}
