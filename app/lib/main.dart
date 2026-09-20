import 'package:flutter/widgets.dart';
import 'package:forui/forui.dart';

import 'device_client.dart';
import 'theme/theme.dart';

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
  late final Stream<List<Device>> _devices = _client.watchDevices();
  Device? _selected;

  @override
  void dispose() {
    _client.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return FScaffold(
      sidebar: StreamBuilder<List<Device>>(
        stream: _devices,
        builder: (context, snapshot) {
          final devices = snapshot.data;
          return FSidebar(
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
                        icon: Icon(FLucideIcons.mouse),
                        label: Text(device.name),
                        selected: _selected?.path == device.path,
                        onPress: () => setState(() => _selected = device),
                      ),
                ],
              ),
            ],
          );
        },
      ),
      child: Center(child: Text(_selected?.name ?? 'Select a device')),
    );
  }
}
