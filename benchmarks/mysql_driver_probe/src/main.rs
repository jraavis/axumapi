//! Diagnostic native-driver comparison, never a framework speedup claim.

mod driver;
mod fixture;
#[cfg(test)]
mod tests;
mod trial;

use driver::Kind;
use fixture::Fixture;
use serde::Serialize;
use std::env;

type ProbeError = Box<dyn std::error::Error + Send + Sync>;
type ProbeResult<T> = Result<T, ProbeError>;

struct Config {
    url: String,
    requests: usize,
    concurrency: usize,
    pool: usize,
    pairs: usize,
    timeout: u64,
    held: bool,
}

impl Config {
    fn read() -> ProbeResult<Self> {
        let mut config = Self {
            url: required_url()?,
            requests: 10_000,
            concurrency: 10,
            pool: 10,
            pairs: 5,
            timeout: 120,
            held: false,
        };
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--held" {
                config.held = true;
                continue;
            }
            let value = args.next().ok_or_else(|| invalid("missing value"))?;
            match arg.as_str() {
                "--requests" => config.requests = value.parse()?,
                "--concurrency" => config.concurrency = value.parse()?,
                "--pool" => config.pool = value.parse()?,
                "--pairs" => config.pairs = value.parse()?,
                "--timeout" => config.timeout = value.parse()?,
                _ => return Err(invalid("unknown probe option")),
            }
        }
        if config.requests < config.concurrency
            || config.concurrency == 0
            || config.pool == 0
            || config.pairs == 0
            || config.timeout == 0
            || config.requests > 1_000_000
            || config.concurrency > 256
            || config.pool > 256
            || config.pairs > 100
            || (config.held && config.concurrency > config.pool)
        {
            return Err(invalid(
                "invalid request/concurrency/pool/timeout bounds",
            ));
        }
        Ok(config)
    }
}

#[derive(Serialize)]
struct Manifest {
    scope: &'static str,
    sqlx: &'static str,
    mysql_async: &'static str,
    concurrency: usize,
    pool: usize,
    held_connections: bool,
    settings: std::collections::BTreeMap<String, String>,
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let config = Config::read()?;
    let mut fixture = Fixture::create(&config.url).await?;
    let result = compare(&config, &mut fixture).await;
    let cleanup = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fixture.destroy(),
    )
    .await;
    // Preserve the original failure; never emit credential-bearing URLs.
    if let Err(error) = result {
        if !matches!(cleanup, Ok(Ok(()))) {
            eprintln!("probe failed and disposable database cleanup failed");
        }
        return Err(error);
    }
    cleanup?
}

async fn compare(config: &Config, fixture: &mut Fixture) -> ProbeResult<()> {
    let manifest = Manifest {
        scope: "prepared driver-only independent autocommit inserts",
        sqlx: "0.8.6",
        mysql_async: "0.37.1",
        concurrency: config.concurrency,
        pool: config.pool,
        held_connections: config.held,
        settings: fixture.settings().await?,
    };
    println!("{}", serde_json::to_string(&manifest)?);
    let modes = [Kind::Sqlx, Kind::NativeReset, Kind::NativeRetain];
    for pair in 0..config.pairs {
        for position in 0..modes.len() {
            let index = if pair % 2 == 0 {
                position
            } else {
                modes.len() - position - 1
            };
            let result =
                trial::run(modes[index], pair, config, fixture).await?;
            println!("{}", serde_json::to_string(&result)?);
        }
    }
    Ok(())
}

fn invalid(message: &str) -> ProbeError {
    std::io::Error::other(message.to_owned()).into()
}

fn required_url() -> ProbeResult<String> {
    Ok(env::var("MYSQL_PROBE_URL").or_else(|_| env::var("MYSQL_URL"))?)
}
