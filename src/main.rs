mod dev;
mod udev;

use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{env, thread};

use eyre::{ensure, eyre};
use hidapi::{HidApi, HidDevice};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracing::{error, info, trace, warn};
use uuid::Uuid;
use zbus::zvariant::{OwnedObjectPath, Type};
use zbus::{connection, interface};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::dev::DeviceFile;
use crate::udev::{DeviceAction, DeviceMonitor};

const RAZER_VID: u16 = 0x1532;
const RAZER_MOUSE_DOCK_PRO_PID: u16 = 0x00a4;
const RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID: u16 = 0xcd;

const RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID: u8 = 0xe0;

#[tokio::main(flavor = "local")]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt::init();

    let use_session_bus = env::args().any(|arg| arg == "--session");

    let builder = if use_session_bus {
        connection::Builder::session()?
    } else {
        connection::Builder::system()?
    };

    let dbus_name = "dev.hasali.fang";
    let dbus = builder.name(dbus_name)?.build().await?;

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
    let monitor = DeviceMonitor::new(RAZER_VID)?;

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
    // Maps usb device syspath to list of known hid interfaces
    hid_interface_map: BTreeMap<PathBuf, BTreeMap<u8, PathBuf>>,
    // Maps a device syspath to one or more dbus object paths
    registered_objects: HashMap<PathBuf, Vec<OwnedObjectPath>>,
    // Maps a device syspath to the device state
    devices: HashMap<PathBuf, (Arc<AtomicBool>, tokio::task::AbortHandle)>,
}

impl DeviceManager {
    fn new() -> DeviceManager {
        DeviceManager {
            hid_interface_map: BTreeMap::default(),
            registered_objects: HashMap::default(),
            devices: HashMap::default(),
        }
    }
}

async fn handle_udev_event(
    hid: &HidApi,
    device_manager: &mut DeviceManager,
    dbus: &zbus::Connection,
    event: udev::DeviceEvent,
) -> eyre::Result<()> {
    match event.action {
        DeviceAction::Add => {
            if event.device.product_id != RAZER_MOUSE_DOCK_PRO_PID {
                return Ok(());
            }

            let usb_syspath = event.device.usb_device_syspath;
            let known_interfaces = device_manager
                .hid_interface_map
                .entry(usb_syspath.clone())
                .or_default();

            known_interfaces.insert(event.device.interface_number, event.device.devnode);

            // Keep waiting until all required usb interfaces are available
            if !known_interfaces.contains_key(&0) || !known_interfaces.contains_key(&1) {
                return Ok(());
            }

            // Skip if device has already been initialised
            if device_manager.devices.contains_key(&usb_syspath) {
                return Ok(());
            }

            info!(syspath = %usb_syspath.display(), "Device connected");

            // The Mouse Dock Pro has two interfaces that we care about.
            // Interface 0 is the main one where we can use feature reports to send commands to the device.
            // Interface 1 sends input reports where we get notifications for connection status and other state changes.

            let iface1_devnode = known_interfaces[&1].clone();
            let wireless_connection_state = Arc::new(AtomicBool::new(false));

            let reader_task = tokio::spawn({
                let wireless_connection_state = wireless_connection_state.clone();
                async move {
                    if let Err(error) =
                        read_device_events(&iface1_devnode, &wireless_connection_state).await
                    {
                        error!(?error, "Error in reader thread");
                    }
                }
            });

            let devnode = CString::new(known_interfaces[&0].as_os_str().as_bytes())?;
            let device = Arc::new(Mutex::new(hid.open_path(&devnode)?));

            let mut dock = MouseDock::new(device.clone());

            // FIXME: Don't do blocking io on this thread
            let paired_device = dock.get_paired_device()?;

            let object_paths = device_manager
                .registered_objects
                .entry(usb_syspath.clone())
                .or_default();

            device_manager.devices.insert(
                usb_syspath,
                (
                    wireless_connection_state.clone(),
                    reader_task.abort_handle(),
                ),
            );

            if let Some((status, pid)) = &paired_device {
                info!(pid, connected = *status == 1, "Discovered paired device");

                if *pid != RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID {
                    return Ok(());
                }

                wireless_connection_state.store(*status == 1, Ordering::Release);

                let uuid = Uuid::new_v4();
                let object_path =
                    OwnedObjectPath::try_from(format!("/dev/hasali/fang/{}", uuid.simple()))?;

                let service = RazerMouseService {
                    mouse: Mutex::new(Mouse::new(
                        device.clone(),
                        RAZER_BASILISK_V3_PRO_35K_WIRELESS_BASE_TXN_ID,
                    )),
                    is_connected: wireless_connection_state,
                };

                dbus.object_server().at(&object_path, service).await?;

                info!(path = %object_path, "Registered device");

                object_paths.push(object_path);
            }
        }
        DeviceAction::Remove => {
            let Some(paths) = device_manager
                .registered_objects
                .remove(&event.device.usb_device_syspath)
            else {
                return Ok(());
            };

            info!(
                syspath = %event.device.usb_device_syspath.display(),
                "Device disconnected"
            );

            for path in &paths {
                dbus.object_server()
                    .remove::<RazerMouseService, _>(path)
                    .await?;
                info!(path = %path, "Unregistered device")
            }

            if let Some((_, reader_task)) = device_manager
                .devices
                .remove(&event.device.usb_device_syspath)
            {
                reader_task.abort();
            }
        }
    }

    Ok(())
}

async fn read_device_events(path: &Path, state: &AtomicBool) -> eyre::Result<()> {
    let file = DeviceFile::open(path)?;

    let mut buf = [0; 16];
    loop {
        let size = file.read(&mut buf).await?;

        ensure!(size == buf.len());

        if buf[0] == 5 && buf[1] == 9 {
            let is_connected = match buf[2] {
                2 => false,
                3 => true,
                v => {
                    warn!("Unrecognised connection state: {v}");
                    continue;
                }
            };

            state.swap(is_connected, Ordering::AcqRel);
        } else {
            trace!("Unrecognised event: {buf:?}");
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Type)]
struct BatteryStatus {
    level: u8,
    charging: bool,
}

struct RazerMouseService {
    mouse: Mutex<Mouse>,
    is_connected: Arc<AtomicBool>,
}

#[interface(name = "dev.hasali.fang.mouse")]
impl RazerMouseService {
    #[zbus(property)]
    async fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Acquire)
    }

    async fn get_battery_status(&self) -> zbus::fdo::Result<BatteryStatus> {
        let mut mouse = self.mouse.lock();

        let level = mouse
            .get_battery_level()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        let charging = mouse
            .get_charging_status()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;

        Ok(BatteryStatus {
            level,
            charging: charging == 1,
        })
    }
}

struct MouseDock {
    device: Arc<Mutex<HidDevice>>,
    base_transaction_id: u8,
    next_transaction_id: u8,
}

impl MouseDock {
    pub fn new(device: Arc<Mutex<HidDevice>>) -> MouseDock {
        MouseDock {
            device,
            base_transaction_id: 0x00,
            next_transaction_id: 0x00,
        }
    }

    pub fn get_paired_device(&mut self) -> eyre::Result<Option<(u8, u16)>> {
        let r = device_request(
            &self.device.lock(),
            0xe0 | self.next_transaction_id,
            0x00,
            0x80 | 0x3f,
            80,
            &[],
        )?;

        self.next_transaction_id += 1;
        if self.next_transaction_id == self.base_transaction_id + 31 {
            self.next_transaction_id = self.base_transaction_id;
        }

        let status = r.data[1];
        let pid = u16::from_be_bytes([r.data[2], r.data[3]]);
        if pid == 0xffff {
            return Ok(None);
        }

        Ok(Some((status, pid)))
    }
}

struct Mouse {
    device: Arc<Mutex<HidDevice>>,
    base_transaction_id: u8,
    next_transaction_id: u8,
}

impl Mouse {
    pub fn new(device: Arc<Mutex<HidDevice>>, base_transaction_id: u8) -> Mouse {
        Mouse {
            device,
            base_transaction_id,
            next_transaction_id: base_transaction_id,
        }
    }

    pub fn get_battery_level(&mut self) -> eyre::Result<u8> {
        let r = device_request(
            &self.device.lock(),
            0xe0 | self.next_transaction_id,
            0x07,
            0x80 | 0x00,
            2,
            &[],
        )?;

        self.next_transaction_id += 1;
        if self.next_transaction_id > self.base_transaction_id + 30 {
            self.next_transaction_id = self.base_transaction_id;
        }

        Ok((f64::from(r.data[1] as f64 / 255.0) * 100.0) as u8)
    }

    pub fn get_charging_status(&mut self) -> eyre::Result<u8> {
        let r = device_request(
            &self.device.lock(),
            0xe0 | self.next_transaction_id,
            0x07,
            0x80 | 0x04,
            2,
            &[],
        )?;

        self.next_transaction_id += 1;
        if self.next_transaction_id > self.base_transaction_id + 30 {
            self.next_transaction_id = self.base_transaction_id;
        }

        Ok(r.data[1])
    }
}

#[derive(Immutable, IntoBytes, FromBytes)]
#[repr(C)]
struct Report {
    report_id: u8,
    status: u8,
    transaction_id: u8,
    _reserved1: [u8; 3],
    data_len: u8,
    command_class: u8,
    command_id: u8,
    data: [u8; 80],
    checksum: u8,
    _reserved2: u8,
}

fn send_and_receive(device: &hidapi::HidDevice, report: &mut Report) -> eyre::Result<Report> {
    report.checksum = report.as_bytes()[3..=88]
        .iter()
        .fold(0u8, |acc, &b| acc ^ b);

    trace!("write: {:?}", report.as_bytes());

    device.send_feature_report(report.as_bytes())?;

    thread::sleep(Duration::from_millis(30));

    let mut response = [0u8; 91];

    device.get_feature_report(&mut response)?;

    trace!("read: {:?}", response);

    let report = Report::read_from_bytes(&response).map_err(|e| eyre!("{e:?}"))?;

    Ok(report)
}

fn device_request(
    device: &hidapi::HidDevice,
    transaction_id: u8,
    command_class: u8,
    command_id: u8,
    data_len: u8,
    data: &[u8],
) -> eyre::Result<Report> {
    let mut req = Report {
        report_id: 0,
        status: 0,
        transaction_id,
        _reserved1: [0; _],
        data_len,
        command_class,
        command_id,
        data: [0; _],
        checksum: 0,
        _reserved2: 0,
    };

    req.data[..data.len()].copy_from_slice(data);

    let res = send_and_receive(device, &mut req)?;

    ensure!(res.status == 2);
    ensure!(res.transaction_id == req.transaction_id);
    ensure!(res.command_class == req.command_class);
    ensure!(res.command_id == req.command_id);

    Ok(res)
}
