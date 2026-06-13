use crate::driver::{RazerDevice, Request};

pub struct LightingRegion {
    pub region_id: u8,
    pub matrix_x: u8,
    pub matrix_y: u8,
}

#[tracing::instrument(skip(device))]
pub async fn get_lighting_regions(device: &RazerDevice) -> eyre::Result<Vec<LightingRegion>> {
    let r = device.request(Request::new(0x0f, 0x80 | 0x00, 80)).await?;

    let region_count = r.data_len / 5;
    let mut regions = Vec::with_capacity(region_count as usize);

    for i in 0..region_count as usize {
        let region_id = r.data[i * 5 + 0];
        // What are these?
        // let _ = r.data[i * 5 + 1];
        // let _ = r.data[i * 5 + 2];
        let matrix_x = r.data[i * 5 + 3];
        let matrix_y = r.data[i * 5 + 4];
        regions.push(LightingRegion {
            region_id,
            matrix_x,
            matrix_y,
        });
    }

    Ok(regions)
}

#[tracing::instrument(skip(device))]
pub async fn get_available_effects(device: &RazerDevice, region_id: u8) -> eyre::Result<Vec<u8>> {
    let res = device
        .request(Request::new(0x0f, 0x80 | 0x01, 80).with_data(&[region_id]))
        .await?;

    Ok(Vec::from(&res.data[1..res.data_len as usize]))
}

#[tracing::instrument(skip(device))]
pub async fn get_effect(device: &RazerDevice, region_id: u8) -> eyre::Result<u8> {
    let res = device
        .request(Request::new(0x0f, 0x80 | 0x02, 80).with_data(&[0x00, region_id]))
        .await?;

    Ok(res.data[2])
}

#[tracing::instrument(skip(device))]
pub async fn set_effect(device: &RazerDevice, region_id: u8, effect: u8) -> eyre::Result<u8> {
    let res = device
        .request(Request::new(0x0f, 0x00 | 0x02, 80).with_data(&[0x00, region_id, effect]))
        .await?;

    Ok(res.data[2])
}

#[tracing::instrument(skip(device))]
pub async fn get_brightness(device: &RazerDevice, region_id: u8) -> eyre::Result<u8> {
    let res = device
        .request(Request::new(0x0f, 0x80 | 0x04, 3).with_data(&[0x00, region_id]))
        .await?;

    Ok(res.data[2])
}

#[tracing::instrument(skip(device))]
pub async fn set_brightness(device: &RazerDevice, region_id: u8, value: u8) -> eyre::Result<()> {
    device
        .request(Request::new(0x0f, 0x00 | 0x04, 3).with_data(&[0x00, region_id, value]))
        .await?;

    Ok(())
}
