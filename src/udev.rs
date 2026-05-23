use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::Poll;

use async_fn_stream::try_fn_stream;
use tokio::io::unix::AsyncFd;
use tokio_stream::{Stream, StreamExt};
use udev::MonitorSocket;

pub struct DeviceMonitor {
    vendor_id: u16,
    socket: AsyncMonitorSocket,
}

impl DeviceMonitor {
    pub fn new(vendor_id: u16) -> eyre::Result<DeviceMonitor> {
        Ok(DeviceMonitor {
            vendor_id,
            socket: AsyncMonitorSocket::new(
                udev::MonitorBuilder::new()?
                    .match_subsystem("hidraw")?
                    .listen()?,
            )?,
        })
    }

    pub fn events(mut self) -> impl Stream<Item = io::Result<DeviceEvent>> {
        try_fn_stream(|emitter| async move {
            let mut devices = HashMap::new();

            let mut enumerator = udev::Enumerator::new()?;
            enumerator.match_subsystem("hidraw")?;

            for device in enumerator.scan_devices()? {
                if let Some(device) = DeviceInfo::from_udev(&device, self.vendor_id) {
                    devices.insert(device.hidraw_syspath.clone(), device);
                }
            }

            for device in devices.values() {
                emitter
                    .emit(DeviceEvent {
                        action: DeviceAction::Add,
                        device: device.clone(),
                    })
                    .await;
            }

            while let Some(event) = self.socket.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        emitter.emit_err(e).await;
                        continue;
                    }
                };

                let event = if event.event_type() == udev::EventType::Add {
                    let Some(device) = DeviceInfo::from_udev(&event.device(), self.vendor_id)
                    else {
                        continue;
                    };
                    devices.insert(device.hidraw_syspath.clone(), device.clone());
                    DeviceEvent {
                        action: DeviceAction::Add,
                        device,
                    }
                } else if event.event_type() == udev::EventType::Remove {
                    let Some(device) = devices.remove(event.syspath()) else {
                        continue;
                    };
                    DeviceEvent {
                        action: DeviceAction::Remove,
                        device,
                    }
                } else {
                    continue;
                };

                emitter.emit(event).await;
            }

            Ok(())
        })
    }
}

pub enum DeviceAction {
    Add,
    Remove,
}

pub struct DeviceEvent {
    pub action: DeviceAction,
    pub device: DeviceInfo,
}

#[derive(Clone)]
pub struct DeviceInfo {
    pub hidraw_syspath: PathBuf,
    pub usb_device_syspath: PathBuf,
    pub devnode: PathBuf,
    pub product_id: u16,
    pub interface_number: u8,
}

impl DeviceInfo {
    fn from_udev(device: &udev::Device, vid_filter: u16) -> Option<DeviceInfo> {
        let devnode = device.devnode()?.to_path_buf();

        let usb_iface = device
            .parent_with_subsystem_devtype("usb", "usb_interface")
            .ok()??;

        let interface_number = usb_iface
            .attribute_value("bInterfaceNumber")
            .and_then(|v| v.to_str())
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .unwrap_or(0);

        let usb_device = device
            .parent_with_subsystem_devtype("usb", "usb_device")
            .ok()??;

        let (vid, pid, _product) = parse_attrs(&usb_device)?;

        if vid != vid_filter {
            return None;
        }

        Some(DeviceInfo {
            hidraw_syspath: device.syspath().to_path_buf(),
            usb_device_syspath: usb_device.syspath().to_path_buf(),
            devnode,
            product_id: pid,
            interface_number,
        })
    }
}

fn parse_attrs(usb_device: &udev::Device) -> Option<(u16, u16, Option<&str>)> {
    let product = usb_device
        .attribute_value("product")
        .and_then(|v| v.to_str());

    let vid = usb_device.attribute_value("idVendor")?.to_str()?;
    let pid = usb_device.attribute_value("idProduct")?.to_str()?;

    let vid = u16::from_str_radix(vid, 16).ok()?;
    let pid = u16::from_str_radix(pid, 16).ok()?;

    Some((vid, pid, product))
}

struct AsyncMonitorSocket {
    fd: AsyncFd<MonitorSocket>,
}

impl AsyncMonitorSocket {
    pub fn new(monitor: MonitorSocket) -> io::Result<AsyncMonitorSocket> {
        Ok(AsyncMonitorSocket {
            fd: AsyncFd::new(monitor)?,
        })
    }
}

impl Stream for AsyncMonitorSocket {
    type Item = io::Result<udev::Event>;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut std::task::Context) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(e) = self.fd.get_ref().iter().next() {
                return Poll::Ready(Some(Ok(e)));
            }
            match self.fd.poll_read_ready(ctx) {
                Poll::Ready(Ok(mut ready_guard)) => {
                    ready_guard.clear_ready();
                }
                Poll::Ready(Err(err)) => return Poll::Ready(Some(Err(err))),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
