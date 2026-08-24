//! Proves a real Syncplay client can still drive the native server.
//!
//! Ignored by default: it needs a Syncplay client and a media player on PATH,
//! which are exactly the dependencies hosting no longer has. CI installs them
//! in one Linux job so the claim cannot rot silently, and no developer machine
//! has to carry them.
//!
//! This is the only test in the suite that talks to something we did not
//! write. Everything else pins our own idea of the protocol, which is worth
//! nothing if that idea is wrong.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use syncparty_lib::core::events::NullEventBus;
use syncparty_lib::core::syncplay::{
    MonitorConfig, NativeServer, RoomMonitor, ServerConfig, ServerController,
};

const PASSWORD: &str = "swordfish";
const ROOM: &str = "CI";
const NICKNAME: &str = "ci";

/// Long enough for a Python client to start an interpreter, connect and be
/// listed; short enough that a broken server fails the job rather than hanging
/// it.
const PATIENCE: Duration = Duration::from_secs(30);

#[tokio::test]
#[ignore = "requires a Syncplay client on PATH; run in the compatibility job"]
async fn a_real_syncplay_client_joins_and_appears_in_the_room() {
    let config = ServerConfig {
        port: free_port(),
        password: PASSWORD.to_owned(),
        salt: "PEPPER".to_owned(),
    };

    let server = NativeServer::new(Arc::new(NullEventBus));
    server
        .start(&config)
        .await
        .expect("the server should start");

    let mut client = spawn_client(config.port);

    // The same monitor the room panel uses, so what this asserts is what a
    // host would actually see.
    let monitor = RoomMonitor::start(MonitorConfig {
        host: "127.0.0.1".to_owned(),
        port: config.port,
        password: PASSWORD.to_owned(),
        room: ROOM.to_owned(),
    });

    let joined = wait_for(|| {
        monitor.snapshot().rooms.iter().any(|room| {
            room.name == ROOM && room.watchers.iter().any(|watcher| watcher.name == NICKNAME)
        })
    })
    .await;

    // Torn down before the assertion, or a failure leaves a Python process and
    // a listening port behind for the next run to trip over.
    let _ = client.kill();
    let _ = client.wait();
    server.stop().await.expect("the server should stop");

    assert!(
        joined,
        "a real Syncplay client did not appear in the room within {}s",
        PATIENCE.as_secs()
    );
}

fn spawn_client(port: u16) -> Child {
    Command::new("syncplay")
        .args([
            "--no-gui",
            // Leaves no configuration behind, so a developer running this by
            // hand does not have their own Syncplay settings rewritten.
            "--no-store",
            "-a",
            &format!("127.0.0.1:{port}"),
            "-n",
            NICKNAME,
            "-r",
            ROOM,
            "-p",
            PASSWORD,
            // Refused outright without one, even though nothing here plays a
            // file. The name has to be a player Syncplay recognises.
            "--player-path",
            "mpv",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("syncplay should be on PATH; this test is meant to be skipped without it")
}

async fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + PATIENCE;

    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    false
}

/// A port the kernel has just confirmed is free, so this can run beside
/// anything else.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("address")
        .port()
}
