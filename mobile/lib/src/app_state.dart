import 'package:flutter/foundation.dart';

import 'kw_ffi.dart';
import 'models.dart';

/// 全局状态：一份 bootstrap（主机/备注/密钥包）+ 当前 SSH 会话。
class AppState extends ChangeNotifier {
  Bootstrap? bootstrap;
  SessionView? session;
  Map<String, dynamic>? sshd;
  bool busy = false;
  String? error;

  KwEngine get _engine => KwEngine.instance;

  bool get connected => session != null;
  bool get sshKeyOnly => sshd?['keyOnly'] == true;

  Future<void> init() async {
    try {
      await _engine.init();
      await reload();
    } catch (e) {
      error = e.toString();
      notifyListeners();
      rethrow;
    }
  }

  Future<T> _run<T>(Future<T> Function() action) async {
    busy = true;
    error = null;
    notifyListeners();
    try {
      return await action();
    } catch (e) {
      error = e.toString();
      rethrow;
    } finally {
      busy = false;
      notifyListeners();
    }
  }

  Future<void> reload() => _run(_reloadQuiet);

  Future<void> _reloadQuiet() async {
    final data = await _engine.call('bootstrap');
    bootstrap = Bootstrap.fromJson(data);
  }

  /* ------------------------------------------------------------------ */
  /* 主机                                                                */
  /* ------------------------------------------------------------------ */

  Future<HostConfig> saveHost({
    String? id,
    required String name,
    required String host,
    required int port,
    required String username,
    String identityFile = '',
    String remotePath = '',
  }) => _run(() async {
    final data = await _engine.call('saveHost', {
      if (id != null && id.isNotEmpty) 'id': id,
      'name': name,
      'host': host,
      'port': port,
      'username': username,
      'identityFile': identityFile,
      'remotePath': remotePath,
    });
    await _reloadQuiet();
    return HostConfig.fromJson(data);
  });

  Future<void> deleteHost(String id) => _run(() async {
    await _engine.call('deleteHost', {'id': id});
    await _reloadQuiet();
  });

  /* ------------------------------------------------------------------ */
  /* 连接                                                                */
  /* ------------------------------------------------------------------ */

  Future<void> connect(
    HostConfig host, {
    String? password,
    String? passphrase,
    String? trustFingerprint,
  }) => _run(() async {
    final data = await _engine.call('connect', {
      'hostId': host.id,
      if (password != null && password.isNotEmpty) 'password': password,
      if (passphrase != null && passphrase.isNotEmpty) 'passphrase': passphrase,
      if (trustFingerprint != null && trustFingerprint.isNotEmpty)
        'trustFingerprint': trustFingerprint,
      'withKeys': true,
    });
    session = SessionView.fromJson(data);
    await _loadSshdQuiet();
  });

  Future<void> refreshSession() => _run(() async {
    final current = session;
    if (current == null) return;
    final data = await _engine.call('refresh', {'sid': current.sid});
    session = SessionView.fromJson(data);
    await _loadSshdQuiet();
  });

  Future<void> disconnect() => _run(() async {
    final current = session;
    if (current != null) {
      await _engine.call('disconnect', {'sid': current.sid});
    }
    session = null;
    sshd = null;
  });

  Future<void> _loadSshdQuiet() async {
    final current = session;
    if (current == null) {
      sshd = null;
      return;
    }
    try {
      sshd = await _engine.call('sshd', {'sid': current.sid});
    } catch (_) {
      // sshd 读不到不影响密钥管理，界面上仅显示「状态未知」
      sshd = null;
    }
  }

  Future<void> loadSshd() => _run(() async {
    final current = session;
    if (current == null) return;
    sshd = await _engine.call('sshd', {'sid': current.sid});
  });

  Future<Map<String, dynamic>> setKeyOnly(bool enable) => _run(() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final data = await _engine.call('setKeyOnly', {
      'sid': current.sid,
      'enable': enable,
    });
    final status = data['status'];
    if (status is Map) {
      sshd = Map<String, dynamic>.from(status);
    } else {
      await _loadSshdQuiet();
    }
    return data;
  });

  /* ------------------------------------------------------------------ */
  /* 密钥操作                                                            */
  /* ------------------------------------------------------------------ */

  void _applySession(Map<String, dynamic> data) {
    final next = data['session'];
    if (next is Map) {
      session = SessionView.fromJson(Map<String, dynamic>.from(next));
    }
  }

  /// 新增密钥到服务器。
  ///
  /// `mode` 为 `generate` 时在手机上现生成一对；为 `paste` 时把已有公钥
  /// （通常是从电脑同步过来的那把）直接追加进去 —— 换设备时不必重新造钥匙。
  Future<Map<String, dynamic>> addKey({
    String mode = 'generate',
    String algorithm = 'ed25519',
    String comment = '',
    String? passphrase,
    String publicKey = '',
    bool switchToNew = false,
  }) => _run(() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final body = <String, dynamic>{
      'sid': current.sid,
      'mode': mode,
      'switchToNew': switchToNew,
    };
    if (mode == 'paste') {
      body['publicKey'] = publicKey;
    } else {
      body['generator'] = algorithm;
      body['comment'] = comment;
      if (passphrase != null && passphrase.isNotEmpty) {
        body['passphrase'] = passphrase;
      }
    }
    final data = await _engine.call('addKey', body);
    _applySession(data);
    await _reloadQuiet();
    return data;
  });

  Future<Map<String, dynamic>> replaceKey({
    required String oldFingerprint,
    String algorithm = 'ed25519',
    String comment = '',
    String? passphrase,
    bool force = false,
  }) => _run(() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final data = await _engine.call('replaceKey', {
      'sid': current.sid,
      'oldFingerprint': oldFingerprint,
      'mode': 'generate',
      'generator': algorithm,
      'comment': comment,
      if (passphrase != null && passphrase.isNotEmpty) 'passphrase': passphrase,
      if (force) 'force': true,
    });
    _applySession(data);
    await _reloadQuiet();
    return data;
  });

  Future<Map<String, dynamic>> deleteKeys(
    List<String> fingerprints, {
    bool force = false,
  }) => _run(() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final data = await _engine.call('deleteKeys', {
      'sid': current.sid,
      'fingerprints': fingerprints,
      if (force) 'force': true,
    });
    _applySession(data);
    return data;
  });

  Future<Map<String, dynamic>> setNote(String fingerprint, String note) =>
      _run(() async {
        await _engine.call('setKeyNote', {
          'fingerprint': fingerprint,
          'note': note,
        });
        await _reloadQuiet();
        return const {};
      });

  /* ------------------------------------------------------------------ */
  /* 备份                                                                */
  /* ------------------------------------------------------------------ */

  Future<List<Map<String, dynamic>>> backups() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final data = await _engine.call('backups', {'sid': current.sid});
    final list = data['backups'];
    return list is List
        ? list
              .whereType<Map>()
              .map((e) => Map<String, dynamic>.from(e))
              .toList()
        : const [];
  }

  Future<Map<String, dynamic>> backupDiff(String name) async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    return _engine.call('backupDiff', {'sid': current.sid, 'name': name});
  }

  Future<Map<String, dynamic>> restoreBackup(
    String name, {
    bool force = false,
  }) => _run(() async {
    final current = session;
    if (current == null) throw KwException('尚未连接服务器');
    final data = await _engine.call('restoreBackup', {
      'sid': current.sid,
      'name': name,
      if (force) 'force': true,
    });
    _applySession(data);
    return data;
  });

  /* ------------------------------------------------------------------ */
  /* 随身密钥包                                                          */
  /* ------------------------------------------------------------------ */

  Future<Map<String, dynamic>> generateKey({
    String algorithm = 'ed25519',
    String label = 'mobile',
    String comment = '',
    String? passphrase,
  }) => _run(() async {
    final data = await _engine.call('generateKey', {
      'algorithm': algorithm,
      'label': label,
      'comment': comment,
      if (passphrase != null && passphrase.isNotEmpty) 'passphrase': passphrase,
    });
    await _reloadQuiet();
    return data;
  });

  /* ------------------------------------------------------------------ */
  /* 微软账号（OneDrive）同步                                            */
  /* ------------------------------------------------------------------ */

  /// 只读本机状态文件，不碰网络；启动时也会调，所以不走 busy。
  Future<MsStatus> msStatus() async {
    final data = await _engine.call('msStatus');
    return MsStatus.fromJson(data);
  }

  /// 传 null 表示不动，传空串表示清掉该项。
  Future<MsStatus> msSaveConfig({String? clientId, String? syncKey}) =>
      _run(() async {
        final data = await _engine.call('msSaveConfig', {
          'clientId': ?clientId,
          'syncKey': ?syncKey,
        });
        return MsStatus.fromJson(data);
      });

  Future<DeviceLoginPrompt> msStartLogin({String? clientId}) => _run(() async {
    final data = await _engine.call('msStartLogin', {
      if (clientId != null && clientId.isNotEmpty) 'clientId': clientId,
    });
    return DeviceLoginPrompt.fromJson(data);
  });

  /// 轮询直到用户授权完成 / 超时；[timeoutSec] 传 0 用后端默认值。
  Future<MsStatus> msCompleteLogin(String deviceCode, {int timeoutSec = 0}) =>
      _run(() async {
        final data = await _engine.call('msCompleteLogin', {
          'deviceCode': deviceCode,
          'timeoutSec': timeoutSec,
        });
        return MsStatus.fromJson(data);
      });

  Future<void> msLogout() => _run(() async {
    await _engine.call('msLogout');
  });

  /// 口令留空就用本机记住的那份；指纹冲突通过 [trustChanged] 二次确认后覆盖。
  ///
  /// [forceUpload] 跳过拉取、直接用本机覆盖云端 —— 云端包解不开时的唯一出路。
  Future<MsSyncResult> msSync({
    String passphrase = '',
    List<String> trustChanged = const [],
    bool forceUpload = false,
  }) => _run(() async {
    final data = await _engine.call('msSync', {
      'passphrase': passphrase,
      if (trustChanged.isNotEmpty) 'trustChanged': trustChanged,
      if (forceUpload) 'forceUpload': true,
    });
    await _reloadQuiet();
    return MsSyncResult.fromJson(data);
  });
}
