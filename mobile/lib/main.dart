import 'package:flutter/material.dart';

import 'src/app_state.dart';
import 'src/ui/shell.dart';
import 'src/ui/theme.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const KwApp());
}

class KwApp extends StatefulWidget {
  const KwApp({super.key});

  @override
  State<KwApp> createState() => _KwAppState();
}

class _KwAppState extends State<KwApp> {
  final AppState _state = AppState();
  int _palette = 0;

  @override
  void initState() {
    super.initState();
    KwColors.select(_palette);
    _state.init().catchError((Object _) {
      // 错误已经写进 state.error，界面会显示重试入口
    });
  }

  void _selectPalette(int index) {
    KwColors.select(index);
    if (mounted) setState(() => _palette = index);
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'SSH Keywarden',
      debugShowCheckedModeBanner: false,
      theme: KwTheme.build(),
      home: AnimatedBuilder(
        animation: _state,
        builder: (context, _) => MobileShell(
          state: _state,
          palette: _palette,
          onPalette: _selectPalette,
        ),
      ),
    );
  }
}
