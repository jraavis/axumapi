//! Matched worker concurrency, paired order and untimed validation.

use crate::driver::{Driver, Kind};
use crate::fixture::Fixture;
use crate::{Config, ProbeResult};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

#[derive(Serialize)]
pub(crate) struct Trial {
    pair: usize,
    kind: Kind,
    requests: usize,
    seconds: f64,
    successful_commits_per_second: f64,
    latency_us_p50: u128,
    latency_us_p90: u128,
    latency_us_p99: u128,
    verified_rows: usize,
    global_counter_deltas: BTreeMap<String, u64>,
}

pub(crate) async fn run(
    kind: Kind,
    pair: usize,
    config: &Config,
    fixture: &mut Fixture,
) -> ProbeResult<Trial> {
    let driver =
        Driver::open(kind, &config.url, &fixture.name, config.pool).await?;
    let result = tokio::time::timeout(
        timeout(config),
        measure(&driver, kind, pair, config, fixture),
    )
    .await;
    // Keep ownership outside the cancellable measurement so timed-out
    // workers release their leases before the pool finishes disconnecting.
    let closed =
        tokio::time::timeout(Duration::from_secs(30), driver.close()).await;
    let trial = result??;
    closed??;
    Ok(trial)
}

async fn measure(
    driver: &Driver,
    kind: Kind,
    pair: usize,
    config: &Config,
    fixture: &mut Fixture,
) -> ProbeResult<Trial> {
    driver.warm(config.pool).await?;
    fixture.reset().await?;
    let before = fixture.counters().await?;
    let barrier = Arc::new(Barrier::new(config.concurrency + 1));
    let mut workers = JoinSet::new();
    for index in 0..config.concurrency {
        let driver = driver.clone();
        let barrier = Arc::clone(&barrier);
        let count = config.requests / config.concurrency
            + usize::from(index < config.requests % config.concurrency);
        let mut lease = if config.held {
            Some(driver.acquire().await?)
        } else {
            None
        };
        workers.spawn(async move {
            let mut ids = Vec::with_capacity(count);
            let mut latencies = Vec::with_capacity(count);
            barrier.wait().await;
            barrier.wait().await;
            for _ in 0..count {
                let started = Instant::now();
                let id = match lease.as_mut() {
                    Some(conn) => conn.insert().await?,
                    None => driver.acquire().await?.insert().await?,
                };
                latencies.push(started.elapsed().as_micros());
                ids.push(id);
            }
            Ok::<_, crate::ProbeError>((ids, latencies))
        });
    }
    // Allocate and acquire held leases before starting the measured interval.
    barrier.wait().await;
    let mut ids = Vec::with_capacity(config.requests);
    let mut latencies = Vec::with_capacity(config.requests);
    let started = Instant::now();
    barrier.wait().await;
    while let Some(result) = workers.join_next().await {
        match result {
            Ok(Ok((worker_ids, worker_latencies))) => {
                ids.extend(worker_ids);
                latencies.extend(worker_latencies);
            }
            error => {
                workers.abort_all();
                while workers.join_next().await.is_some() {}
                match error {
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(error.into()),
                    Ok(Ok(_)) => unreachable!(),
                }
            }
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    // Count release/recycle work after the measured acknowledgements drain.
    driver.idle(config.pool).await;
    let after = fixture.counters().await?;
    fixture.verify(&ids).await?;
    latencies.sort_unstable();
    let percentile = |percent: usize| {
        let rank = (latencies.len() * percent).div_ceil(100);
        latencies[rank.saturating_sub(1)]
    };
    let deltas = after
        .into_iter()
        .map(|(key, value)| {
            let delta = value.saturating_sub(*before.get(&key).unwrap_or(&0));
            (key, delta)
        })
        .collect();
    Ok(Trial {
        pair,
        kind,
        requests: ids.len(),
        seconds,
        successful_commits_per_second: ids.len() as f64 / seconds,
        latency_us_p50: percentile(50),
        latency_us_p90: percentile(90),
        latency_us_p99: percentile(99),
        verified_rows: ids.len(),
        global_counter_deltas: deltas,
    })
}

pub(crate) fn timeout(config: &Config) -> Duration {
    Duration::from_secs(config.timeout)
}
