/// kw-ffi 返回结构的轻量模型。字段名与 Rust 侧 camelCase 一一对应。
library;

class HostConfig {
  const HostConfig({
    required this.id,
    required this.name,
    required this.host,
    required this.port,
    required this.username,
    required this.identityFile,
    required this.remotePath,
  });

  final String id;
  final String name;
  final String host;
  final int port;
  final String username;
  final String identityFile;
  final String remotePath;

  String get label => name.isEmpty ? host : name;
  String get endpoint => '$username@$host:$port';

  factory HostConfig.fromJson(Map<String, dynamic> json) => HostConfig(
    id: json['id']?.toString() ?? '',
    name: json['name']?.toString() ?? '',
    host: json['host']?.toString() ?? '',
    port: (json['port'] as num?)?.toInt() ?? 22,
    username: json['username']?.toString() ?? '',
    identityFile:
        json['identityFile']?.toString() ??
        json['identity_file']?.toString() ??
        '',
    remotePath: json['remotePath']?.toString() ?? '',
  );
}

class LocalKeyEntry {
  const LocalKeyEntry({
    required this.path,
    required this.name,
    required this.kind,
    required this.type,
    required this.fingerprint,
    required this.comment,
    required this.encrypted,
    required this.error,
    required this.size,
  });

  final String path;
  final String name;
  final String kind; // private / public
  final String type;
  final String fingerprint;
  final String comment;
  final bool encrypted;
  final String error;
  final int size;

  bool get isPrivate => kind == 'private';

  factory LocalKeyEntry.fromJson(Map<String, dynamic> json) => LocalKeyEntry(
    path: json['path']?.toString() ?? '',
    name: json['name']?.toString() ?? '',
    kind: json['kind']?.toString() ?? '',
    type: json['type']?.toString() ?? '',
    fingerprint: json['fingerprint']?.toString() ?? '',
    comment: json['comment']?.toString() ?? '',
    encrypted: json['encrypted'] == true,
    error: json['error']?.toString() ?? '',
    size: (json['size'] as num?)?.toInt() ?? 0,
  );
}

class KeyItem {
  const KeyItem({
    required this.index,
    required this.kind,
    required this.type,
    required this.fingerprint,
    required this.comment,
    required this.options,
    required this.duplicateOf,
    required this.reason,
    required this.text,
  });

  final int index;
  final String kind; // key / comment / malformed
  final String type;
  final String fingerprint;
  final String comment;
  final String options;
  final int? duplicateOf;
  final String reason;
  final String text;

  bool get isKey => kind == 'key';
  bool get isDuplicate => duplicateOf != null;

  factory KeyItem.fromJson(Map<String, dynamic> json) => KeyItem(
    index: (json['index'] as num?)?.toInt() ?? 0,
    kind: json['kind']?.toString() ?? '',
    type: json['type']?.toString() ?? '',
    fingerprint: json['fingerprint']?.toString() ?? '',
    comment: json['comment']?.toString() ?? '',
    options: json['options']?.toString() ?? '',
    duplicateOf: (json['duplicateOf'] as num?)?.toInt(),
    reason: json['reason']?.toString() ?? '',
    text: json['text']?.toString() ?? '',
  );
}

class SessionView {
  const SessionView({
    required this.sid,
    required this.host,
    required this.serverUname,
    required this.home,
    required this.remotePath,
    required this.fileExists,
    required this.keysLoaded,
    required this.authMethod,
    required this.usedKeyFp,
    required this.items,
    required this.totalKeys,
    required this.malformed,
    required this.comments,
  });

  final String sid;
  final Map<String, dynamic> host;
  final String serverUname;
  final String home;
  final String remotePath;
  final bool fileExists;
  final bool keysLoaded;
  final String authMethod;
  final String usedKeyFp;
  final List<KeyItem> items;
  final int totalKeys;
  final int malformed;
  final int comments;

  String get hostLabel {
    final name = host['name']?.toString() ?? '';
    if (name.isNotEmpty) return name;
    return host['host']?.toString() ?? '';
  }

  String get endpoint {
    final user = host['username']?.toString() ?? '';
    final addr = host['host']?.toString() ?? '';
    final port = (host['port'] as num?)?.toInt() ?? 22;
    return '$user@$addr:$port';
  }

  bool get isKeySession => keysLoaded;

  List<KeyItem> get keys => items.where((i) => i.isKey).toList();

  factory SessionView.fromJson(Map<String, dynamic> json) {
    final server = json['server'];
    final stats = json['stats'];
    final rawItems = json['items'];
    return SessionView(
      sid: json['sid']?.toString() ?? '',
      host: json['host'] is Map
          ? Map<String, dynamic>.from(json['host'] as Map)
          : <String, dynamic>{},
      serverUname: server is Map ? server['uname']?.toString() ?? '' : '',
      home: json['home']?.toString() ?? '',
      remotePath: json['remotePath']?.toString() ?? '',
      fileExists: json['fileExists'] == true,
      keysLoaded: json['keysLoaded'] == true,
      authMethod: json['authMethod']?.toString() ?? '',
      usedKeyFp: json['usedKeyFp']?.toString() ?? '',
      items: rawItems is List
          ? rawItems
                .whereType<Map>()
                .map((e) => KeyItem.fromJson(Map<String, dynamic>.from(e)))
                .toList()
          : const [],
      totalKeys: stats is Map ? (stats['total'] as num?)?.toInt() ?? 0 : 0,
      malformed: stats is Map ? (stats['malformed'] as num?)?.toInt() ?? 0 : 0,
      comments: stats is Map ? (stats['comments'] as num?)?.toInt() ?? 0 : 0,
    );
  }
}

/// 微软账号同步的当前状态（`msStatus` / `msSaveConfig` / `msCompleteLogin`）。
class MsStatus {
  const MsStatus({
    required this.configured,
    required this.clientId,
    required this.loggedIn,
    required this.accountName,
    required this.accountEmail,
    required this.lastSyncAt,
    required this.hasSyncKey,
  });

  final bool configured;
  final String clientId;
  final bool loggedIn;
  final String accountName;
  final String accountEmail;
  final int lastSyncAt;
  final bool hasSyncKey;

  static const empty = MsStatus(
    configured: false,
    clientId: '',
    loggedIn: false,
    accountName: '',
    accountEmail: '',
    lastSyncAt: 0,
    hasSyncKey: false,
  );

  String get accountLabel {
    if (accountName.isNotEmpty) return accountName;
    if (accountEmail.isNotEmpty) return accountEmail;
    return '已登录的微软账号';
  }

  factory MsStatus.fromJson(Map<String, dynamic> json) => MsStatus(
    configured: json['configured'] == true,
    clientId: json['clientId']?.toString() ?? '',
    loggedIn: json['loggedIn'] == true,
    accountName: json['accountName']?.toString() ?? '',
    accountEmail: json['accountEmail']?.toString() ?? '',
    lastSyncAt: (json['lastSyncAt'] as num?)?.toInt() ?? 0,
    hasSyncKey: json['hasSyncKey'] == true,
  );
}

/// 设备码登录的一次性提示：用户要在浏览器里输 [userCode]。
class DeviceLoginPrompt {
  const DeviceLoginPrompt({
    required this.deviceCode,
    required this.userCode,
    required this.verificationUri,
    required this.expiresIn,
    required this.interval,
  });

  final String deviceCode;
  final String userCode;
  final String verificationUri;
  final int expiresIn;
  final int interval;

  factory DeviceLoginPrompt.fromJson(Map<String, dynamic> json) =>
      DeviceLoginPrompt(
        deviceCode: json['deviceCode']?.toString() ?? '',
        userCode: json['userCode']?.toString() ?? '',
        verificationUri: json['verificationUri']?.toString() ?? '',
        expiresIn: (json['expiresIn'] as num?)?.toInt() ?? 0,
        interval: (json['interval'] as num?)?.toInt() ?? 5,
      );
}

/// 一次同步的结果：拉没拉、推没推、导入报告。
class MsSyncResult {
  const MsSyncResult({
    required this.pulled,
    required this.pushed,
    required this.lastSyncAt,
    required this.report,
  });

  final bool pulled;
  final bool pushed;
  final int lastSyncAt;
  final Map<String, dynamic> report;

  factory MsSyncResult.fromJson(Map<String, dynamic> json) {
    final raw = json['report'];
    return MsSyncResult(
      pulled: json['pulled'] == true,
      pushed: json['pushed'] == true,
      lastSyncAt: (json['lastSyncAt'] as num?)?.toInt() ?? 0,
      report: raw is Map ? Map<String, dynamic>.from(raw) : const {},
    );
  }
}

class Bootstrap {
  const Bootstrap({
    required this.version,
    required this.hosts,
    required this.keyNotes,
    required this.localKeys,
    required this.keysDir,
    required this.scanErrors,
  });

  final String version;
  final List<HostConfig> hosts;
  final Map<String, String> keyNotes;
  final List<LocalKeyEntry> localKeys;
  final String keysDir;
  final List<String> scanErrors;

  factory Bootstrap.fromJson(Map<String, dynamic> json) {
    final hostsRaw = json['hosts'];
    final notesRaw = json['keyNotes'];
    final local = json['localKeys'];
    final entries = local is Map ? local['entries'] : null;
    final errors = local is Map ? local['errors'] : null;
    final notes = <String, String>{};
    if (notesRaw is Map) {
      notesRaw.forEach((key, value) {
        if (value is Map && value['note'] != null) {
          notes[key.toString()] = value['note'].toString();
        }
      });
    }
    return Bootstrap(
      version: json['version']?.toString() ?? '',
      hosts: hostsRaw is List
          ? hostsRaw
                .whereType<Map>()
                .map((e) => HostConfig.fromJson(Map<String, dynamic>.from(e)))
                .toList()
          : const [],
      keyNotes: notes,
      localKeys: entries is List
          ? entries
                .whereType<Map>()
                .map(
                  (e) => LocalKeyEntry.fromJson(Map<String, dynamic>.from(e)),
                )
                .toList()
          : const [],
      keysDir: local is Map ? local['dir']?.toString() ?? '' : '',
      scanErrors: errors is List
          ? errors
                .map((e) => e is Map ? e['message']?.toString() ?? '' : '')
                .toList()
          : const [],
    );
  }

  LocalKeyEntry? privateKeyFor(String fingerprint) {
    if (fingerprint.isEmpty) return null;
    for (final entry in localKeys) {
      if (entry.isPrivate && entry.fingerprint == fingerprint) return entry;
    }
    return null;
  }

  List<LocalKeyEntry> get privateKeys =>
      localKeys.where((e) => e.isPrivate).toList();
}
