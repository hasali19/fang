use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use zbus::interface;
use zbus::names::InterfaceName;
use zbus::object_server::Interface;
use zbus::zvariant::{OwnedObjectPath, Type, Value};

use crate::driver;
use crate::driver::RazerDevice;
use crate::driver::chroma::LightingRegion;
use crate::manager::DeviceManager;

pub fn interface_name<I: Interface>() -> InterfaceName<'static> {
    <I as Interface>::name()
}

pub fn create_object_path(
    vid: u16,
    pid: u16,
    sysname: &OsStr,
) -> zbus::zvariant::Result<OwnedObjectPath> {
    let sysname_hex = hex::encode(sysname.as_bytes());
    let object_path = format!("/dev/hasali/Fang/{vid}_{pid}_{sysname_hex}");
    OwnedObjectPath::try_from(object_path)
}

pub struct DeviceManagerService {
    pub device_manager: Arc<DeviceManager>,
}

#[interface(name = "dev.hasali.Fang.DeviceManager")]
impl DeviceManagerService {
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        self.device_manager
            .devices
            .lock()
            .values()
            .flatten()
            .flat_map(|device| {
                device.objects.iter().filter(|object| {
                    [
                        interface_name::<RazerDeviceService>(),
                        interface_name::<RazerMouseService>(),
                    ]
                    .iter()
                    .any(|interface| object.interfaces.contains(interface))
                })
            })
            .map(|object| object.path.clone())
            .collect()
    }
}

#[derive(Copy, Clone, Type, Value)]
pub enum DeviceType {
    Dock = 1,
    Mouse = 2,
}

pub struct RazerDeviceService {
    pub name: &'static str,
    pub device_type: DeviceType,
    pub lighting_regions: Vec<OwnedObjectPath>,
}

#[interface(name = "dev.hasali.Fang.Device")]
impl RazerDeviceService {
    #[zbus(property(emits_changed_signal = "const"))]
    fn name(&self) -> &'static str {
        self.name
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn device_type(&self) -> DeviceType {
        self.device_type
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn lighting_regions(&self) -> &[OwnedObjectPath] {
        &self.lighting_regions
    }
}

pub struct RazerMouseService {
    pub state: MouseState,
}

#[derive(Default)]
pub struct MouseState {
    pub is_connected: bool,
    pub battery_level: u8,
    pub is_charging: bool,
    pub dpi: u16,
}

#[interface(name = "dev.hasali.Fang.Mouse")]
impl RazerMouseService {
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

pub struct LightingRegionInterface {
    pub device: RazerDevice,
    pub region: LightingRegion,
    pub brightness: u8,
    pub effects: Vec<u8>,
    pub effect: u8,
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
