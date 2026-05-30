use std::thread;
use std::time::Duration;

use eyre::{ensure, eyre};
use hidapi::HidDevice;
use tokio::sync::{mpsc, oneshot};
use tracing::{trace, warn};
use zerocopy::{FromBytes, Immutable, IntoBytes};

pub struct MouseDock {
    device: AsyncHidDevice,
    base_transaction_id: u8,
}

impl MouseDock {
    pub fn new(device: AsyncHidDevice) -> MouseDock {
        MouseDock {
            device: device,
            base_transaction_id: 0xe0,
        }
    }

    pub async fn get_paired_device(&self) -> eyre::Result<Option<(u8, u16)>> {
        let r = self
            .device
            .request(Request::new(
                self.base_transaction_id,
                0x00,
                0x80 | 0x3f,
                80,
            ))
            .await?;

        let status = r.data[1];
        let pid = u16::from_be_bytes([r.data[2], r.data[3]]);
        if pid == 0xffff {
            return Ok(None);
        }

        Ok(Some((status, pid)))
    }
}

pub struct Mouse {
    device: AsyncHidDevice,
    base_transaction_id: u8,
}

impl Mouse {
    pub fn new(device: AsyncHidDevice, base_transaction_id: u8) -> Mouse {
        Mouse {
            device,
            base_transaction_id,
        }
    }

    pub async fn get_battery_level(&self) -> eyre::Result<u8> {
        let r = self
            .device
            .request(Request::new(self.base_transaction_id, 0x07, 0x80 | 0x00, 2))
            .await?;

        Ok((f64::from(r.data[1] as f64 / 255.0) * 100.0) as u8)
    }

    pub async fn get_charging_status(&self) -> eyre::Result<u8> {
        let r = self
            .device
            .request(Request::new(self.base_transaction_id, 0x07, 0x80 | 0x04, 2))
            .await?;

        Ok(r.data[1])
    }

    pub async fn get_dpi(&self) -> eyre::Result<u16> {
        let r = self
            .device
            .request(Request::new(self.base_transaction_id, 0x04, 0x80 | 0x05, 7))
            .await?;

        Ok(u16::from_be_bytes([r.data[1], r.data[2]]))
    }
}

#[derive(Clone)]
pub struct AsyncHidDevice {
    sender: mpsc::Sender<(Request, oneshot::Sender<eyre::Result<Report>>)>,
}

impl AsyncHidDevice {
    pub fn create(device: HidDevice) -> AsyncHidDevice {
        let (sender, mut receiver) =
            mpsc::channel::<(Request, oneshot::Sender<eyre::Result<Report>>)>(8);

        thread::spawn(move || {
            let mut transaction_id = 0;

            while let Some((task, reply_sender)) = receiver.blocking_recv() {
                let _ = match Self::process_task(&device, task, transaction_id) {
                    Ok(res) => reply_sender.send(Ok(res)),
                    Err(error) => reply_sender.send(Err(error)),
                };
                transaction_id = (transaction_id + 1) % 31;
            }
        });

        AsyncHidDevice { sender }
    }

    fn process_task(
        device: &hidapi::HidDevice,
        request: Request,
        transaction_id: u8,
    ) -> eyre::Result<Report> {
        loop {
            let mut req_report = Report {
                report_id: 0,
                status: 0,
                transaction_id: request.base_transaction_id | transaction_id,
                _reserved1: [0; _],
                data_len: request.data_len,
                command_class: request.command_class,
                command_id: request.command_id,
                data: [0; _],
                checksum: 0,
                _reserved2: 0,
            };

            req_report.data[..request.data.len()].copy_from_slice(&request.data);

            req_report.checksum = req_report.as_bytes()[3..=88]
                .iter()
                .fold(0u8, |acc, &b| acc ^ b);

            trace!("write: {:?}", req_report.as_bytes());

            device.send_feature_report(req_report.as_bytes())?;

            thread::sleep(Duration::from_millis(30));

            let mut response = [0u8; 91];

            device.get_feature_report(&mut response)?;

            trace!("read: {:?}", response);

            let res_report = Report::read_from_bytes(&response).map_err(|e| eyre!("{e:?}"))?;

            if res_report.status == 1 {
                warn!("Device is busy, retrying command");
                continue;
            }

            // TODO: Implement retry
            ensure!(
                res_report.status == 2,
                "Failed with status: {}",
                res_report.status
            );

            ensure!(res_report.transaction_id == req_report.transaction_id);
            ensure!(res_report.command_class == req_report.command_class);
            ensure!(res_report.command_id == req_report.command_id);

            return Ok(res_report);
        }
    }

    async fn request(&self, request: Request) -> eyre::Result<Report> {
        let (reply_sender, reply_receiver) = oneshot::channel();
        self.sender.send((request, reply_sender)).await?;
        reply_receiver.await?
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

struct Request {
    base_transaction_id: u8,
    data_len: u8,
    command_class: u8,
    command_id: u8,
    data: [u8; 80],
}

impl Request {
    fn new(base_transaction_id: u8, command_class: u8, command_id: u8, data_len: u8) -> Request {
        Request {
            base_transaction_id,
            data_len,
            command_class,
            command_id,
            data: [0; _],
        }
    }

    fn with_data(mut self, data: &[u8]) -> Request {
        self.data[..data.len()].copy_from_slice(data);
        self
    }
}
