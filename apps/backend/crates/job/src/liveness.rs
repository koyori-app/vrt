//! ワーカーのハートビート監視。
//!
//! apalis のワーカーは、ハートビートに失敗した時点で走行ループを抜ける。
//! そのため大半の停止は `JoinHandle` の完了として即座に分かる（一次検知）。
//! ここで見るのは、タスクが終了しないまま進まなくなる残りの経路である（二次検知）。
//!
//! 判定は**このプロセスが登録したワーカー ID** に対してだけ行う。
//! `apalis.workers` の行はワーカー ID ごとに INSERT され、削除する経路が無い。
//! ワーカー ID は起動のたびに新しくなるので、キュー名で引くと過去のプロセスが
//! 残した行が必ずヒットし、起動直後から常に「古い」と判定されてしまう。

use std::time::{Duration, Instant};

use apalis_postgres::PgPool;
use tokio::sync::watch;

/// ハートビートが途切れたと見なすまでの既定時間。
///
/// apalis のハートビート間隔は 30 秒なので、6 回分の欠落にあたる。
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(180);

/// 起動直後と再接続直後に判定を待つ既定時間。
///
/// ワーカー行はワーカーが最初のハートビートを書くまで存在しない。
/// DB が落ちていた場合も、戻ってから 1 回書かれるまでは古いままに見える。
pub const DEFAULT_GRACE: Duration = Duration::from_secs(120);

/// ハートビートを読む間隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// 監視対象の 1 本。
#[derive(Clone, Debug)]
pub struct WatchedWorker {
    pub queue: String,
    /// `apalis.workers.id`。プロセスごとに一意。
    pub worker_id: String,
}

/// 監視の設定。
#[derive(Clone, Debug)]
pub struct LivenessConfig {
    pub stale_after: Duration,
    pub grace: Duration,
    pub poll_interval: Duration,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            stale_after: DEFAULT_STALE_AFTER,
            grace: DEFAULT_GRACE,
            poll_interval: POLL_INTERVAL,
        }
    }
}

impl LivenessConfig {
    /// 環境変数 `WORKER_HEARTBEAT_STALE_SECS` で閾値を上書きする（不正値は既定）。
    pub fn from_env() -> Self {
        let stale_after = std::env::var("WORKER_HEARTBEAT_STALE_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_STALE_AFTER);
        Self {
            stale_after,
            ..Self::default()
        }
    }
}

/// 1 回分の観測結果。行が無い場合は `None`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation<'a> {
    pub worker_id: &'a str,
    pub age: Option<Duration>,
}

/// 観測から異常を選び出す純関数。
///
/// 猶予中は何も返さない。行が無い状態も、猶予中なら「まだ書かれていない」
/// として扱う。猶予を過ぎても行が現れないのは、行が現れないままタスクだけが
/// 生き続ける未知の止まり方への保険として異常にする。
pub fn stale_workers<'a>(
    observations: &[Observation<'a>],
    stale_after: Duration,
    in_grace: bool,
) -> Vec<String> {
    if in_grace {
        return Vec::new();
    }
    observations
        .iter()
        .filter_map(|observation| match observation.age {
            None => Some(format!(
                "{}: no heartbeat row (worker never registered)",
                observation.worker_id
            )),
            Some(age) if age > stale_after => Some(format!(
                "{}: heartbeat is {}s old",
                observation.worker_id,
                age.as_secs()
            )),
            Some(_) => None,
        })
        .collect()
}

/// 観測がすべて新しいか（猶予を早く終わらせてよいか）。
pub fn all_fresh(observations: &[Observation<'_>], stale_after: Duration) -> bool {
    !observations.is_empty()
        && observations
            .iter()
            .all(|o| o.age.is_some_and(|age| age <= stale_after))
}

/// 自分のワーカーのハートビートの古さを DB から読む。
///
/// 古さは DB の `NOW()` との差で測る。アプリ側の時刻と比べると、コンテナと
/// DB の時計のずれがそのまま誤検知になる。
pub async fn observe(
    pool: &PgPool,
    workers: &[WatchedWorker],
) -> Result<Vec<(String, Option<Duration>)>, sqlx::Error> {
    let ids: Vec<String> = workers.iter().map(|w| w.worker_id.clone()).collect();
    let rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT id, EXTRACT(EPOCH FROM (NOW() - last_seen))::float8 \
         FROM apalis.workers WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;

    Ok(workers
        .iter()
        .map(|worker| {
            let age = rows
                .iter()
                .find(|(id, _)| id == &worker.worker_id)
                // 時計の逆転で負になった場合は 0 に丸める。
                .map(|(_, secs)| Duration::from_secs_f64(secs.max(0.0)));
            (worker.worker_id.clone(), age)
        })
        .collect())
}

/// ハートビート監視を回す。
///
/// 異常を見つけたら `Err` を返す。呼び出し元はプロセスを非ゼロ終了させ、
/// 復帰は restart policy に委ねる。停止要求中は何も異常と見なさない。
pub async fn watch_heartbeats(
    pool: PgPool,
    workers: Vec<WatchedWorker>,
    config: LivenessConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    if workers.is_empty() {
        return Ok(());
    }

    let mut grace_until = Instant::now() + config.grace;

    loop {
        tokio::select! {
            () = tokio::time::sleep(config.poll_interval) => {}
            result = shutdown.changed() => {
                // 停止要求が来たら監視を終える。送信側が落ちた場合も同じ。
                if result.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
        }

        if *shutdown.borrow() {
            return Ok(());
        }

        let observed = match observe(&pool, &workers).await {
            Ok(observed) => observed,
            Err(error) => {
                // DB へ届かない間は判定できない。古いとは見なさず、戻ってから
                // 最初のハートビートが書かれるまで待つ。
                tracing::warn!(%error, "worker liveness: could not read heartbeats");
                grace_until = Instant::now() + config.grace;
                continue;
            }
        };

        let observations: Vec<Observation<'_>> = observed
            .iter()
            .map(|(id, age)| Observation {
                worker_id: id,
                age: *age,
            })
            .collect();

        let in_grace = Instant::now() < grace_until;
        if in_grace {
            if all_fresh(&observations, config.stale_after) {
                grace_until = Instant::now();
            }
            continue;
        }

        let stale = stale_workers(&observations, config.stale_after, false);
        if !stale.is_empty() {
            return Err(format!(
                "worker heartbeats went stale: {}",
                stale.join(", ")
            ));
        }
    }
}

/// 監視向けに外へ出すキュー 1 本の状態。
#[derive(Clone, Debug, PartialEq)]
pub struct QueueHealth {
    pub queue: String,
    /// 直近 [`LivenessConfig::stale_after`] 以内にハートビートを打ったワーカー数。
    ///
    /// `apalis.workers` の行は消えないので、単純な行数はいつまでも増える。
    /// 「いま動いている数」を出すにはこの絞り込みが要る。
    pub live_workers: i64,
    /// 最新ハートビートの古さ。行が 1 つも無ければ `None`。
    pub newest_heartbeat_age: Option<Duration>,
    /// 一度も取得されていない待ちジョブ数（`attempts = 0` の Pending）。
    pub waiting_jobs: i64,
    /// その最古の待ち時間。待ちが無ければ `None`。
    pub oldest_wait: Option<Duration>,
}

/// キューごとの状態を読む。
///
/// 詰まりの検知はこの値を見る人と外形監視に任せる。滞留は「ワーカーが詰まって
/// いる」場合と「単に混んでいる」場合を区別できず、混雑を障害と誤認して
/// 再起動すると、孤児の再投入で `attempts` を消費して事態を悪化させる。
pub async fn queue_health(
    pool: &PgPool,
    stale_after: Duration,
) -> Result<Vec<QueueHealth>, sqlx::Error> {
    let stale_secs = stale_after.as_secs_f64();

    let workers: Vec<(String, i64, Option<f64>)> = sqlx::query_as(
        "SELECT worker_type, \
                count(*) FILTER (WHERE EXTRACT(EPOCH FROM (NOW() - last_seen)) <= $1), \
                EXTRACT(EPOCH FROM (NOW() - max(last_seen)))::float8 \
         FROM apalis.workers GROUP BY worker_type",
    )
    .bind(stale_secs)
    .fetch_all(pool)
    .await?;

    let jobs: Vec<(String, i64, Option<f64>)> = sqlx::query_as(
        "SELECT job_type, count(*), \
                EXTRACT(EPOCH FROM (NOW() - min(run_at)))::float8 \
         FROM apalis.jobs WHERE status = 'Pending' AND attempts = 0 \
         GROUP BY job_type",
    )
    .fetch_all(pool)
    .await?;

    let mut queues: Vec<String> = workers.iter().map(|(queue, _, _)| queue.clone()).collect();
    for (queue, _, _) in &jobs {
        if !queues.contains(queue) {
            queues.push(queue.clone());
        }
    }
    queues.sort();

    Ok(queues
        .into_iter()
        .map(|queue| {
            let worker = workers.iter().find(|(name, _, _)| name == &queue);
            let job = jobs.iter().find(|(name, _, _)| name == &queue);
            QueueHealth {
                live_workers: worker.map_or(0, |(_, count, _)| *count),
                newest_heartbeat_age: worker
                    .and_then(|(_, _, age)| *age)
                    .map(|secs| Duration::from_secs_f64(secs.max(0.0))),
                waiting_jobs: job.map_or(0, |(_, count, _)| *count),
                oldest_wait: job
                    .and_then(|(_, _, age)| *age)
                    .map(|secs| Duration::from_secs_f64(secs.max(0.0))),
                queue,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation<'a>(id: &'a str, age_secs: Option<u64>) -> Observation<'a> {
        Observation {
            worker_id: id,
            age: age_secs.map(Duration::from_secs),
        }
    }

    /// 猶予中は、古くても行が無くても異常にしない。
    /// 起動直後とDB復旧直後はどちらもこの形になる。
    #[test]
    fn grace_suppresses_every_finding() {
        let observations = [
            observation("compare-1", Some(9_999)),
            observation("status-1", None),
        ];
        assert!(stale_workers(&observations, DEFAULT_STALE_AFTER, true).is_empty());
    }

    /// 閾値の境界そのものは異常にしない。境界を明確に跨いだときだけ異常。
    #[test]
    fn staleness_is_decided_strictly_past_the_threshold() {
        let stale_after = Duration::from_secs(180);
        for age in [0, 30, 179, 180] {
            let observations = [observation("compare-1", Some(age))];
            assert!(
                stale_workers(&observations, stale_after, false).is_empty(),
                "{age}s must be treated as alive"
            );
        }
        for age in [181, 600, 86_400] {
            let observations = [observation("compare-1", Some(age))];
            assert_eq!(
                stale_workers(&observations, stale_after, false).len(),
                1,
                "{age}s must be treated as stale"
            );
        }
    }

    /// 3 本のうち 1 本でも古ければ異常。残り 2 本が健全でも見逃さない。
    #[test]
    fn one_stale_worker_among_healthy_ones_is_reported() {
        let observations = [
            observation("compare-1", Some(10)),
            observation("status-1", Some(600)),
            observation("webhook-1", Some(20)),
        ];
        let stale = stale_workers(&observations, DEFAULT_STALE_AFTER, false);
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("status-1"), "{stale:?}");
    }

    /// 猶予を過ぎても行が現れないのは異常として扱う。
    #[test]
    fn a_missing_row_past_the_grace_is_reported() {
        let observations = [observation("compare-1", None)];
        let stale = stale_workers(&observations, DEFAULT_STALE_AFTER, false);
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("never registered"), "{stale:?}");
    }

    /// 全部が新しいときだけ猶予を早く切り上げてよい。
    #[test]
    fn all_fresh_requires_every_worker_to_have_a_recent_row() {
        let stale_after = Duration::from_secs(180);
        assert!(all_fresh(
            &[observation("a", Some(10)), observation("b", Some(20))],
            stale_after
        ));
        assert!(!all_fresh(
            &[observation("a", Some(10)), observation("b", None)],
            stale_after
        ));
        assert!(!all_fresh(
            &[observation("a", Some(10)), observation("b", Some(600))],
            stale_after
        ));
        // 監視対象が無い状態を「全部新しい」と誤解しない。
        assert!(!all_fresh(&[], stale_after));
    }

    /// 閾値は環境変数で上書きできる。不正値は既定へ落とす。
    #[test]
    fn stale_threshold_comes_from_the_environment() {
        assert_eq!(LivenessConfig::default().stale_after, DEFAULT_STALE_AFTER);

        // SAFETY: テスト内でのみ環境変数を触る。
        unsafe {
            std::env::set_var("WORKER_HEARTBEAT_STALE_SECS", "45");
        }
        assert_eq!(
            LivenessConfig::from_env().stale_after,
            Duration::from_secs(45)
        );

        for invalid in ["0", "-1", "abc", ""] {
            unsafe {
                std::env::set_var("WORKER_HEARTBEAT_STALE_SECS", invalid);
            }
            assert_eq!(
                LivenessConfig::from_env().stale_after,
                DEFAULT_STALE_AFTER,
                "`{invalid}` must fall back to the default"
            );
        }
        unsafe {
            std::env::remove_var("WORKER_HEARTBEAT_STALE_SECS");
        }
    }
}
