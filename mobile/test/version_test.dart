// 版本号散在两处：`pubspec.yaml`（决定 APK 的 versionName / versionCode）
// 和 `lib/src/version.dart`（顶栏展示）。漏改过一次（pubspec 涨到 .2、顶栏还写着 .1），
// 用一条断言把它们钉在一起。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kw_mobile/src/version.dart';

void main() {
  test('kMobileVersion 与 pubspec 的 versionCode 对得上', () {
    final pubspec = File('pubspec.yaml').readAsStringSync();
    final line = pubspec
        .split('\n')
        .firstWhere((l) => l.trimLeft().startsWith('version:'));
    final raw = line.split(':').last.trim(); // 2026.10.30+2026103002

    final parts = raw.split('+');
    expect(parts, hasLength(2), reason: 'version 必须是 <日期>+<versionCode> 形式');

    final code = parts[1];
    expect(code, hasLength(10), reason: 'versionCode 应是 YYYYMMDDNN 十位');
    expect(RegExp(r'^\d+$').hasMatch(code), isTrue, reason: 'versionCode 必须是纯数字');

    // 2026103002 → v2026.10.30.2
    final date =
        '${code.substring(0, 4)}.${code.substring(4, 6)}.${code.substring(6, 8)}';
    final seq = int.parse(code.substring(8));
    expect(
      kMobileVersion,
      'v$date.$seq',
      reason: '改了 pubspec 的 version 就要同步改 lib/src/version.dart 的 kMobileVersion',
    );

    // versionName 本身也得是那个日期（Android 设置页显示的）
    expect(parts[0], date, reason: 'versionName 与 versionCode 里的日期不一致');
  });
}
