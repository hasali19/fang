mod dev;
mod driver;
mod udev;

use std::collections::BTreeMap;
use std::env;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::Duration;

use eyre::ensure;
use hidapi::HidApi;
use tokio_stream::StreamExt;
use tracing::{error, info, trace, warn};
use tracing_subscriber::EnvFilter;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::OwnedObjectPath;
use zbus::{connection, interface};

use crate::dev::DeviceFile;
use crate::driver::{AsyncHidDevice, Mouse, MouseDock};
use crate::udev::{DeviceAction, UsbMonitor};

const RAZER_VID: u16 = 0x1532;
const RAZER_MOUSE_DOCK_PRO_PID: u16 = 0x00a4;
const RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID: u16 = 0xcd;

const RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID: u8 = 0xe0;

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

    let dbus_name = "dev.hasali.Fang";
    let dbus = builder
        .name(dbus_name)?
        .serve_at("/dev/hasali/Fang", DeviceManagerService)?
        .build()
        .await?;

    info!(
        "Listening on dbus {} bus at {dbus_name}",
        if use_session_bus { "session" } else { "system" }
    );

    tokio::task::spawn_local(run_device_monitor(dbus));

    tokio::signal::ctrl_c().await?;

    Ok(())
}

async fn run_device_monitor(dbus: zbus::Connection) -> eyre::Result<()> {
    let mut device_manager = DeviceManager::new();

    let hid = HidApi::new()?;
    let monitor = UsbMonitor::new(RAZER_VID)?
        // The Mouse Dock Pro has two interfaces that we care about.
        // Interface 0 is the main one where we can write feature reports to
        // send commands to the device.
        // Interface 1 sends input reports where we get notifications for
        // connection status and other state changes.
        .with_product(RAZER_MOUSE_DOCK_PRO_PID, vec![0, 1]);

    let mut events = pin!(monitor.events());
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                error!(?error, "Failed to read udev event");
                continue;
            }
        };

        if let Err(error) = handle_udev_event(&hid, &mut device_manager, &dbus, event).await {
            error!(?error, "Failed to handle udev event");
        }
    }

    Ok(())
}

struct DeviceManager {
    devices: BTreeMap<PathBuf, Device>,
}

struct Device {
    registered_objects: Vec<OwnedObjectPath>,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl DeviceManager {
    fn new() -> DeviceManager {
        DeviceManager {
            devices: BTreeMap::default(),
        }
    }
}

async fn handle_udev_event(
    hid: &HidApi,
    device_manager: &mut DeviceManager,
    dbus: &zbus::Connection,
    event: udev::UsbDeviceEvent,
) -> eyre::Result<()> {
    match event.action {
        DeviceAction::Add => {
            if event.device.product_id != RAZER_MOUSE_DOCK_PRO_PID {
                return Ok(());
            }

            let usb_syspath = event.device.syspath;
            let hid_interfaces = event.device.hid_interfaces;

            info!(syspath = %usb_syspath.display(), "Device connected");

            let devnode = CString::new(hid_interfaces[&0].as_os_str().as_bytes())?;
            let hid_device = AsyncHidDevice::create(hid.open_path(&devnode)?);

            let dock = MouseDock::new(hid_device.clone());

            let paired_device = dock.get_paired_device().await?;

            let device = device_manager
                .devices
                .entry(usb_syspath.clone())
                .or_insert_with(|| Device {
                    registered_objects: vec![],
                    tasks: vec![],
                });

            if let Some((status, pid)) = &paired_device {
                info!(pid, connected = *status == 1, "Discovered paired device");

                if *pid != RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID {
                    return Ok(());
                }

                let sysname_hex = hex::encode(event.device.sysname.as_bytes());
                let object_name = format!("{RAZER_VID}_{pid}_{sysname_hex}");
                let object_path =
                    OwnedObjectPath::try_from(format!("/dev/hasali/Fang/{}", object_name))?;

                let mouse = Mouse::new(
                    hid_device.clone(),
                    RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID,
                );

                let battery_level = mouse.get_battery_level().await?;
                let is_charging = mouse.get_charging_status().await? == 1;
                let dpi = mouse.get_dpi().await?;

                let service = RazerMouseService {
                    state: MouseState {
                        is_connected: *status == 1,
                        battery_level,
                        is_charging,
                        dpi,
                    },
                };

                dbus.object_server().at(&object_path, service).await?;

                info!(path = %object_path, "Registered device");

                let mouse_interface: InterfaceRef<RazerMouseService> =
                    dbus.object_server().interface(&object_path).await?;

                let reader_task = tokio::spawn(run_device_reader(
                    hid_interfaces[&1].clone(),
                    mouse_interface.clone(),
                ));

                let poller_task = tokio::spawn(run_device_poller(mouse, mouse_interface));

                device.registered_objects.push(object_path.clone());

                device.tasks.push(reader_task.abort_handle());
                device.tasks.push(poller_task.abort_handle());

                dbus.object_server()
                    .interface::<_, DeviceManagerService>("/dev/hasali/Fang")
                    .await?
                    .signal_emitter()
                    .device_added(object_path)
                    .await?;
            }
        }
        DeviceAction::Remove => {
            let Some(device) = device_manager.devices.remove(&event.device.syspath) else {
                return Ok(());
            };

            info!(
                syspath = %event.device.syspath.display(),
                "Device disconnected"
            );

            for path in device.registered_objects {
                dbus.object_server()
                    .remove::<RazerMouseService, _>(&path)
                    .await?;

                info!(path = %path, "Unregistered device");

                dbus.object_server()
                    .interface::<_, DeviceManagerService>("/dev/hasali/Fang")
                    .await?
                    .signal_emitter()
                    .device_removed(path)
                    .await?;
            }

            for task in device.tasks {
                task.abort();
            }
        }
    }

    Ok(())
}

async fn run_device_poller(mouse: Mouse, mouse_interface: InterfaceRef<RazerMouseService>) {
    loop {
        tokio::time::sleep(Duration::from_secs(150)).await;

        info!("Polling mouse state");

        if let Err(error) = poll_device_state(&mouse, mouse_interface.clone()).await {
            error!(?error, "Failed to poll mouse state");
        }
    }
}

async fn poll_device_state(
    mouse: &Mouse,
    mouse_interface: InterfaceRef<RazerMouseService>,
) -> eyre::Result<()> {
    let is_connected = mouse_interface.get().await.state.is_connected;
    if !is_connected {
        return Ok(());
    }

    let battery_level = mouse.get_battery_level().await?;
    let is_charging = mouse.get_charging_status().await? == 1;

    let mut mouse_service = mouse_interface.get_mut().await;

    if mouse_service.state.battery_level != battery_level {
        mouse_service.state.battery_level = battery_level;
        mouse_service
            .battery_level_changed(mouse_interface.signal_emitter())
            .await?;
    }

    if mouse_service.state.is_charging != is_charging {
        mouse_service.state.is_charging = is_charging;
        mouse_service
            .is_charging_changed(mouse_interface.signal_emitter())
            .await?;
    }

    Ok(())
}

async fn run_device_reader(devnode: PathBuf, mouse_interface: InterfaceRef<RazerMouseService>) {
    if let Err(error) = read_device_events(&devnode, mouse_interface).await {
        error!(?error, "Error in reader thread");
    }
}

async fn read_device_events(
    path: &Path,
    mouse_interface: InterfaceRef<RazerMouseService>,
) -> eyre::Result<()> {
    let file = DeviceFile::open(path)?;

    let mut buf = [0; 16];
    loop {
        let size = file.read(&mut buf).await?;

        ensure!(
            size == buf.len(),
            "Unexpected size for input report: {size}"
        );

        let mut mouse_service = mouse_interface.get_mut().await;

        if buf[0] == 5 && buf[1] == 2 {
            let dpi = u16::from_be_bytes([buf[2], buf[3]]);

            if mouse_service.state.dpi != dpi {
                mouse_service.state.dpi = dpi;
                mouse_service
                    .dpi_changed(mouse_interface.signal_emitter())
                    .await?;
            }
        } else if buf[0] == 5 && buf[1] == 9 {
            let is_connected = match buf[2] {
                2 => false,
                3 => true,
                v => {
                    warn!("Unrecognised connection state: {v}");
                    continue;
                }
            };

            if mouse_service.state.is_connected != is_connected {
                mouse_service.state.is_connected = is_connected;
                mouse_service
                    .is_connected_changed(mouse_interface.signal_emitter())
                    .await?;
            }
        } else if buf[0] == 5 && buf[1] == 49 {
            let battery_level = ((f64::from(buf[2]) / 255.0) * 100.0) as u8;
            let is_charging = buf[3] == 1;

            if mouse_service.state.battery_level != battery_level {
                mouse_service.state.battery_level = battery_level;
                mouse_service
                    .battery_level_changed(mouse_interface.signal_emitter())
                    .await?;
            }

            if mouse_service.state.is_charging != is_charging {
                mouse_service.state.is_charging = is_charging;
                mouse_service
                    .is_charging_changed(mouse_interface.signal_emitter())
                    .await?;
            }
        } else {
            trace!("Unrecognised event: {buf:?}");
        }
    }
}

struct DeviceManagerService;

#[interface(name = "dev.hasali.Fang.DeviceManager")]
impl DeviceManagerService {
    #[zbus(signal)]
    async fn device_added(emitter: &SignalEmitter<'_>, path: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn device_removed(emitter: &SignalEmitter<'_>, path: OwnedObjectPath)
    -> zbus::Result<()>;
}

struct RazerMouseService {
    state: MouseState,
}

struct MouseState {
    is_connected: bool,
    battery_level: u8,
    is_charging: bool,
    dpi: u16,
}

#[interface(name = "dev.hasali.Fang.Mouse")]
impl RazerMouseService {
    #[zbus(property(emits_changed_signal = "const"))]
    async fn name(&self) -> &'static str {
        "Basilisk V3 Pro 35K"
    }

    #[zbus(property)]
    async fn is_connected(&self) -> bool {
        self.state.is_connected
    }

    #[zbus(property(emits_changed_signal = "const"))]
    async fn has_battery(&self) -> bool {
        // TODO: Implement properly once we have support for wired mice
        true
    }

    #[zbus(property)]
    async fn battery_level(&self) -> u8 {
        self.state.battery_level
    }

    #[zbus(property)]
    async fn is_charging(&self) -> bool {
        self.state.is_charging
    }

    #[zbus(property)]
    async fn dpi(&self) -> u16 {
        self.state.dpi
    }
}
