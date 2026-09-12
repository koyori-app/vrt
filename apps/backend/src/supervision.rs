//! ワーカーを抱えるプロセス共通の監視。
//!
//! apalis のワーカーは、ハートビートやジョブ取得に失敗した時点で走行ループを
//! 抜ける。タスクが終わっただけではプロセスは生き続けるため、放っておくと
//! HTTP は正常なのにキューだけが永久に止まる（2026-08-27 の障害がこれ）。
//!
//! ここでは、ワーカーとハートビート監視の終了を停止要求と同時に見張り、
//! 停止要求より先に終わったものを異常として扱う。復帰の実体は Compose や
//! Dokploy の restart policy で、プロセスを非ゼロ終了させて発火させる。

use std::fmt::Display;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// 監視対象のタスク 1 本。
///
/// ワーカー本体（`WorkerError`）とハートビート監視（`String`）でエラー型が
/// 違うので、生成時に文字列へ寄せて同じ形で扱う。
pub struct SupervisedTask {
    label: String,
    handle: JoinHandle<Result<(), String>>,
}

impl SupervisedTask {
    pub fn new<E>(label: impl Into<String>, handle: JoinHandle<Result<(), E>>) -> Self
    where
        E: Display + Send + 'static,
    {
        // 監視タスク自身が panic した場合も JoinError として拾えるよう、
        // ここで包んでから扱う。
        let normalized = tokio::spawn(async move {
            match handle.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(error.to_string()),
                Err(join_error) => Err(format!("task failed: {join_error}")),
            }
        });
        Self {
            label: label.into(),
            handle: normalized,
        }
    }
}

/// 異常終了へ向かうときに、残りのタスクを待つ上限。
///
/// 1 本が落ちて残りが待機中という普通の形なら `drain` はほぼ即座に返るので、
/// この期限が効くのは **in-flight のジョブが戻ってこないとき**だけである。
/// apalis の停止は tracked task の完了を待つ設計で、停止タイムアウトを持たない
/// （`WorkerContext` の doc: “resolves once the worker is shut down and all tasks
/// have completed”）。hung なジョブが 1 本あると `drain` は永久に返らず、
/// `Err` に到達できずプロセスが生き残る——ワーカーのコンテナは healthcheck を
/// 持たず `restart: unless-stopped` だけなので、外から異常と判定して落とす経路も
/// 無い。つまり「1 本が死に、別の 1 本が hung」で、この仕組み自体が止まり、
/// **HTTP は正常なのにキューだけ死ぬ**という無くそうとした状態へ戻ってしまう。
///
/// 長く取るほど復帰が遅れ、短く取ると進行中のジョブを取り残す。取り残しは
/// `Running` のまま 300 秒後に孤児として再投入される（`attempts` を 1 消費する。
/// `docs/worker-supervision.md`「再起動の代償を認めておく」）ので失われはしない。
/// 復帰の速さを優先して 60 秒とする——Compose の `stop_grace_period`（worker は
/// 10 分）より十分短い。
///
/// 停止要求が先に来た**正常な停止には期限を掛けない**。在庫を捌く猶予は
/// `stop_grace_period` が持っており、そこを縮めるとデプロイのたびに
/// 進行中のジョブを捨てることになる。
pub const DRAIN_DEADLINE: Duration = Duration::from_secs(60);

/// 監視対象をまとめて見張る。
pub struct TaskWatcher {
    rx: mpsc::UnboundedReceiver<(String, Result<(), String>)>,
    /// まだ終了を報告していないタスク。期限切れの `drain_within` が
    /// 「何を待っていたか」を名指しするために持つ。
    pending: Vec<String>,
}

impl TaskWatcher {
    pub fn new(tasks: Vec<SupervisedTask>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut pending = Vec::with_capacity(tasks.len());
        for task in tasks {
            let tx = tx.clone();
            let label = task.label;
            pending.push(label.clone());
            let handle = task.handle;
            tokio::spawn(async move {
                let result = match handle.await {
                    Ok(result) => result,
                    Err(join_error) => Err(format!("task failed: {join_error}")),
                };
                let _ = tx.send((label, result));
            });
        }
        Self { rx, pending }
    }

    /// 報告のあったタスクを未報告一覧から外す。
    fn settle(&mut self, label: &str) {
        self.pending.retain(|pending| pending != label);
    }

    /// 最初に終わったタスクを待ち、その理由を返す。
    ///
    /// 停止要求の前に呼ばれる想定なので、正常終了も異常として説明する。
    ///
    /// 監視対象が無い（または全部の報告を受け取り終えた）場合は解決しない。
    /// ここで即座に値を返すと、`select!` が停止要求と同時にready になったときに
    /// 「タスクが落ちた」側を引く可能性があり、ワーカーを持たないプロセスの
    /// 正常停止がランダムに異常終了へ化ける。
    pub async fn first_exit(&mut self) -> String {
        match self.rx.recv().await {
            Some((label, Ok(()))) => {
                self.settle(&label);
                format!("{label} stopped unexpectedly")
            }
            Some((label, Err(error))) => {
                self.settle(&label);
                format!("{label} stopped: {error}")
            }
            None => std::future::pending().await,
        }
    }

    /// 残りの終了を待ってログに残す。停止処理の最後に呼ぶ。
    ///
    /// 待つ相手はワーカーの走行ループで、apalis は in-flight のジョブが終わる
    /// まで返さない。戻ってこないジョブがあると永久に返らないので、**異常終了
    /// へ向かう経路では [`TaskWatcher::drain_within`] を使うこと**。
    pub async fn drain(&mut self) {
        while let Some((label, result)) = self.rx.recv().await {
            self.settle(&label);
            match result {
                Ok(()) => info!("{label} stopped"),
                Err(error) => warn!("{label} stopped with an error: {error}"),
            }
        }
    }

    /// 残りの終了を `deadline` まで待ち、間に合わなければ諦める。
    ///
    /// 諦めた場合は待っていたタスクのラベルを返し、警告に残す。**戻り値が
    /// 空でなくても呼び出し側は終了処理を続けること**——待ち続けると、
    /// 復帰そのものが止まる（[`DRAIN_DEADLINE`] の理由）。
    pub async fn drain_within(&mut self, deadline: Duration) -> Vec<String> {
        if tokio::time::timeout(deadline, self.drain()).await.is_ok() {
            return Vec::new();
        }
        let unfinished = self.pending.clone();
        warn!(
            deadline_secs = deadline.as_secs(),
            tasks = %unfinished.join(", "),
            "gave up waiting for in-flight jobs; exiting anyway so the restart policy can take over"
        );
        unfinished
    }
}

/// HTTP を持たないプロセス（`vrt-runner` / `vrt-worker`）の実行ループ。
///
/// 停止要求より先にどれかのタスクが終わったら、`Err` を返してプロセスを
/// 非ゼロ終了させる。停止要求が先なら、在庫を捌き終えるまで待って `Ok`。
pub async fn run_until_shutdown<F>(
    tasks: Vec<SupervisedTask>,
    shutdown_tx: watch::Sender<bool>,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()>,
{
    run_until_shutdown_with_deadline(tasks, shutdown_tx, shutdown, DRAIN_DEADLINE).await
}

/// [`run_until_shutdown`] の本体。異常経路の待ち上限を差し替えられる。
///
/// 試験から短い期限を渡すためだけに分けてある——本番の値は
/// [`DRAIN_DEADLINE`] 1 箇所に固定したまま、「期限が効くこと」を秒単位で
/// 待たずに確かめられるようにする。
async fn run_until_shutdown_with_deadline<F>(
    tasks: Vec<SupervisedTask>,
    shutdown_tx: watch::Sender<bool>,
    shutdown: F,
    drain_deadline: Duration,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()>,
{
    let mut watcher = TaskWatcher::new(tasks);
    tokio::pin!(shutdown);

    tokio::select! {
        reason = watcher.first_exit() => {
            // 停止要求より先に止まった。残りも畳んでから落ちる。
            let _ = shutdown_tx.send(true);
            // 期限つきで待つ。戻ってこないジョブに合わせて待ち続けると、
            // 非ゼロ終了に到達できず restart policy が発火しない。
            watcher.drain_within(drain_deadline).await;
            Err(std::io::Error::other(reason))
        }
        () = &mut shutdown => {
            let _ = shutdown_tx.send(true);
            info!("shutting down; waiting for in-flight jobs");
            watcher.drain().await;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 3 本のうち 1 本が落ちただけでもプロセスを終わらせる。
    /// 残り 2 本が生きていても、そのキューは誰も消費しない。
    #[tokio::test]
    async fn one_dead_task_among_healthy_ones_stops_the_process() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let healthy = |mut rx: watch::Receiver<bool>| {
            tokio::spawn(async move {
                let _ = rx.changed().await;
                Ok::<(), std::io::Error>(())
            })
        };

        let tasks = vec![
            SupervisedTask::new("compare build worker", healthy(shutdown_rx.clone())),
            SupervisedTask::new(
                "github status worker",
                tokio::spawn(async { Err::<(), std::io::Error>(std::io::Error::other("boom")) }),
            ),
            SupervisedTask::new("github webhook worker", healthy(shutdown_rx.clone())),
        ];

        let error = run_until_shutdown(tasks, shutdown_tx, std::future::pending())
            .await
            .expect_err("a dead worker must stop the process");

        assert!(
            error.to_string().contains("github status worker"),
            "{error}"
        );
        assert!(error.to_string().contains("boom"), "{error}");
        // 残りにも停止が伝わっている。
        assert!(shutdown_rx.changed().await.is_ok());
        assert!(*shutdown_rx.borrow());
    }

    /// 停止要求のあとにワーカーが正常終了しても、終了コードは 0 のまま。
    /// ここを異常にすると、デプロイのたびに異常終了扱いになる。
    #[tokio::test]
    async fn a_worker_finishing_after_shutdown_is_not_a_failure() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let tasks = vec![SupervisedTask::new(
            "compare build worker",
            tokio::spawn({
                let mut rx = shutdown_rx.clone();
                async move {
                    let _ = rx.changed().await;
                    // 在庫を捌く時間を模す。
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok::<(), std::io::Error>(())
                }
            }),
        )];

        run_until_shutdown(tasks, shutdown_tx, std::future::ready(()))
            .await
            .expect("a graceful stop must exit zero");
    }

    /// 【復帰そのものが止まらないこと】1 本が落ちた時点で、別の 1 本が
    /// 戻ってこないジョブを抱えていても非ゼロ終了に到達する。
    ///
    /// apalis の停止は in-flight のジョブの完了を待ち、停止タイムアウトを
    /// 持たない。期限を切らずに `drain` すると `Err` へ到達できずプロセスが
    /// 生き残り、healthcheck の無いワーカーは誰にも落とされない——
    /// 「HTTP は正常なのにキューだけ死ぬ」に戻る。
    #[tokio::test]
    async fn a_hung_task_does_not_block_the_failure_exit() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let tasks = vec![
            SupervisedTask::new(
                "compare build worker",
                tokio::spawn(async { Err::<(), std::io::Error>(std::io::Error::other("boom")) }),
            ),
            // 停止要求を受け取っても戻ってこない = hung なジョブを抱えたワーカー。
            SupervisedTask::new(
                "github status worker",
                tokio::spawn(async {
                    std::future::pending::<()>().await;
                    Ok::<(), std::io::Error>(())
                }),
            ),
        ];

        let error = run_until_shutdown_with_deadline(
            tasks,
            shutdown_tx,
            std::future::pending(),
            Duration::from_millis(50),
        )
        .await
        .expect_err("a hung sibling must not keep the process alive");
        assert!(
            error.to_string().contains("compare build worker"),
            "{error}"
        );
    }

    /// 期限切れで諦めたときは、待っていた相手を名指しする。
    /// 名前が出ないと、次に見る人は何が hung したのか調べようがない。
    #[tokio::test]
    async fn a_timed_out_drain_names_what_it_gave_up_on() {
        let mut watcher = TaskWatcher::new(vec![
            SupervisedTask::new(
                "finished worker",
                tokio::spawn(async { Ok::<(), std::io::Error>(()) }),
            ),
            SupervisedTask::new(
                "hung worker",
                tokio::spawn(async {
                    std::future::pending::<()>().await;
                    Ok::<(), std::io::Error>(())
                }),
            ),
        ]);

        let unfinished = watcher.drain_within(Duration::from_millis(50)).await;
        assert_eq!(unfinished, vec!["hung worker".to_string()]);
    }

    /// 全部が期限内に終われば、諦めた相手は 0 件。
    #[tokio::test]
    async fn a_drain_that_completes_reports_nothing_unfinished() {
        let mut watcher = TaskWatcher::new(vec![SupervisedTask::new(
            "compare build worker",
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<(), std::io::Error>(())
            }),
        )]);

        assert!(
            watcher
                .drain_within(Duration::from_secs(5))
                .await
                .is_empty()
        );
    }

    /// 監視タスク自身が panic しても、監視が無言で消えず同じ終了経路に入る。
    #[tokio::test]
    async fn a_panicking_task_is_reported_instead_of_vanishing() {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let tasks = vec![SupervisedTask::new(
            "worker heartbeat monitor",
            tokio::spawn(async {
                panic!("monitor died");
                #[allow(unreachable_code)]
                Ok::<(), std::io::Error>(())
            }),
        )];

        let error = run_until_shutdown(tasks, shutdown_tx, std::future::pending())
            .await
            .expect_err("a panicking monitor must stop the process");

        assert!(
            error.to_string().contains("worker heartbeat monitor"),
            "{error}"
        );
    }

    /// ワーカーを持たないプロセス（API で JOB_WORKERS_ENABLED=false）は、
    /// 監視対象が空でも停止要求で普通に終わる。
    ///
    /// 停止要求が即座に ready な場合、`select!` は ready な分岐から無作為に選ぶ。
    /// 監視側が「対象なし」を値で返していると、その半分で正常停止が異常終了に
    /// 化ける（実際に CI で再現した）。取り違えを一度の実行で捕まえるため繰り返す。
    #[tokio::test]
    async fn no_tasks_still_shuts_down_cleanly() {
        for attempt in 0..32 {
            let (shutdown_tx, _shutdown_rx) = watch::channel(false);
            run_until_shutdown(Vec::new(), shutdown_tx, std::future::ready(()))
                .await
                .unwrap_or_else(|error| {
                    panic!("an empty supervisor must exit zero (attempt {attempt}): {error}")
                });
        }
    }
}
