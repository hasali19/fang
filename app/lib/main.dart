import 'package:dbus/dbus.dart';
import 'package:flutter/widgets.dart';
import 'package:forui/forui.dart';

import 'device_client.dart';
import 'theme/theme.dart';

extension on DeviceType {
  IconData get icon => switch (this) {
    DeviceType.dock => FLucideIcons.dock,
    DeviceType.mouse => FLucideIcons.mouse,
  };
}

void main() {
  runApp(const FangApp());
}

class FangApp extends StatelessWidget {
  const FangApp({super.key});

  @override
  Widget build(BuildContext context) {
    return WidgetsApp(
      color: const Color(0xFF000000),
      pageRouteBuilder: <T>(settings, builder) => PageRouteBuilder<T>(
        settings: settings,
        pageBuilder: (context, _, _) => builder(context),
      ),
      builder: (context, child) {
        final brightness = MediaQuery.of(context).platformBrightness;
        final theme = switch (brightness) {
          Brightness.dark => darkTheme,
          Brightness.light => lightTheme,
        };
        return FTheme(data: theme, child: child!);
      },
      home: const HomePage(),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  final _client = DeviceClient();

  late final Stream<List<Device>> _devices;
  DBusObjectPath? _selectedPath;

  @override
  void initState() {
    super.initState();
    _devices = _client.watchDevices();
  }

  @override
  void dispose() {
    _client.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<List<Device>>(
      stream: _devices,
      builder: (context, snapshot) {
        final devices = snapshot.data;

        // Fall back to the first device if nothing is selected or the
        // selected device is gone.
        final selected = devices == null || devices.isEmpty
            ? null
            : devices.firstWhere(
                (d) => d.path == _selectedPath,
                orElse: () => devices.first,
              );

        final emptyMessage = switch (devices) {
          _ when snapshot.hasError => 'Service unavailable',
          null => 'Connecting...',
          _ => 'No devices found',
        };

        return FScaffold(
          sidebar: FSidebar(
            header: SizedBox(height: 16),
            children: [
              FSidebarGroup(
                label: const Text('Devices'),
                children: [
                  if (snapshot.hasError)
                    FSidebarItem(label: const Text('Service unavailable'))
                  else if (devices == null)
                    FSidebarItem(label: const Text('Connecting…'))
                  else if (devices.isEmpty)
                    FSidebarItem(label: const Text('No devices found'))
                  else
                    for (final device in devices)
                      FSidebarItem(
                        icon: Icon(
                          device.type?.icon ?? FLucideIcons.circleHelp,
                        ),
                        label: Text(device.name),
                        selected: selected?.path == device.path,
                        onPress: () =>
                            setState(() => _selectedPath = device.path),
                      ),
                ],
              ),
            ],
          ),
          child: switch (selected) {
            null => Center(child: Text(emptyMessage)),
            final device => Padding(
              padding: const EdgeInsets.all(24),
              child: DeviceDetails(
                key: ValueKey(device.path),
                client: _client,
                device: device,
              ),
            ),
          },
        );
      },
    );
  }
}

class DeviceDetails extends StatelessWidget {
  const DeviceDetails({super.key, required this.client, required this.device});

  final DeviceClient client;
  final Device device;

  @override
  Widget build(BuildContext context) {
    final theme = context.theme;
    return SingleChildScrollView(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        spacing: 24,
        children: [
          Row(
            spacing: 16,
            children: [
              Icon(
                device.type?.icon ?? FLucideIcons.circleHelp,
                size: 40,
                color: theme.colors.primary,
              ),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(device.name, style: theme.typography.display.xl2),
                    FBadge(
                      variant: .secondary,
                      child: Text(device.type?.label ?? 'Unknown'),
                    ),
                  ],
                ),
              ),
            ],
          ),
          if (device.type == DeviceType.mouse)
            StreamBuilder<MouseStatus>(
              stream: client.watchMouse(device.path),
              builder: (context, snapshot) {
                final status = snapshot.data;
                if (snapshot.hasError) {
                  return const Text('Status unavailable');
                }
                if (status == null) return SizedBox();
                return _MouseStatusView(status: status);
              },
            ),
        ],
      ),
    );
  }
}

class _MouseStatusView extends StatelessWidget {
  const _MouseStatusView({required this.status});

  final MouseStatus status;

  @override
  Widget build(BuildContext context) {
    final theme = context.theme;
    return Wrap(
      spacing: 16,
      runSpacing: 16,
      children: [
        _StatCard(
          icon: status.isConnected ? FLucideIcons.wifi : FLucideIcons.wifiOff,
          label: 'Connection',
          value: status.isConnected ? 'Connected' : 'Disconnected',
          valueColor: status.isConnected
              ? theme.colors.foreground
              : theme.colors.destructive,
        ),
        if (status.isConnected && status.hasBattery)
          _StatCard(
            icon: status.isCharging
                ? FLucideIcons.batteryCharging
                : FLucideIcons.batteryFull,
            label: status.isCharging ? 'Battery · Charging' : 'Battery',
            value: '${status.batteryLevel}%',
            footer: FDeterminateProgress(value: status.batteryLevel / 100),
          ),
        if (status.isConnected)
          _StatCard(
            icon: FLucideIcons.gauge,
            label: 'Sensitivity',
            value: '${status.dpi} DPI',
          ),
      ],
    );
  }
}

class _StatCard extends StatelessWidget {
  const _StatCard({
    required this.icon,
    required this.label,
    required this.value,
    this.valueColor,
    this.footer,
  });

  final IconData icon;
  final String label;
  final String value;
  final Color? valueColor;
  final Widget? footer;

  @override
  Widget build(BuildContext context) {
    final theme = context.theme;
    return SizedBox(
      // width: 220,
      child: FCard(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            spacing: 8,
            children: [
              Row(
                spacing: 8,
                children: [
                  Icon(icon, size: 16, color: theme.colors.mutedForeground),
                  Text(
                    label,
                    style: theme.typography.body.sm.copyWith(
                      color: theme.colors.mutedForeground,
                    ),
                  ),
                ],
              ),
              Text(
                value,
                style: theme.typography.display.xl.copyWith(color: valueColor),
              ),
              ?footer,
            ],
          ),
        ),
      ),
    );
  }
}
