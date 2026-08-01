use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use zbus::names::InterfaceName;
use zbus::zvariant::OwnedObjectPath;

pub struct DeviceManager {
    pub devices: Mutex<BTreeMap<PathBuf, Vec<Device>>>,
}

pub struct Device {
    pub objects: Vec<DeviceObject>,
    pub tasks: Vec<tokio::task::AbortHandle>,
}

pub struct DeviceObject {
    pub path: OwnedObjectPath,
    pub interfaces: Vec<InterfaceName<'static>>,
}

impl DeviceManager {
    pub fn new() -> DeviceManager {
        DeviceManager {
            devices: Mutex::new(BTreeMap::default()),
        }
    }

    pub fn add_device(&self, syspath: PathBuf, device: Device) {
        self.devices.lock().entry(syspath).or_default().push(device);
    }

    pub fn remove_devices(&self, syspath: &Path) -> Vec<Device> {
        self.devices.lock().remove(syspath).unwrap_or_default()
    }
}
