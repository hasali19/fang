use std::thread;
use std::time::Duration;

use eyre::{ensure, eyre};
use hidapi::HidDevice;
use tokio::sync::{mpsc, oneshot};
use tracing::{trace, warn};
use zerocopy::{FromBytes, Immutable, IntoBytes};

struct RequestTask {
    request: Request,
    base_transaction_id: u8,
    reply_sender: oneshot::Sender<eyre::Result<Report>>,
    span: tracing::Span,
}

#[derive(Clone)]
pub struct AsyncHidDevice {
    sender: mpsc::Sender<RequestTask>,
}

impl AsyncHidDevice {
    pub fn create(device: HidDevice) -> AsyncHidDevice {
        let (sender, mut receiver) = mpsc::channel::<RequestTask>(8);

        thread::spawn(move || {
            let mut transaction_id = 0;

            while let Some(task) = receiver.blocking_recv() {
                let _guard = task.span.enter();
                let _ = match Self::process_task(
                    &device,
                    task.request,
                    task.base_transaction_id | transaction_id,
                ) {
                    Ok(res) => task.reply_sender.send(Ok(res)),
                    Err(error) => task.reply_sender.send(Err(error)),
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
                transaction_id,
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

            trace!(?req_report, "write");

            device.send_feature_report(req_report.as_bytes())?;

            thread::sleep(Duration::from_millis(30));

            let mut response = [0u8; 91];

            device.get_feature_report(&mut response)?;

            let res_report = Report::read_from_bytes(&response).map_err(|e| eyre!("{e:?}"))?;

            trace!(?res_report, "read");

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

    pub async fn request(&self, request: Request, base_transaction_id: u8) -> eyre::Result<Report> {
        let (reply_sender, reply_receiver) = oneshot::channel();
        self.sender
            .send(RequestTask {
                request,
                base_transaction_id,
                reply_sender,
                span: tracing::Span::current(),
            })
            .await?;
        reply_receiver.await?
    }
}

#[derive(Debug, Immutable, IntoBytes, FromBytes)]
#[repr(C)]
pub struct Report {
    report_id: u8,
    status: u8,
    transaction_id: u8,
    _reserved1: [u8; 3],
    pub data_len: u8,
    command_class: u8,
    command_id: u8,
    pub data: [u8; 80],
    checksum: u8,
    _reserved2: u8,
}

pub struct Request {
    data_len: u8,
    command_class: u8,
    command_id: u8,
    data: [u8; 80],
}

impl Request {
    pub fn new(command_class: u8, command_id: u8, data_len: u8) -> Request {
        Request {
            data_len,
            command_class,
            command_id,
            data: [0; _],
        }
    }

    pub fn with_data(mut self, data: &[u8]) -> Request {
        self.data[..data.len()].copy_from_slice(data);
        self
    }
}
