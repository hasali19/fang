use anyhow::anyhow;
use hidapi::{HidApi, HidDevice};
use parking_lot::Mutex;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use zerocopy::{FromBytes, Immutable, IntoBytes};

const RAZER_VID: u16 = 0x1532;
const RAZER_MOUSE_DOCK_PRO_PID: u16 = 0x00a4;
const RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID: u16 = 0xcd;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let api = HidApi::new()?;

    let device_info = api
        .device_list()
        .find(|d| {
            d.vendor_id() == RAZER_VID
                && d.product_id() == RAZER_MOUSE_DOCK_PRO_PID
                && d.interface_number() == 0
        })
        .ok_or_else(|| anyhow!("Failed to find mouse"))?;

    let device = Arc::new(Mutex::new(device_info.open_device(&api)?));

    let mut dock = MouseDock::new(device.clone());

    let paired_devices = dock.get_paired_devices()?;
    for (status, pid) in &paired_devices {
        println!("---");
        println!("pid: 0x{pid:x}");
        println!("connected: {}", *status == 1);
    }

    if let Some((_, pid)) = paired_devices.first()
        && *pid == RAZER_BASILISK_V3_PRO_35K_WIRELESS_PID
    {
        let mut mouse = Mouse::new(device, 0xe0);

        let battery_level = mouse.get_battery_level()?;
        let charging_status = mouse.get_charging_status()?;
        println!(
            "battery: {:.00}%",
            (f64::from(battery_level) / 255.0) * 100.0
        );
        println!("charging: {}", charging_status == 1);
    }

    Ok(())
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

    pub fn get_paired_devices(&mut self) -> anyhow::Result<Vec<(u8, u16)>> {
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

        let mut devices = vec![];

        for i in 0..r.data[0] as usize {
            let status = r.data[i * 3 + 1];
            let pid = u16::from_be_bytes([r.data[i * 3 + 2], r.data[i * 3 + 3]]);
            if pid != 0xffff {
                devices.push((status, pid));
            }
        }

        Ok(devices)
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

    pub fn get_battery_level(&mut self) -> anyhow::Result<u8> {
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

        Ok(r.data[1])
    }

    pub fn get_charging_status(&mut self) -> anyhow::Result<u8> {
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

fn send_and_receive(device: &hidapi::HidDevice, report: &mut Report) -> anyhow::Result<Report> {
    report.checksum = report.as_bytes()[3..=88]
        .iter()
        .fold(0u8, |acc, &b| acc ^ b);

    tracing::debug!("write: {:?}", report.as_bytes());

    device.send_feature_report(report.as_bytes())?;

    thread::sleep(Duration::from_millis(30));

    let mut response = [0u8; 91];

    device.get_feature_report(&mut response)?;

    tracing::debug!("read: {:?}", response);

    let report = Report::read_from_bytes(&response).map_err(|e| anyhow!("{e:?}"))?;

    Ok(report)
}

fn device_request(
    device: &hidapi::HidDevice,
    transaction_id: u8,
    command_class: u8,
    command_id: u8,
    data_len: u8,
    data: &[u8],
) -> anyhow::Result<Report> {
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

    assert_eq!(res.status, 2);
    assert_eq!(res.transaction_id, req.transaction_id);
    assert_eq!(res.command_class, req.command_class);
    assert_eq!(res.command_id, req.command_id);

    Ok(res)
}
