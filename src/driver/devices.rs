use crate::driver::protocol::{AsyncHidDevice, Report, Request};

#[derive(Clone)]
pub struct RazerDevice {
    device: AsyncHidDevice,
    base_transaction_id: u8,
}

impl RazerDevice {
    pub async fn request(&self, request: Request) -> eyre::Result<Report> {
        self.device.request(request, self.base_transaction_id).await
    }
}

#[derive(Clone)]
pub struct MouseDock {
    device: RazerDevice,
}

impl MouseDock {
    pub fn new(device: AsyncHidDevice) -> MouseDock {
        MouseDock {
            device: RazerDevice {
                device,
                base_transaction_id: 0xe0,
            },
        }
    }

    pub fn into_generic(self) -> RazerDevice {
        self.device
    }

    pub async fn get_paired_device(&self) -> eyre::Result<Option<(u8, u16)>> {
        let r = self
            .device
            .request(Request::new(0x00, 0x80 | 0x3f, 80))
            .await?;

        let status = r.data[1];
        let pid = u16::from_be_bytes([r.data[2], r.data[3]]);
        if pid == 0xffff {
            return Ok(None);
        }

        Ok(Some((status, pid)))
    }
}

#[derive(Clone)]
pub struct Mouse {
    device: RazerDevice,
}

impl Mouse {
    pub fn new(device: AsyncHidDevice, base_transaction_id: u8) -> Mouse {
        Mouse {
            device: RazerDevice {
                device,
                base_transaction_id,
            },
        }
    }

    pub async fn get_battery_level(&self) -> eyre::Result<u8> {
        let r = self
            .device
            .request(Request::new(0x07, 0x80 | 0x00, 2))
            .await?;

        Ok((f64::from(r.data[1] as f64 / 255.0) * 100.0) as u8)
    }

    pub async fn get_charging_status(&self) -> eyre::Result<u8> {
        let r = self
            .device
            .request(Request::new(0x07, 0x80 | 0x04, 2))
            .await?;

        Ok(r.data[1])
    }

    pub async fn get_dpi(&self) -> eyre::Result<u16> {
        let r = self
            .device
            .request(Request::new(0x04, 0x80 | 0x05, 7))
            .await?;

        Ok(u16::from_be_bytes([r.data[1], r.data[2]]))
    }
}
