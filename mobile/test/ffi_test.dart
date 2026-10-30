import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:kw_mobile/src/kw_ffi.dart';

/// 真 FFI 测试：需要先把动态库路径传进来。
///
/// PowerShell:
///   $env:KW_FFI_LIB='D:\my-own-program\ssh-keywarden-rs\target\release\kw_ffi.dll'
///   flutter test
void main() {
  final lib = Platform.environment['KW_FFI_LIB'];
  final skip = lib == null || lib.isEmpty
      ? '设置 KW_FFI_LIB 指向 kw_ffi 动态库后再跑'
      : null;

  test('初始化 / 密钥包 / 主机 / 微软同步配置（真 FFI）', () async {
    final dir = Directory.systemTemp.createTempSync('kw-mobile-ffi-');
    addTearDown(() {
      try {
        dir.deleteSync(recursive: true);
      } catch (_) {
        // Windows 上引擎还握着目录时删不掉，交给系统临时目录清理
      }
    });

    final ffi = KwFfi.instance;
    ffi.init(dir.path);

    final boot = ffi.callMap('bootstrap');
    expect(boot['hosts'], isEmpty);
    expect(boot['localKeys'], isA<Map>());

    final key = ffi.callMap('generateKey', {
      'algorithm': 'ed25519',
      'label': '测试手机',
      'comment': 'mobile@test',
    });
    final privatePath = key['privateKeyPath'] as String;
    expect(File(privatePath).existsSync(), isTrue);
    expect(File('$privatePath.pub').existsSync(), isTrue);
    expect(key['fingerprint'], startsWith('SHA256:'));

    final boot2 = ffi.callMap('bootstrap');
    final entries = ((boot2['localKeys'] as Map)['entries'] as List)
        .cast<Map>();
    expect(entries.where((e) => e['kind'] == 'private'), isNotEmpty);
    expect(entries.where((e) => e['kind'] == 'public'), isNotEmpty);

    final host = ffi.callMap('saveHost', {
      'name': 'prod',
      'host': '203.0.113.10',
      'port': 22,
      'username': 'root',
      'identityFile': '',
      'remotePath': '',
    });
    expect(host['id'], isNotEmpty);

    ffi.callMap('setKeyNote', {
      'fingerprint': key['fingerprint'],
      'note': '测试备注',
    });
    final notes = ffi.callMap('bootstrap')['keyNotes'] as Map;
    expect(notes[key['fingerprint']], isA<Map>());

    final ms = ffi.callMap('msStatus');
    expect(ms['loggedIn'], isFalse);
    expect(
      () => ffi.callMap('msSaveConfig', {'clientId': 'not-a-guid'}),
      throwsA(isA<KwException>()),
    );
    final saved = ffi.callMap('msSaveConfig', {
      'clientId': '0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9',
      'syncKey': 'correct horse',
    });
    expect(saved['configured'], isTrue);
    expect(saved['clientId'], '0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9');
    expect(saved['hasSyncKey'], isTrue);

    final cleared = ffi.callMap('msSaveConfig', {'syncKey': ''});
    expect(cleared['hasSyncKey'], isFalse);

    ffi.callRaw('deleteHost', {'id': host['id']});
    expect(ffi.callMap('bootstrap')['hosts'], isEmpty);

    expect(() => ffi.callMap('doesNotExist'), throwsA(isA<KwException>()));
  }, skip: skip);
}
