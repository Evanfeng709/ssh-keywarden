import 'dart:io';

import 'package:flutter/services.dart';

/// 应用可写目录：Rust 侧会在它下面建 `data/` 与 `keys/`。
///
/// 刻意不引 path_provider：Flutter 在 Windows 上构建插件需要开发者模式开符号链接，
/// 而这里 Android 一行 MethodChannel、iOS/macOS 用沙盒 HOME 就够了。
class AppPaths {
  static const MethodChannel _channel = MethodChannel('kw_mobile/paths');

  static Future<String> supportDir() async {
    // 测试与本地联调可以指定目录，避免写进真实配置
    final override = Platform.environment['KW_APP_DIR'];
    if (override != null && override.isNotEmpty) return override;

    if (Platform.isAndroid) {
      final dir = await _channel.invokeMethod<String>('appSupportDir');
      if (dir == null || dir.isEmpty) {
        throw StateError('无法取得 Android 应用目录');
      }
      return dir;
    }

    final home =
        Platform.environment['USERPROFILE'] ?? Platform.environment['HOME'];
    if (home == null || home.isEmpty) {
      throw StateError('无法确定用户主目录');
    }
    if (Platform.isWindows) {
      return '$home\\AppData\\Roaming\\ssh-keywarden-mobile';
    }
    if (Platform.isIOS || Platform.isMacOS) {
      return '$home/Library/Application Support/ssh-keywarden-mobile';
    }
    return '$home/.ssh-keywarden-mobile';
  }
}
