import 'dart:async';
import 'dart:io';

import 'package:dbus/dbus.dart';

const _busName = 'dev.hasali.Fang';
const _managerPath = '/dev/hasali/Fang';
const _managerInterface = 'dev.hasali.Fang.DeviceManager';
const _deviceInterface = 'dev.hasali.Fang.Device';

class Device {
  const Device({required this.path, required this.name});

  final DBusObjectPath path;
  final String name;
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
        return Device(path: path, name: name.asString());
      }),
    );
  }

  Future<void> close() => _client.close();
}
