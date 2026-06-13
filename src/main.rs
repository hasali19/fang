mod dev;
mod driver;
mod udev;

use std::collections::BTreeMap;
use std::env;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::Duration;

use eyre::ensure;
use hidapi::HidApi;
use tokio_stream::StreamExt;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::EnvFilter;
use zbus::names::InterfaceName;
use zbus::object_server::{Interface, InterfaceRef, SignalEmitter};
use zbus::zvariant::OwnedObjectPath;
use zbus::{connection, interface};

use crate::dev::DeviceFile;
use crate::driver::chroma::LightingRegion;
use crate::driver::{AsyncHidDevice, Mouse, MouseDock, RazerDevice};
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
    let device_manager_interface = dbus.object_server().interface("/dev/hasali/Fang").await?;
    let mut device_manager = DeviceManager::new(device_manager_interface);

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
    devices: BTreeMap<PathBuf, Vec<Device>>,
    interface: InterfaceRef<DeviceManagerService>,
}

struct Device {
    objects: Vec<DeviceObject>,
    tasks: Vec<tokio::task::AbortHandle>,
}

struct DeviceObject {
    path: OwnedObjectPath,
    interfaces: Vec<InterfaceName<'static>>,
}

impl DeviceManager {
    fn new(interface: InterfaceRef<DeviceManagerService>) -> DeviceManager {
        DeviceManager {
            devices: BTreeMap::default(),
            interface,
        }
    }

    fn add_device(&mut self, syspath: PathBuf, device: Device) {
        self.devices.entry(syspath).or_default().push(device);
    }

    fn remove_devices(&mut self, syspath: &Path) -> Vec<Device> {
        self.devices.remove(syspath).unwrap_or_default()
    }

    async fn notify_device_added(&self, object_path: OwnedObjectPath) -> eyre::Result<()> {
        self.interface
            .signal_emitter()
            .device_added(object_path)
            .await?;
        Ok(())
    }

    async fn notify_device_removed(&self, object_path: OwnedObjectPath) -> eyre::Result<()> {
        self.interface
            .signal_emitter()
            .device_removed(object_path)
            .await?;
        Ok(())
    }
}

fn create_object_path(
    vid: u16,
    pid: u16,
    sysname: &OsStr,
) -> zbus::zvariant::Result<OwnedObjectPath> {
    let sysname_hex = hex::encode(sysname.as_bytes());
    let object_path = format!("/dev/hasali/Fang/{vid}_{pid}_{sysname_hex}");
    OwnedObjectPath::try_from(object_path)
}

fn interface_name<I: Interface>() -> InterfaceName<'static> {
    <I as Interface>::name()
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
            let lighting_regions =
                driver::chroma::get_lighting_regions(&dock.clone().into_generic()).await?;

            let object_path =
                create_object_path(RAZER_VID, RAZER_MOUSE_DOCK_PRO_PID, &event.device.sysname)?;

            let mut objects = vec![];
            let mut lighting_region_paths = vec![];

            for lighting_region in lighting_regions {
                let object_path = OwnedObjectPath::try_from(format!(
                    "{object_path}/light{}",
                    lighting_region.region_id
                ))?;

                let device = dock.clone().into_generic();
                let brightness =
                    driver::chroma::get_brightness(&device, lighting_region.region_id).await?;

                let effects =
                    driver::chroma::get_available_effects(&device, lighting_region.region_id)
                        .await?;

                let effect = driver::chroma::get_effect(&device, lighting_region.region_id).await?;

                dbus.object_server()
                    .at(
                        object_path.clone(),
                        LightingRegionInterface {
                            device: dock.clone().into_generic(),
                            region: lighting_region,
                            brightness,
                            effects,
                            effect,
                        },
                    )
                    .await?;

                info!(path = %object_path, "Mounted object");

                lighting_region_paths.push(object_path.clone());

                objects.push(DeviceObject {
                    path: object_path,
                    interfaces: vec![interface_name::<LightingRegionInterface>()],
                });
            }

            let service = RazerDeviceService {
                name: "Mouse Dock Pro",
                lighting_regions: lighting_region_paths,
            };

            dbus.object_server()
                .at(object_path.clone(), service)
                .await?;

            info!(path = %object_path, "Registered device");

            objects.push(DeviceObject {
                path: object_path.clone(),
                interfaces: vec![interface_name::<RazerDeviceService>()],
            });

            device_manager.add_device(
                usb_syspath.clone(),
                Device {
                    objects,
                    tasks: vec![],
                },
            );

            device_manager.notify_device_added(object_path).await?;

            let paired_device = dock.get_paired_device().await?;

            if let Some((status, pid)) = &paired_device {
                info!(pid, connected = *status == 1, "Discovered paired device");

                if *pid != RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID {
                    return Ok(());
                }

                let object_path = create_object_path(RAZER_VID, *pid, &event.device.sysname)?;

                let mouse = Mouse::new(
                    hid_device.clone(),
                    RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID,
                );

                let mouse_state = if *status == 1 {
                    read_mouse_state(&mouse).await?
                } else {
                    MouseState::default()
                };

                let service = RazerMouseService { state: mouse_state };

                dbus.object_server().at(&object_path, service).await?;

                info!(path = %object_path, "Registered device");

                let mouse_interface: InterfaceRef<RazerMouseService> =
                    dbus.object_server().interface(&object_path).await?;

                let reader_task = tokio::spawn(run_device_reader(
                    hid_interfaces[&1].clone(),
                    mouse.clone(),
                    mouse_interface.clone(),
                ));

                let poller_task = tokio::spawn(run_device_poller(mouse, mouse_interface));

                device_manager.add_device(
                    usb_syspath.clone(),
                    Device {
                        objects: vec![DeviceObject {
                            path: object_path.clone(),
                            interfaces: vec![interface_name::<RazerMouseService>()],
                        }],
                        tasks: vec![reader_task.abort_handle(), poller_task.abort_handle()],
                    },
                );

                dbus.object_server()
                    .interface::<_, DeviceManagerService>("/dev/hasali/Fang")
                    .await?
                    .signal_emitter()
                    .device_added(object_path)
                    .await?;
            }
        }
        DeviceAction::Remove => {
            let devices = device_manager.remove_devices(&event.device.syspath);

            for device in devices {
                info!(
                    syspath = %event.device.syspath.display(),
                    "Device disconnected"
                );

                let object_server = dbus.object_server();

                for object in device.objects {
                    for interface in object.interfaces {
                        object_server.remove_named(&object.path, interface).await?;
                    }

                    info!(path = %object.path, "Unregistered device");

                    device_manager.notify_device_removed(object.path).await?;
                }

                for task in device.tasks {
                    task.abort();
                }
            }
        }
    }

    Ok(())
}

async fn run_device_poller(mouse: Mouse, mouse_interface: InterfaceRef<RazerMouseService>) {
    loop {
        tokio::time::sleep(Duration::from_secs(150)).await;

        debug!("Polling mouse state");

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

async fn run_device_reader(
    devnode: PathBuf,
    mouse: Mouse,
    mouse_interface: InterfaceRef<RazerMouseService>,
) {
    if let Err(error) = read_device_events(&devnode, &mouse, mouse_interface).await {
        error!(?error, "Error in reader thread");
    }
}

async fn read_device_events(
    path: &Path,
    mouse: &Mouse,
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

                debug!(is_connected, "Connection state changed");

                if is_connected {
                    mouse_service.state = read_mouse_state(mouse).await?;
                }

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

async fn read_mouse_state(mouse: &Mouse) -> eyre::Result<MouseState> {
    let battery_level = mouse.get_battery_level().await?;
    let is_charging = mouse.get_charging_status().await? == 1;
    let dpi = mouse.get_dpi().await?;

    Ok(MouseState {
        is_connected: true,
        battery_level,
        is_charging,
        dpi,
    })
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

struct RazerDeviceService {
    name: &'static str,
    lighting_regions: Vec<OwnedObjectPath>,
}

#[interface(name = "dev.hasali.Fang.Device")]
impl RazerDeviceService {
    #[zbus(property(emits_changed_signal = "const"))]
    async fn name(&self) -> &'static str {
        self.name
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn lighting_regions(&self) -> &[OwnedObjectPath] {
        &self.lighting_regions
    }
}

struct RazerMouseService {
    state: MouseState,
}

#[derive(Default)]
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

struct LightingRegionInterface {
    device: RazerDevice,
    region: LightingRegion,
    brightness: u8,
    effects: Vec<u8>,
    effect: u8,
}

#[interface(name = "dev.hasali.Fang.LightingRegion")]
impl LightingRegionInterface {
    #[zbus(property(emits_changed_signal = "const"))]
    pub fn region_id(&self) -> u8 {
        self.region.region_id
    }

    #[zbus(property(emits_changed_signal = "const"))]
    pub fn matrix_x(&self) -> u8 {
        self.region.matrix_x
    }

    #[zbus(property(emits_changed_signal = "const"))]
    pub fn matrix_y(&self) -> u8 {
        self.region.matrix_y
    }

    #[zbus(property(emits_changed_signal = "const"))]
    pub fn effects(&self) -> &[u8] {
        &self.effects
    }

    #[zbus(property)]
    pub fn brightness(&self) -> u8 {
        self.brightness
    }

    #[zbus(property)]
    pub async fn set_brightness(&mut self, value: u8) -> zbus::fdo::Result<()> {
        driver::chroma::set_brightness(&self.device, self.region.region_id, value)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        self.brightness = value;

        Ok(())
    }

    #[zbus(property)]
    pub fn effect(&self) -> u8 {
        self.effect
    }

    #[zbus(property)]
    pub async fn set_effect(&mut self, value: u8) -> zbus::fdo::Result<()> {
        driver::chroma::set_effect(&self.device, self.region.region_id, value)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        Ok(())
    }
}
