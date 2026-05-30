use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eyre::{ensure, eyre};
use hidapi::HidDevice;
use parking_lot::Mutex;
use tracing::trace;
use zerocopy::{FromBytes, Immutable, IntoBytes};

pub struct MouseDock {
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

pub struct Mouse {
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

    pub fn get_dpi(&mut self) -> eyre::Result<u16> {
        let r = device_request(
            &self.device.lock(),
            0xe0 | self.next_transaction_id,
            0x04,
            0x80 | 0x05,
            7,
            &[],
        )?;

        self.next_transaction_id += 1;
        if self.next_transaction_id > self.base_transaction_id + 30 {
            self.next_transaction_id = self.base_transaction_id;
        }

        Ok(u16::from_be_bytes([r.data[1], r.data[2]]))
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

    // TODO: Implement retry
    ensure!(res.status == 2);
    ensure!(res.transaction_id == req.transaction_id);
    ensure!(res.command_class == req.command_class);
    ensure!(res.command_id == req.command_id);

    Ok(res)
}
