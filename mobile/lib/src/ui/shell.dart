import 'package:flutter/material.dart';

import '../app_state.dart';
import '../version.dart';
import 'keys_tab.dart';
import 'keyring_tab.dart';
import 'terminal_tab.dart';
import 'theme.dart';

class MobileShell extends StatefulWidget {
  const MobileShell({
    super.key,
    required this.state,
    required this.palette,
    required this.onPalette,
  });

  final AppState state;
  final int palette;
  final ValueChanged<int> onPalette;

  @override
  State<MobileShell> createState() => _MobileShellState();
}

class _MobileShellState extends State<MobileShell> {
  int _index = 0;

  void _openPalettes() {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (ctx) => paletteSheet(
        ctx,
        current: widget.palette,
        onSelect: (i) {
          Navigator.pop(ctx);
          widget.onPalette(i);
        },
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final state = widget.state;

    if (state.bootstrap == null && state.error != null) {
      return _FatalScreen(state: state);
    }

    return Scaffold(
      appBar: AppBar(
        titleSpacing: 12,
        title: Row(
          children: [
            Icon(Icons.vpn_key_rounded, color: KwColors.accent, size: 17),
            const SizedBox(width: 7),
            const Text('SSH Keywarden'),
            const SizedBox(width: 7),
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
              decoration: BoxDecoration(
                border: Border.all(color: KwColors.border),
                borderRadius: BorderRadius.circular(9),
              ),
              child: Text(
                kMobileVersion,
                style: TextStyle(
                  fontSize: 9.5,
                  color: KwColors.faint,
                  fontFamily: 'monospace',
                ),
              ),
            ),
          ],
        ),
        actions: [
          IconButton(
            tooltip: '外观主题',
            visualDensity: VisualDensity.compact,
            onPressed: _openPalettes,
            icon: Icon(Icons.palette_outlined, size: 19, color: KwColors.dim),
          ),
          if (state.busy)
            const Padding(
              padding: EdgeInsets.only(right: 14),
              child: Center(
                child: SizedBox(
                  width: 15,
                  height: 15,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
              ),
            ),
        ],
        bottom: state.busy
            ? const PreferredSize(
                preferredSize: Size.fromHeight(2),
                child: LinearProgressIndicator(minHeight: 2),
              )
            : null,
      ),
      body: IndexedStack(
        index: _index,
        children: [
          KeysTab(state: state, onOpenSync: () => setState(() => _index = 2)),
          TerminalTab(state: state, active: _index == 1),
          KeyringTab(state: state),
        ],
      ),
      bottomNavigationBar: Container(
        decoration: BoxDecoration(
          border: Border(top: BorderSide(color: KwColors.border)),
        ),
        child: NavigationBar(
          height: 56,
          selectedIndex: _index,
          onDestinationSelected: (i) => setState(() => _index = i),
          destinations: const [
            NavigationDestination(
              icon: Icon(Icons.shield_outlined),
              selectedIcon: Icon(Icons.shield_rounded),
              label: '服务器密钥',
            ),
            NavigationDestination(
              icon: Icon(Icons.terminal_outlined),
              selectedIcon: Icon(Icons.terminal_rounded),
              label: '纯终端',
            ),
            NavigationDestination(
              icon: Icon(Icons.key_outlined),
              selectedIcon: Icon(Icons.key_rounded),
              label: '密钥包·同步',
            ),
          ],
        ),
      ),
    );
  }
}

class _FatalScreen extends StatelessWidget {
  const _FatalScreen({required this.state});

  final AppState state;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                Icons.error_outline_rounded,
                color: KwColors.danger,
                size: 36,
              ),
              const SizedBox(height: 12),
              Text(
                '无法初始化 kw-ffi',
                style: TextStyle(
                  fontSize: 16,
                  fontWeight: FontWeight.w600,
                  color: KwColors.text,
                ),
              ),
              const SizedBox(height: 8),
              SelectableText(
                state.error ?? '未知错误',
                textAlign: TextAlign.center,
                style: TextStyle(
                  fontSize: 12,
                  color: KwColors.dim,
                  height: 1.5,
                ),
              ),
              const SizedBox(height: 18),
              FilledButton(
                onPressed: () => state.init().catchError((Object _) {}),
                child: const Text('重试'),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
