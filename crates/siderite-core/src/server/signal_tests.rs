//! Actual SIGTERM delivery to an owned test subprocess.

use crate::{ApiError, App, ServerError, State, get};
use std::future::pending;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Files(PathBuf);

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct PendingMarker(PathBuf);

impl Drop for PendingMarker {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.join("dropped"), "dropped");
    }
}

async fn blocked(State(folder): State<PathBuf>) -> Result<(), ApiError> {
    let _marker = PendingMarker((*folder).clone());
    let marker = folder.join("started");
    std::fs::write(marker, "started").map_err(ApiError::internal)?;
    pending().await
}

#[test]
fn signal_child_process() -> TestResult {
    let Some(folder) = std::env::var_os("SIDERITE_SIGNAL_CHILD_DIR") else {
        // This entry point runs only in the subprocess created below.
        return Ok(());
    };
    let addr = std::env::var("SIDERITE_SIGNAL_CHILD_ADDR")?;
    let folder = PathBuf::from(folder);
    let cleanup = folder.clone();
    let app = App::new()
        .with_state(folder)
        .route("/", get(blocked))
        .shutdown_timeout(Duration::from_millis(100))
        .on_shutdown(move || async move {
            let marker = cleanup.join("dropped");
            let reading = std::fs::read_to_string(marker);
            let dropped = reading.map_err(ApiError::internal)?;
            let output = cleanup.join("cleaned");
            std::fs::write(output, dropped).map_err(ApiError::internal)?;
            Ok(())
        });
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(app.run(&addr));
    assert!(matches!(result, Err(ServerError::ShutdownTimeout)));
    Ok(())
}

async fn check_signal(signal: &str) -> TestResult {
    let id = uuid::Uuid::new_v4();
    let folder = std::env::temp_dir().join(format!("siderite-signal-{id}"));
    std::fs::create_dir(&folder)?;
    let files = Files(folder);
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = reservation.local_addr()?;
    drop(reservation);
    let child = Command::new(std::env::current_exe()?)
        .args(["--exact", "server::signal_tests::signal_child_process"])
        .env("SIDERITE_SIGNAL_CHILD_DIR", &files.0)
        .env("SIDERITE_SIGNAL_CHILD_ADDR", addr.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child = OwnedChild(child);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut socket = loop {
        match TcpStream::connect(addr).await {
            Ok(socket) => break socket,
            Err(_) if tokio::time::Instant::now() < deadline => {
                assert!(child.0.try_wait()?.is_none());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await?;
    while !files.0.join("started").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let delivered = Command::new("kill")
        .args([signal, &child.0.id().to_string()])
        .status()?;
    assert!(delivered.success());
    let mut buffer = Vec::new();
    let reading = socket.read_to_end(&mut buffer);
    tokio::time::timeout(Duration::from_secs(2), reading).await??;
    loop {
        if let Some(status) = child.0.try_wait()? {
            assert!(status.success());
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(std::fs::read_to_string(files.0.join("cleaned"))?, "dropped");
    assert!(TcpStream::connect(addr).await.is_err());
    Ok(())
}

#[tokio::test]
async fn sigterm_closes_pending_requests_before_teardown() -> TestResult {
    check_signal("-TERM").await
}

#[tokio::test]
async fn sigint_closes_pending_requests_before_teardown() -> TestResult {
    check_signal("-INT").await
}
