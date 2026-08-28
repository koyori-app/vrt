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

/// 監視対象をまとめて見張る。
pub struct TaskWatcher {
    rx: mpsc::UnboundedReceiver<(String, Result<(), String>)>,
}

impl TaskWatcher {
    pub fn new(tasks: Vec<SupervisedTask>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        for task in tasks {
            let tx = tx.clone();
            let label = task.label;
            let handle = task.handle;
            tokio::spawn(async move {
                let result = match handle.await {
                    Ok(result) => result,
                    Err(join_error) => Err(format!("task failed: {join_error}")),
                };
                let _ = tx.send((label, result));
            });
        }
        Self { rx }
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
            Some((label, Ok(()))) => format!("{label} stopped unexpectedly"),
            Some((label, Err(error))) => format!("{label} stopped: {error}"),
            None => std::future::pending().await,
        }
    }

    /// 残りの終了を待ってログに残す。停止処理の最後に呼ぶ。
    pub async fn drain(&mut self) {
        while let Some((label, result)) = self.rx.recv().await {
            match result {
                Ok(()) => info!("{label} stopped"),
                Err(error) => warn!("{label} stopped with an error: {error}"),
            }
        }
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
    let mut watcher = TaskWatcher::new(tasks);
    tokio::pin!(shutdown);

    tokio::select! {
        reason = watcher.first_exit() => {
            // 停止要求より先に止まった。残りも畳んでから落ちる。
            let _ = shutdown_tx.send(true);
            watcher.drain().await;
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
