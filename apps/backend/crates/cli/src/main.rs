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
use vrt_cli::provenance::{self, ArtifactPaths, Verification};

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

    /// storybook build を実行し、成果物に生成元コミットの provenance を書き込む。
    ///
    /// `vrt stamp -- <build command...>` の形で build コマンドを渡す。vrt が
    /// build 開始前に HEAD と worktree の clean を観測して**旧 provenance と
    /// stats / index を削除**し、build を実行し、成功後に同一 HEAD・clean を
    /// 再観測してから stamp する——「build がその commit で走り、成果物を
    /// 再生成した」ことを vrt 自身の計器で証明するためである。
    /// `vrt plan` / `vrt upload --only-changed` は絞り込みの前にこれを検証し、
    /// 別コミットで生成された成果物（古いキャッシュ等）での絞り込みを拒否する。
    Stamp(StampArgs),
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

#[derive(Parser)]
struct StampArgs {
    /// provenance を書き込む storybook-static ディレクトリ。
    #[arg(long, default_value = "./storybook-static")]
    dir: PathBuf,

    /// webpack stats JSON のパス。省略時は `<dir>/preview-stats.json`。
    /// plan / upload に渡すのと同じ値を渡すこと（解決規則が同一）。
    #[arg(long)]
    stats_json: Option<PathBuf>,

    /// Storybook index JSON のパス。省略時は `<dir>/index.json`。
    #[arg(long)]
    index_json: Option<PathBuf>,

    /// storybook build を実行するコマンド。`--` の後に argv をそのまま並べる
    /// （シェル展開はしない。シェル機能が要るなら `sh -c '...'` を渡す）。
    ///
    /// 例: `vrt stamp --dir ./storybook-static -- pnpm build-storybook --stats-json`
    ///
    /// build 後の stamp だけを単独で行う形は提供しない——build と stamp の間に
    /// checkout が挟まると「その commit でビルドした」証明にならないためである。
    #[arg(last = true, required = true, num_args = 1..)]
    build_command: Vec<String>,
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
        Command::Stamp(args) => {
            init_logging();
            finish(run_stamp(args))
        }
    }
}

/// `vrt stamp -- <build command>`: build を実行し、成果物へ provenance を書き込む。
///
/// 「stamp 時点の HEAD」ではなく「build がその HEAD で走った」ことを証明する
/// ため、観測は build を挟んで二度行う。さらに「コマンドが成功した」ことと
/// 「成果物がその build で生成された」ことは別なので、build 前に旧成果物を
/// 無効化し、build 後の存在をもって再生成を証明する。
///
/// 1. build 開始前: HEAD を解決し、worktree が clean（追跡ファイルに
///    未コミット変更なし）であることを検査する
/// 2. 旧成果物の無効化: 旧 provenance と stats / index を削除する。何も
///    生成しない命令が build 前の成果物をそのまま stamp する経路を断ち、
///    build が失敗しても古い証明が残らないようにする
/// 3. build コマンドを実行する（失敗したら stamp しない）
/// 4. build 成功後: HEAD が開始前と同一のまま動いていないこと、worktree が
///    依然 clean であることを再検査する（build 中の checkout / commit /
///    追跡ファイル書き換えはここで検出され、stamp は行われない）
/// 5. その HEAD で provenance を書く。stats / index は手順 2 で消されている
///    ため、ここで存在する＝build の実行中に生成されたものである
fn run_stamp(args: StampArgs) -> Result<ExitCode> {
    // 1. build 開始前の観測。
    let head_before =
        git::head_commit().context("failed to resolve HEAD; run inside a git checkout")?;
    git::verify_worktree_matches(&head_before)
        .context("the worktree must be clean before the build so the stamp can prove it")?;

    // 2. 旧成果物の無効化。storybook には HEAD に束縛された信頼できる
    //    build-time marker が無いため、cache-hit を証明で受け入れる形は取らず、
    //    実入力 2 ファイルの再生成を build に強制する（実際の storybook build は
    //    常に両ファイルを書くので、失敗するのは生成しない命令だけである）。
    let paths = ArtifactPaths {
        dir: &args.dir,
        stats_json: args.stats_json.as_deref(),
        index_json: args.index_json.as_deref(),
    };
    provenance::invalidate(&paths)?;
    tracing::info!(
        dir = %args.dir.display(),
        "invalidated the previous provenance and artifacts; the build must regenerate them"
    );

    // 3. build 実行。argv をそのまま起動する（シェルを介さない）。
    let (program, rest) = args
        .build_command
        .split_first()
        .context("pass the build command after `--`")?;
    tracing::info!(command = %args.build_command.join(" "), commit = %head_before, "running the build");
    let status = std::process::Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("failed to spawn the build command `{program}`"))?;
    if !status.success() {
        bail!(
            "the build command `{}` failed ({status}); nothing was stamped",
            args.build_command.join(" ")
        );
    }

    // 4. build 成功後の再観測。HEAD が動いた・worktree が汚れた build は
    //    「head_before でビルドした」証明にならないので stamp しない。
    let head_after = git::head_commit().context("failed to re-resolve HEAD after the build")?;
    if head_after != head_before {
        bail!(
            "HEAD moved during the build ({head_before} -> {head_after}); \
             the artifact cannot be attributed to a single commit, so nothing was stamped"
        );
    }
    git::verify_worktree_matches(&head_after).context(
        "the build left the worktree dirty, so the artifact cannot be attributed to HEAD",
    )?;

    // 5. stamp。stats / index は手順 2 で消してあるので、stamp が要求する
    //    存在検査を通る＝この build の実行中に再生成されたことの証明になる。
    let out = provenance::stamp(&paths, &head_after, &args.build_command)?;
    tracing::info!(commit = %head_after, path = %out.display(), "stamped artifact provenance");
    Ok(ExitCode::SUCCESS)
}

/// 絞り込みの前に、成果物が計画の終点 commit で生成されたものか検証する。
///
/// `Verified` = 絞り込んでよい。`Missing` / `Unowned`（v1 の旧形式）は
/// 呼び出し側が全撮影へ倒す。不一致・破損は `Err`（設定ミスを黙って
/// 全撮影に読み替えない——worktree 不一致と同じ方針）。
fn verify_artifact_provenance(
    dir: &Path,
    stats_json: Option<&Path>,
    index_json: Option<&Path>,
    head_commit_sha: &str,
) -> Result<Verification> {
    let paths = ArtifactPaths {
        dir,
        stats_json,
        index_json,
    };
    provenance::verify(&paths, head_commit_sha)
}

/// provenance が絞り込みを許さなかった（`Missing` / `Unowned`）ときの説明文。
/// plan の `reason` と upload の警告ログで共用する。
fn provenance_fallback_reason(verification: &Verification, dir: &Path) -> String {
    match verification {
        Verification::Missing => format!(
            "no artifact provenance ({} in {}). build via `vrt stamp -- <build command>` \
             to bind the artifact to its commit and enable per-story capture",
            provenance::PROVENANCE_FILE,
            dir.display()
        ),
        Verification::Unowned => format!(
            "the artifact provenance in {} predates build ownership (version 1: it proves \
             the HEAD at stamp time, not at build time). re-build via \
             `vrt stamp -- <build command>` to enable per-story capture",
            dir.display()
        ),
        Verification::Verified => unreachable!("verified provenance never falls back"),
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

    // worktree 検査は tracked しか見ない。実入力の stats / index は通常
    // untracked なので、成果物自体が `commit` で生成された証明（provenance）を
    // 別途検証する。不在と build 所有なしの旧形式（v1）は全撮影へ
    // （stats 不在と同じ扱い）、不一致はエラー。
    let verification = verify_artifact_provenance(dir, stats_json, None, commit)?;
    if verification != Verification::Verified {
        tracing::warn!(
            "{}; capturing all stories",
            provenance_fallback_reason(&verification, dir)
        );
        return Ok(None);
    }

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
        Selection::Only {
            story_ids, notes, ..
        } => {
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
    // サーバー経路では選択計画をビルドへ固定するためクライアントを持ち回る。
    let explicit_baseline = args.baseline_commit.clone();
    let mut client: Option<Client> = None;
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
            let c = Client::new(url, token)?;
            let build = c
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
            client = Some(c);
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

    // `plan = "only"` のときの母集合（現行 index の全 story ID）。
    // サーバーへ計画を固定するときの manifest_names に使う。
    let mut manifest_story_ids: Option<Vec<String>> = None;

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
                // worktree 検査は tracked のみ。untracked の stats / index が
                // `head_for_diff` で生成された成果物であることは provenance で
                // 検証する。不在と build 所有なしの旧形式（v1）は全撮影へ倒し
                // （移行期）、不一致はエラー。
                let verification = verify_artifact_provenance(
                    &args.dir,
                    args.stats_json.as_deref(),
                    args.index_json.as_deref(),
                    &head_for_diff,
                )?;
                if verification != Verification::Verified {
                    PlanDocument::capture_all(
                        coords,
                        provenance_fallback_reason(&verification, &args.dir),
                        Vec::new(),
                    )
                } else {
                    let repo_root = git::repo_root()?;
                    let cwd =
                        std::env::current_dir().context("failed to read current directory")?;
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
                    if let Selection::Only {
                        manifest_story_ids: manifest,
                        ..
                    } = &selection
                    {
                        manifest_story_ids = Some(manifest.clone());
                    }
                    PlanDocument::from_selection(coords, selection)
                }
            }
        },
    };

    warn_notes(&document.notes);
    if let Some(reason) = &document.reason {
        tracing::warn!("{reason}; capturing all stories");
    } else if let Some(ids) = &document.story_ids {
        tracing::info!(count = ids.len(), "planned story set");
    }

    // `plan = "only"` かつサーバー経路（ビルドを作った）なら、CI が撮影を始める前に
    // 選択計画をビルドへ固定する。finalize と比較はこの保存値と実アップロードを
    // 突き合わせるので、撮影が全滅しても「空の申告 == 空のアップロード」の
    // 循環一致で通り抜けることはない。固定に失敗したら計画は出力しない
    // （束縛の無い部分撮影を CI に始めさせない）。
    if let (Some(build_id), Some(story_ids), Some(manifest), Some(baseline)) = (
        &document.build_id,
        &document.story_ids,
        &manifest_story_ids,
        &document.baseline_commit_sha,
    ) {
        let client = client
            .as_ref()
            .expect("a server-created build implies an API client");
        client
            .attach_plan(build_id, story_ids, manifest, baseline)
            .await
            .context("failed to pin the capture plan to the build")?;
        tracing::info!(
            build_id = %build_id,
            selected = story_ids.len(),
            manifest = manifest.len(),
            "capture plan pinned to the build"
        );
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

    /// graph fixture（stats / index）を temp dir へ複製し、`commit` で stamp した
    /// 成果物ディレクトリを作る。fixture はリポジトリ内の読み取り専用ファイルで、
    /// provenance はテストごとの commit に依存するため、複製してから stamp する。
    fn stamped_graph_artifact(commit: &str) -> tempfile::TempDir {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph");
        let tmp = tempfile::TempDir::new().expect("artifact tempdir");
        for name in ["preview-stats.json", "index.json"] {
            std::fs::copy(fixture.join(name), tmp.path().join(name)).expect("copy fixture");
        }
        vrt_cli::provenance::stamp(
            &ArtifactPaths {
                dir: tmp.path(),
                stats_json: None,
                index_json: None,
            },
            commit,
            &["storybook".to_string(), "build".to_string()],
        )
        .expect("stamp");
        tmp
    }

    /// graph fixture を複製し、v1（build 所有なしの旧形式）の provenance を
    /// 手書きした成果物ディレクトリを作る。旧 CLI で stamp されたキャッシュの再現。
    fn legacy_v1_graph_artifact(commit: &str) -> tempfile::TempDir {
        use sha2::{Digest, Sha256};

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph");
        let tmp = tempfile::TempDir::new().expect("artifact tempdir");
        let mut hashes = Vec::new();
        for name in ["preview-stats.json", "index.json"] {
            let bytes = std::fs::read(fixture.join(name)).expect("read fixture");
            std::fs::write(tmp.path().join(name), &bytes).expect("copy fixture");
            let mut hex = String::new();
            for byte in Sha256::digest(&bytes) {
                use std::fmt::Write;
                write!(hex, "{byte:02x}").expect("hex");
            }
            hashes.push(hex);
        }
        let v1 = serde_json::json!({
            "version": 1,
            "head_commit_sha": commit,
            "stats_sha256": hashes[0],
            "index_sha256": hashes[1],
        });
        std::fs::write(
            tmp.path().join(vrt_cli::provenance::PROVENANCE_FILE),
            serde_json::to_string_pretty(&v1).expect("serialize"),
        )
        .expect("write v1 provenance");
        tmp
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
        let artifact = stamped_graph_artifact(&c2);

        // worktree は c3 のまま --commit c2 → 前提不一致でエラー。
        let err = resolve_only_story_ids(artifact.path(), None, &build, &c2)
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
        let artifact = stamped_graph_artifact(&c2);

        let only = resolve_only_story_ids(artifact.path(), None, &build, &c2).expect("resolve");
        let ids = only.expect("expected a narrowed story set");
        assert_eq!(
            ids,
            vec!["a--one".to_string(), "a--two".to_string()],
            "diff end is --commit (c2: A.tsx only), so only A stories are selected"
        );

        let _keep_tmp = tmp;
    }

    /// 別コミットで生成（stamp）された成果物では絞り込まない。worktree は
    /// `--commit` と完全に一致している（HEAD 一致・clean）にもかかわらず、
    /// untracked の成果物が別コミット由来なら拒否される。
    /// positive control: HEAD/clean 検査しか無い修正前の実装ではここが
    /// `Ok(Some([...]))` になり（tracked しか見ないため素通り）、このテストは落ちる。
    #[test]
    fn only_changed_rejects_an_artifact_stamped_at_another_commit() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, c3, frontend_cwd) = init_story_diff_repo();
        git_in(tmp.path(), &["checkout", "--detach", &c2]);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let build = pending_build(c1);
        // 成果物は c3 でビルドされた体（古いキャッシュを掴んだ状況の再現）。
        let artifact = stamped_graph_artifact(&c3);

        let err = resolve_only_story_ids(artifact.path(), None, &build, &c2)
            .expect_err("an artifact from another commit must not narrow the capture set");
        assert!(
            format!("{err:#}").contains("built from commit"),
            "err={err:#}"
        );

        let _keep_tmp = tmp;
    }

    /// v1（build 所有なしの旧形式）で stamp された成果物は、コミットも内容
    /// ハッシュも一致していて「正しく見える」にもかかわらず絞り込みに使わない。
    /// v1 は stamp 時点の HEAD しか証明せず、build と stamp の間の checkout
    /// （キャッシュ復元と同型の false green 経路）を検出できないためである。
    /// positive control: v1 を Verified として通す修正前の実装ではここが
    /// `Ok(Some([...]))` になり、このテストは落ちる。
    #[test]
    fn only_changed_does_not_narrow_on_a_legacy_v1_stamp() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        git_in(tmp.path(), &["checkout", "--detach", &c2]);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let build = pending_build(c1);
        let artifact = legacy_v1_graph_artifact(&c2);

        let only = resolve_only_story_ids(artifact.path(), None, &build, &c2).expect("resolve");
        assert!(
            only.is_none(),
            "a v1 stamp proves nothing about build time and must fall back to full capture"
        );

        let _keep_tmp = tmp;
    }

    /// commit A の成果物を保持したまま commit B で `vrt stamp -- true`
    /// （何も生成しない命令）を走らせても stamp されない。vrt が build 前に
    /// stats / index と旧 provenance を無効化し、no-op build では再生成
    /// されないためである。
    /// positive control: 無効化の無い修正前の実装では、build「成功」後も
    /// A の成果物がそのまま残っているため stamp が成功し（exit 0・B の v2
    /// provenance が A の成果物に付く）、このテストは落ちる。
    #[test]
    fn stamp_with_a_noop_build_does_not_bless_stale_artifacts() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        // 成果物は commit A(=c2) でビルド・stamp された体（古いキャッシュの再現）。
        let artifact = stamped_graph_artifact(&c2);
        // repo は commit B(=c3, HEAD) の clean な checkout。
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let err = run_stamp(StampArgs {
            dir: artifact.path().to_path_buf(),
            stats_json: None,
            index_json: None,
            build_command: vec!["true".to_string()],
        })
        .expect_err("a no-op build must not stamp artifacts it did not generate");
        assert!(format!("{err:#}").contains("--stats-json"), "err={err:#}");
        // 旧 provenance も残らない（build 前に無効化済み）。
        assert!(
            !artifact
                .path()
                .join(vrt_cli::provenance::PROVENANCE_FILE)
                .is_file(),
            "the stale provenance must have been invalidated"
        );

        let _keep_tmp = tmp;
    }

    /// build コマンドが実際に stats / index を生成すれば stamp は成立し、
    /// provenance は build 後の HEAD と再生成された内容に束縛される
    /// （無効化による過剰ブロックの回帰防止）。
    #[test]
    fn stamp_succeeds_when_the_build_regenerates_the_artifacts() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, c2, c3, frontend_cwd) = init_story_diff_repo();
        // 別コミット由来の古い成果物が転がっていても、build が再生成した
        // 内容だけが stamp される。
        let artifact = stamped_graph_artifact(&c2);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let script = format!(
            "printf '{{\"modules\":[]}}' > '{dir}/preview-stats.json' && \
             printf '{{\"v\":5,\"entries\":{{}}}}' > '{dir}/index.json'",
            dir = artifact.path().display()
        );
        run_stamp(StampArgs {
            dir: artifact.path().to_path_buf(),
            stats_json: None,
            index_json: None,
            build_command: vec!["sh".to_string(), "-c".to_string(), script],
        })
        .expect("a build that regenerates the artifacts must stamp");

        // 新しい provenance は HEAD(c3) と再生成された内容に一致する。
        let verification = vrt_cli::provenance::verify(
            &ArtifactPaths {
                dir: artifact.path(),
                stats_json: None,
                index_json: None,
            },
            &c3,
        )
        .expect("verify the fresh stamp");
        assert_eq!(verification, Verification::Verified);

        let _keep_tmp = tmp;
    }

    /// build が失敗した stamp は旧 provenance を残さない。失敗後に別コミットの
    /// 証明が生き残ると、plan / upload がその旧成果物で絞り込めてしまうためである。
    #[test]
    fn a_failed_build_leaves_no_stale_provenance() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, _c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        let artifact = stamped_graph_artifact(&c2);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let err = run_stamp(StampArgs {
            dir: artifact.path().to_path_buf(),
            stats_json: None,
            index_json: None,
            build_command: vec!["false".to_string()],
        })
        .expect_err("a failing build must not stamp");
        assert!(format!("{err:#}").contains("failed"), "err={err:#}");
        assert!(
            !artifact
                .path()
                .join(vrt_cli::provenance::PROVENANCE_FILE)
                .is_file(),
            "a failed stamp must not leave the previous provenance behind"
        );

        let _keep_tmp = tmp;
    }

    /// provenance が無い成果物は絞り込まず全撮影へ倒す（stamp 未導入の移行経路）。
    #[test]
    fn only_changed_falls_back_to_full_capture_without_provenance() {
        let _lock = REPO_TEST_LOCK.lock().expect("repo test lock");
        let (tmp, c1, c2, _c3, frontend_cwd) = init_story_diff_repo();
        git_in(tmp.path(), &["checkout", "--detach", &c2]);
        let _guard = ChdirGuard::change_to(&frontend_cwd);

        let build = pending_build(c1);
        let graph_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/graph");

        let only = resolve_only_story_ids(&graph_fixture, None, &build, &c2).expect("resolve");
        assert!(
            only.is_none(),
            "without provenance the CLI must capture everything instead of narrowing"
        );

        let _keep_tmp = tmp;
    }
}
