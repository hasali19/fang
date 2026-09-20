import 'dart:async';
import 'dart:io';

import 'package:dbus/dbus.dart';

const _busName = 'dev.hasali.Fang';
const _managerPath = '/dev/hasali/Fang';
const _managerInterface = 'dev.hasali.Fang.DeviceManager';
const _deviceInterface = 'dev.hasali.Fang.Device';
const _mouseInterface = 'dev.hasali.Fang.Mouse';

enum DeviceType {
  dock('Dock'),
  mouse('Mouse');

  const DeviceType(this.label);

  final String label;

  static DeviceType? fromDBus(DBusValue value) {
    return switch (value.asUint32()) {
      1 => dock,
      2 => mouse,
      _ => null,
    };
  }
}

class Device {
  const Device({required this.path, required this.name, required this.type});

  final DBusObjectPath path;
  final String name;
  final DeviceType? type;
}

class MouseStatus {
  const MouseStatus({
    required this.isConnected,
    required this.hasBattery,
    required this.batteryLevel,
    required this.isCharging,
    required this.dpi,
  });

  final bool isConnected;
  final bool hasBattery;
  final int batteryLevel;
  final bool isCharging;
  final int dpi;
}

class DeviceClient {
  DeviceClient()
    : _client = Platform.environment.containsKey('FANG_SESSION_BUS')
          ? DBusClient.session()
          : DBusClient.system();

  final DBusClient _client;

  Stream<List<Device>> watchDevices() async* {
    final manager = DBusRemoteObject(
      _client,
      name: _busName,
      path: DBusObjectPath(_managerPath),
    );

    final changes = StreamController<void>();
    final subscription = manager.propertiesChanged.listen((signal) {
      if (signal.propertiesInterface == _managerInterface &&
          signal.changedProperties.containsKey('Devices')) {
        changes.add(null);
      }
    });

    try {
      yield await _fetchDevices(manager);
      await for (final _ in changes.stream) {
        yield await _fetchDevices(manager);
      }
    } finally {
      await subscription.cancel();
      await changes.close();
    }
  }

  Future<List<Device>> _fetchDevices(DBusRemoteObject manager) async {
    final value = await manager.getProperty(_managerInterface, 'Devices');
    final paths = value.asObjectPathArray().toList();

    return Future.wait(
      paths.map((path) async {
        final object = DBusRemoteObject(_client, name: _busName, path: path);
        final name = await object.getProperty(_deviceInterface, 'Name');
        final type = await object.getProperty(_deviceInterface, 'DeviceType');
        return Device(
          path: path,
          name: name.asString(),
          type: DeviceType.fromDBus(type),
        );
      }),
    );
  }

  Stream<MouseStatus> watchMouse(DBusObjectPath path) async* {
    final object = DBusRemoteObject(_client, name: _busName, path: path);

    final changes = StreamController<void>();
    final subscription = object.propertiesChanged.listen((signal) {
      if (signal.propertiesInterface == _mouseInterface) {
        changes.add(null);
      }
    });

    try {
      yield await _fetchMouse(object);
      await for (final _ in changes.stream) {
        yield await _fetchMouse(object);
      }
    } finally {
      await subscription.cancel();
      await changes.close();
    }
  }

  Future<MouseStatus> _fetchMouse(DBusRemoteObject object) async {
    final props = await object.getAllProperties(_mouseInterface);
    return MouseStatus(
      isConnected: props['IsConnected']!.asBoolean(),
      hasBattery: props['HasBattery']!.asBoolean(),
      batteryLevel: props['BatteryLevel']!.asByte(),
      isCharging: props['IsCharging']!.asBoolean(),
      dpi: props['Dpi']!.asUint16(),
    );
  }

  Future<void> close() => _client.close();
}
