mod dbus;
mod dev;
mod driver;
mod manager;
mod monitor;
mod udev;

use std::env;
use std::sync::Arc;

use tracing::info;
use tracing_subscriber::EnvFilter;
use zbus::connection;

use crate::dbus::DeviceManagerService;
use crate::manager::DeviceManager;

#[tokio::main(flavor = "local")]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let use_session_bus = env::args().any(|arg| arg == "--session");

    let builder = if use_session_bus {
        connection::Builder::session()?
    } else {
        connection::Builder::system()?
    };

    let device_manager = Arc::new(DeviceManager::new());

    let dbus_name = "dev.hasali.Fang";
    let dbus = builder
        .name(dbus_name)?
        .serve_at(
            "/dev/hasali/Fang",
            DeviceManagerService {
                device_manager: device_manager.clone(),
            },
        )?
        .build()
        .await?;

    info!(
        "Listening on dbus {} bus at {dbus_name}",
        if use_session_bus { "session" } else { "system" }
    );

    tokio::task::spawn_local(monitor::run_device_monitor(dbus, device_manager));

    tokio::signal::ctrl_c().await?;

    Ok(())
}
