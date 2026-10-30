import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

import 'app_paths.dart';

/// kw-ffi 返回的业务错误（`ok=false`）。
class KwException implements Exception {
  KwException(this.message, {this.code, this.details});

  final String message;
  final String? code;
  final Map<String, dynamic>? details;

  bool get isHostKeyUnknown => code == 'HOSTKEY_UNKNOWN';
  bool get isHostKeyChanged => code == 'HOSTKEY_CHANGED';
  bool get isSessionLost => code == 'SESSION_LOST';

  @override
  String toString() => message;
}

/// 对 `kw_ffi` 三个 C 导出函数的薄封装。
///
/// Rust 侧的引擎是进程级单例：`kw_init` 只需在根 isolate 调一次，
/// 之后任何 isolate 都可以直接 `kw_call`（连接这类慢调用会放到后台 isolate）。
class KwFfi {
  KwFfi._(DynamicLibrary lib)
    : _init = lib
          .lookupFunction<
            Pointer<Utf8> Function(Pointer<Utf8>),
            Pointer<Utf8> Function(Pointer<Utf8>)
          >('kw_init'),
      _call = lib
          .lookupFunction<
            Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>),
            Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>)
          >('kw_call'),
      _free = lib
          .lookupFunction<
            Void Function(Pointer<Utf8>),
            void Function(Pointer<Utf8>)
          >('kw_free_string');

  final Pointer<Utf8> Function(Pointer<Utf8>) _init;
  final Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>) _call;
  final void Function(Pointer<Utf8>) _free;

  static KwFfi? _instance;

  static KwFfi get instance => _instance ??= KwFfi._(_open());

  static DynamicLibrary _open() {
    final override = Platform.environment['KW_FFI_LIB'];
    if (override != null && override.isNotEmpty) {
      return DynamicLibrary.open(override);
    }
    if (Platform.isWindows) {
      // 手机上不会走这条：Windows 只是本仓的开发/测试宿主。
      // 先找 exe 旁边，再找 cargo 的 target 目录（flutter run 的工作目录是 mobile/）。
      final exeDir = File(Platform.resolvedExecutable).parent.path;
      final candidates = <String>[
        '$exeDir\\kw_ffi.dll',
        'kw_ffi.dll',
        '..\\target\\release\\kw_ffi.dll',
        '..\\..\\target\\release\\kw_ffi.dll',
      ];
      for (final path in candidates) {
        if (File(path).existsSync()) {
          return DynamicLibrary.open(File(path).absolute.path);
        }
      }
      return DynamicLibrary.open('kw_ffi.dll');
    }
    if (Platform.isAndroid) return DynamicLibrary.open('libkw_ffi.so');
    if (Platform.isIOS) return DynamicLibrary.process();
    if (Platform.isMacOS) return DynamicLibrary.open('libkw_ffi.dylib');
    return DynamicLibrary.open('libkw_ffi.so');
  }

  Map<String, dynamic> init(String appDir) {
    final input = appDir.toNativeUtf8();
    try {
      return _decode(_init(input), 'init');
    } finally {
      malloc.free(input);
    }
  }

  /// 返回 `data` 原样（可能是 Map / String / bool / null）。
  dynamic callRaw(String method, [Map<String, dynamic> args = const {}]) {
    final m = method.toNativeUtf8();
    final a = jsonEncode(args).toNativeUtf8();
    try {
      return _decode(_call(m, a), method)['data'];
    } finally {
      malloc.free(m);
      malloc.free(a);
    }
  }

  Map<String, dynamic> callMap(
    String method, [
    Map<String, dynamic> args = const {},
  ]) {
    final data = callRaw(method, args);
    if (data is Map) return Map<String, dynamic>.from(data);
    throw KwException('$method 返回的不是对象：$data');
  }

  String callString(String method, [Map<String, dynamic> args = const {}]) {
    final data = callRaw(method, args);
    if (data is String) return data;
    throw KwException('$method 返回的不是字符串');
  }

  Map<String, dynamic> _decode(Pointer<Utf8> pointer, String op) {
    if (pointer == nullptr) {
      throw KwException('$op 返回了空指针');
    }
    final text = pointer.toDartString();
    _free(pointer);
    final decoded = jsonDecode(text);
    if (decoded is! Map) throw KwException('$op 返回的不是 JSON 对象');
    final map = Map<String, dynamic>.from(decoded);
    if (map['ok'] != true) {
      final rawDetails = map['details'];
      throw KwException(
        map['error']?.toString() ?? '$op 失败',
        code: map['code']?.toString(),
        details: rawDetails is Map
            ? Map<String, dynamic>.from(rawDetails)
            : null,
      );
    }
    return map;
  }
}

/// 引擎门面：初始化一次，之后所有调用都走这里。
class KwEngine {
  KwEngine._();

  static final KwEngine instance = KwEngine._();

  bool _ready = false;
  String? _dir;

  Future<void> init() async {
    if (_ready) return;
    final dir = await AppPaths.supportDir();
    await Isolate.run(() => KwFfi.instance.init(dir));
    _dir = dir;
    _ready = true;
  }

  String get appDir => _dir ?? '';

  /// 会把 FFI 调用放到后台 isolate，避免 connect（最长 20s）冻住界面。
  Future<dynamic> callRaw(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    await init();
    return Isolate.run(() => KwFfi.instance.callRaw(method, args));
  }

  Future<Map<String, dynamic>> call(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final data = await callRaw(method, args);
    if (data is Map) return Map<String, dynamic>.from(data);
    return {'value': data};
  }

  Future<String> callText(
    String method, [
    Map<String, dynamic> args = const {},
  ]) async {
    final data = await callRaw(method, args);
    if (data is String) return data;
    throw KwException('$method 返回的不是字符串');
  }
}
