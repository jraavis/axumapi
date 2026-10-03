//! Configuration failures and debug output never expose URL credentials.

use super::*;

#[test]
fn debug_omits_native_pool_credentials() {
    let options = OptsBuilder::default()
        .user(Some("private-user"))
        .pass(Some("private-password"));
    let backend = NativeMySqlBackend {
        pool: Pool::new(options),
        admission: Arc::new(Semaphore::new(1)),
        acquire_timeout: Duration::from_secs(1),
        max_connections: 1,
        tables: Arc::default(),
    };
    let debug = format!("{backend:?}");
    assert!(!debug.contains("private-user"));
    assert!(!debug.contains("private-password"));
    assert!(debug.contains("max_connections"));
}

#[tokio::test]
async fn invalid_limits_fail_before_network_access() {
    let cases = [
        NativeMySqlOptions {
            max_connections: 0,
            ..NativeMySqlOptions::default()
        },
        NativeMySqlOptions {
            max_connections: usize::MAX,
            max_waiters: 1,
            ..NativeMySqlOptions::default()
        },
        NativeMySqlOptions {
            acquire_timeout: Duration::ZERO,
            ..NativeMySqlOptions::default()
        },
    ];
    for options in cases {
        let result = NativeMySqlBackend::connect_with(
            "mysql://private-user:private-password@127.0.0.1:1/db",
            options,
        )
        .await;
        let Err(error) = result else {
            panic!("invalid limits were accepted");
        };
        let message = error.to_string();
        assert!(!message.contains("private-password"));
    }
}
