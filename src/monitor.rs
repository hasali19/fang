use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use eyre::ensure;
use hidapi::HidApi;
use tokio_stream::StreamExt;
use tracing::{debug, error, info, trace, warn};
use zbus::object_server::InterfaceRef;
use zbus::zvariant::OwnedObjectPath;

use crate::dbus::{
    self, DeviceManagerService, DeviceType, LightingRegionInterface, MouseState,
    RazerDeviceService, RazerMouseService,
};
use crate::dev::DeviceFile;
use crate::driver::chroma;
use crate::driver::{AsyncHidDevice, Mouse, MouseDock};
use crate::manager::{Device, DeviceManager, DeviceObject};
use crate::udev::{self, DeviceAction, UsbMonitor};

const RAZER_VID: u16 = 0x1532;
const RAZER_MOUSE_DOCK_PRO_PID: u16 = 0x00a4;
const RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID: u16 = 0xcd;

const RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID: u8 = 0xe0;

pub async fn run_device_monitor(
    dbus: zbus::Connection,
    device_manager: Arc<DeviceManager>,
) -> eyre::Result<()> {
    let device_manager_interface = dbus.object_server().interface("/dev/hasali/Fang").await?;

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

        if let Err(error) = handle_udev_event(
            &hid,
            &device_manager,
            &device_manager_interface,
            &dbus,
            event,
        )
        .await
        {
            error!(?error, "Failed to handle udev event");
        }
    }

    Ok(())
}

/// Bundles the handles shared by every udev event handler so they don't have
/// to be threaded through each function individually.
struct EventContext<'a> {
    device_manager: &'a DeviceManager,
    device_manager_interface: &'a InterfaceRef<DeviceManagerService>,
    dbus: &'a zbus::Connection,
}

async fn handle_udev_event(
    hid: &HidApi,
    device_manager: &DeviceManager,
    device_manager_interface: &InterfaceRef<DeviceManagerService>,
    dbus: &zbus::Connection,
    event: udev::UsbDeviceEvent,
) -> eyre::Result<()> {
    let ctx = EventContext {
        device_manager,
        device_manager_interface,
        dbus,
    };

    match event.action {
        DeviceAction::Add => {
            if event.device.product_id == RAZER_MOUSE_DOCK_PRO_PID {
                handle_dock_connected(hid, &ctx, event.device).await?;
            }
        }
        DeviceAction::Remove => {
            handle_device_removed(&ctx, event.device).await?;
        }
    }

    Ok(())
}

async fn handle_dock_connected(
    hid: &HidApi,
    ctx: &EventContext<'_>,
    device: udev::UsbDeviceInfo,
) -> eyre::Result<()> {
    let usb_syspath = device.syspath.clone();

    info!(syspath = %usb_syspath.display(), "Device connected");

    let devnode = CString::new(device.hid_interfaces[&0].as_os_str().as_bytes())?;
    let hid_device = AsyncHidDevice::create(hid.open_path(&devnode)?);

    let dock = MouseDock::new(hid_device.clone());
    let lighting_regions = chroma::get_lighting_regions(&dock.clone().into_generic()).await?;

    let object_path =
        dbus::create_object_path(RAZER_VID, RAZER_MOUSE_DOCK_PRO_PID, &device.sysname)?;

    let mut objects = vec![];
    let mut lighting_region_paths = vec![];

    for lighting_region in lighting_regions {
        let object_path =
            OwnedObjectPath::try_from(format!("{object_path}/light{}", lighting_region.region_id))?;

        let region_device = dock.clone().into_generic();
        let brightness = chroma::get_brightness(&region_device, lighting_region.region_id).await?;

        let effects =
            chroma::get_available_effects(&region_device, lighting_region.region_id).await?;

        let effect = chroma::get_effect(&region_device, lighting_region.region_id).await?;

        ctx.dbus
            .object_server()
            .at(
                object_path.clone(),
                LightingRegionInterface {
                    device: region_device,
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
            interfaces: vec![dbus::interface_name::<LightingRegionInterface>()],
        });
    }

    let service = RazerDeviceService {
        name: "Mouse Dock Pro",
        device_type: DeviceType::Dock,
        lighting_regions: lighting_region_paths,
    };

    ctx.dbus
        .object_server()
        .at(object_path.clone(), service)
        .await?;

    info!(path = %object_path, "Registered device");

    objects.push(DeviceObject {
        path: object_path.clone(),
        interfaces: vec![dbus::interface_name::<RazerDeviceService>()],
    });

    ctx.device_manager.add_device(
        usb_syspath.clone(),
        Device {
            objects,
            tasks: vec![],
        },
    );

    ctx.device_manager_interface
        .get()
        .await
        .devices_changed(ctx.device_manager_interface.signal_emitter())
        .await?;

    let paired_device = dock.get_paired_device().await?;

    if let Some((status, pid)) = paired_device {
        handle_mouse_connected(
            ctx,
            &device.sysname,
            usb_syspath,
            hid_device,
            device.hid_interfaces[&1].clone(),
            status,
            pid,
        )
        .await?;
    }

    Ok(())
}

async fn handle_mouse_connected(
    ctx: &EventContext<'_>,
    sysname: &OsStr,
    usb_syspath: PathBuf,
    hid_device: AsyncHidDevice,
    reader_devnode: PathBuf,
    status: u8,
    pid: u16,
) -> eyre::Result<()> {
    info!(pid, connected = status == 1, "Discovered paired device");

    if pid != RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID {
        return Ok(());
    }

    let object_path = dbus::create_object_path(RAZER_VID, pid, sysname)?;

    let mouse = Mouse::new(hid_device, RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID);

    let mouse_state = if status == 1 {
        read_mouse_state(&mouse).await?
    } else {
        MouseState::default()
    };

    ctx.dbus
        .object_server()
        .at(
            object_path.clone(),
            RazerDeviceService {
                name: "Basilisk V3 Pro 35K",
                device_type: DeviceType::Mouse,
                // TODO
                lighting_regions: vec![],
            },
        )
        .await?;

    let service = RazerMouseService { state: mouse_state };

    ctx.dbus.object_server().at(&object_path, service).await?;

    info!(path = %object_path, "Registered device");

    let mouse_interface: InterfaceRef<RazerMouseService> =
        ctx.dbus.object_server().interface(&object_path).await?;

    let reader_task = tokio::spawn(run_device_reader(
        reader_devnode,
        mouse.clone(),
        mouse_interface.clone(),
    ));

    let poller_task = tokio::spawn(run_device_poller(mouse, mouse_interface));

    ctx.device_manager.add_device(
        usb_syspath,
        Device {
            objects: vec![DeviceObject {
                path: object_path.clone(),
                interfaces: vec![dbus::interface_name::<RazerMouseService>()],
            }],
            tasks: vec![reader_task.abort_handle(), poller_task.abort_handle()],
        },
    );

    ctx.device_manager_interface
        .get()
        .await
        .devices_changed(ctx.device_manager_interface.signal_emitter())
        .await?;

    Ok(())
}

async fn handle_device_removed(
    ctx: &EventContext<'_>,
    device: udev::UsbDeviceInfo,
) -> eyre::Result<()> {
    let devices = ctx.device_manager.remove_devices(&device.syspath);

    for removed_device in devices {
        info!(
            syspath = %device.syspath.display(),
            "Device disconnected"
        );

        let object_server = ctx.dbus.object_server();

        for object in removed_device.objects {
            for interface in object.interfaces {
                object_server.remove_named(&object.path, interface).await?;
            }

            info!(path = %object.path, "Unregistered device");

            ctx.device_manager_interface
                .get()
                .await
                .devices_changed(ctx.device_manager_interface.signal_emitter())
                .await?;
        }

        for task in removed_device.tasks {
            task.abort();
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
