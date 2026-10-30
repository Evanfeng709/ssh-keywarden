import 'dart:async';
import 'dart:convert';
import 'dart:isolate';
import 'dart:typed_data';

import 'kw_ffi.dart';

/// 后台终端 isolate 推给界面的消息。
sealed class TerminalEvent {
  const TerminalEvent();
}

/// PTY 已开好；`cols/rows` 是服务端夹取后真正生效的尺寸。
class TerminalOpened extends TerminalEvent {
  const TerminalOpened(this.termId, this.cols, this.rows);

  final String termId;
  final int cols;
  final int rows;
}

/// 一段 PTY 输出。`dropped` 为真说明中间有数据被队列丢掉，界面应清屏重画。
class TerminalOutput extends TerminalEvent {
  const TerminalOutput(this.bytes, {required this.dropped});

  final Uint8List bytes;
  final bool dropped;
}

/// 服务端确认的新尺寸（可能和请求值不同）。
class TerminalResized extends TerminalEvent {
  const TerminalResized(this.cols, this.rows);

  final int cols;
  final int rows;
}

/// PTY 自己退出（shell 收到 exit）。
class TerminalClosed extends TerminalEvent {
  const TerminalClosed();
}

/// 出错或会话没了；`code` 复用 kw-ffi 的错误码。
class TerminalFailed extends TerminalEvent {
  const TerminalFailed(this.message, this.code);

  final String message;
  final String? code;
}

/// 一个终端的后台代理：读循环跑在独立 isolate 里，界面只收事件。
class TerminalRunner {
  TerminalRunner._(this._isolate, this._port, this._events);

  final Isolate _isolate;
  final ReceivePort _port;
  final StreamController<TerminalEvent> _events;

  SendPort? _commands;
  final List<Map<String, Object?>> _outbox = [];
  bool _disposed = false;

  Stream<TerminalEvent> get events => _events.stream;

  static Future<TerminalRunner> open({
    required String sid,
    required int cols,
    required int rows,
  }) async {
    final port = ReceivePort();
    final isolate = await Isolate.spawn(_terminalWorker, <Object?>[
      port.sendPort,
      sid,
      cols,
      rows,
    ], debugName: 'kw-terminal');
    final runner = TerminalRunner._(
      isolate,
      port,
      // 单订阅而不是 broadcast：界面订阅前到达的事件（比如 opened）要缓冲住
      StreamController<TerminalEvent>(),
    );
    runner._listen();
    return runner;
  }

  void _listen() {
    _port.listen((dynamic message) {
      if (message is! Map) return;
      final map = Map<String, Object?>.from(message);
      switch (map['ev']) {
        case 'port':
          _commands = map['port'] as SendPort;
          for (final cmd in _outbox) {
            _commands!.send(cmd);
          }
          _outbox.clear();
        case 'opened':
          _emit(
            TerminalOpened(
              map['termId'] as String,
              map['cols'] as int,
              map['rows'] as int,
            ),
          );
        case 'output':
          final bytes = map['bytes'];
          _emit(
            TerminalOutput(
              bytes is Uint8List ? bytes : Uint8List(0),
              dropped: map['dropped'] == true,
            ),
          );
        case 'resized':
          _emit(TerminalResized(map['cols'] as int, map['rows'] as int));
        case 'closed':
          _emit(const TerminalClosed());
        case 'error':
          _emit(
            TerminalFailed(
              map['message']?.toString() ?? '终端出错',
              map['code'] as String?,
            ),
          );
      }
    }, onDone: () => _events.close());
  }

  void _emit(TerminalEvent event) {
    if (_disposed || _events.isClosed) return;
    _events.add(event);
  }

  void _send(Map<String, Object?> command) {
    if (_disposed) return;
    final port = _commands;
    if (port == null) {
      _outbox.add(command);
    } else {
      port.send(command);
    }
  }

  /// 键盘/粘贴输入，字节直接进 PTY。
  void input(List<int> bytes) {
    if (bytes.isEmpty) return;
    _send({'cmd': 'input', 'bytes': Uint8List.fromList(bytes)});
  }

  void resize(int cols, int rows) =>
      _send({'cmd': 'resize', 'cols': cols, 'rows': rows});

  /// 切到别的页时暂停拉取，输出先堆在 Rust 侧的队列里。
  void pause() => _send({'cmd': 'pause'});

  void resume() => _send({'cmd': 'resume'});

  /// 主动关掉 PTY（不会断开 SSH 会话）。
  void close() => _send({'cmd': 'close'});

  void dispose() {
    if (_disposed) return;
    _disposed = true;
    // 让 worker 先把 PTY 关掉再退，别把句柄留在 Rust 侧
    final commands = _commands;
    if (commands != null) {
      commands.send(<String, Object?>{'cmd': 'stop'});
    }
    _port.close();
    _events.close();
    // worker 万一卡在长轮询里，稍后强制收掉
    unawaited(
      Future<void>.delayed(
        const Duration(seconds: 1),
        () => _isolate.kill(priority: Isolate.immediate),
      ),
    );
  }
}

/* ------------------------------------------------------------------ */
/* isolate 侧                                                          */
/* ------------------------------------------------------------------ */

void _terminalWorker(List<Object?> args) {
  final host = args[0] as SendPort;
  final commands = ReceivePort();
  final worker = _Worker(host, commands, args[1] as String);
  host.send(<String, Object?>{'ev': 'port', 'port': commands.sendPort});
  commands.listen(worker.onCommand);
  worker.start(args[2] as int, args[3] as int);
}

const int _pollWaitMs = 250;
const int _pausedWaitMs = 400;

class _Worker {
  _Worker(this._host, this._commands, this._sid);

  final SendPort _host;
  final ReceivePort _commands;
  final String _sid;

  String? _termId;
  bool _stopped = false;
  bool _paused = false;
  final List<Map<String, Object?>> _early = [];

  Future<void> start(int cols, int rows) async {
    Map<String, dynamic> data;
    try {
      data = await _call('termOpen', <String, dynamic>{
        'sid': _sid,
        'cols': cols,
        'rows': rows,
      });
    } catch (error) {
      _fail(error);
      return;
    }
    _termId = data['termId'] as String?;
    if (_termId == null) {
      _fail(const FormatException('termOpen 没有返回 termId'));
      return;
    }
    _host.send(<String, Object?>{
      'ev': 'opened',
      'termId': _termId,
      'cols': data['cols'],
      'rows': data['rows'],
    });
    for (final command in _early) {
      _run(command);
    }
    _early.clear();
    unawaited(_readLoop());
  }

  void onCommand(Object? message) {
    if (message is! Map) return;
    final command = Map<String, Object?>.from(message);
    switch (command['cmd']) {
      case 'pause':
        _paused = true;
      case 'resume':
        _paused = false;
      case 'stop':
        _closeQuietly();
      default:
        if (_termId == null) {
          _early.add(command);
        } else {
          _run(command);
        }
    }
  }

  /// 写类命令同步执行：一来很快，二来天然保证输入顺序。
  void _run(Map<String, Object?> command) {
    final termId = _termId;
    if (termId == null || _stopped) return;
    try {
      switch (command['cmd']) {
        case 'input':
          final bytes = command['bytes'];
          if (bytes is List<int>) {
            _callNow('termWrite', <String, dynamic>{
              'termId': termId,
              'data': base64Encode(bytes),
            });
          }
        case 'resize':
          final data = _callNow('termResize', <String, dynamic>{
            'termId': termId,
            'cols': command['cols'],
            'rows': command['rows'],
          });
          _host.send(<String, Object?>{
            'ev': 'resized',
            'cols': data['cols'],
            'rows': data['rows'],
          });
        case 'close':
          _callNow('termClose', <String, dynamic>{'termId': termId});
          _stop(closed: true);
      }
    } catch (error) {
      _fail(error);
    }
  }

  Future<void> _readLoop() async {
    while (!_stopped) {
      final termId = _termId;
      if (termId == null) return;
      if (_paused) {
        await Future<void>.delayed(const Duration(milliseconds: _pausedWaitMs));
        continue;
      }
      Map<String, dynamic> data;
      try {
        data = await _call('termRead', <String, dynamic>{
          'termId': termId,
          'waitMs': _pollWaitMs,
        });
      } catch (error) {
        _fail(error);
        return;
      }
      if (_stopped) return;
      final raw = data['data'];
      final dropped = data['dropped'] == true;
      final bytes = raw is String ? base64Decode(raw) : Uint8List(0);
      if (bytes.isNotEmpty || dropped) {
        _host.send(<String, Object?>{
          'ev': 'output',
          'bytes': bytes,
          'dropped': dropped,
        });
      }
      if (data['closed'] == true) {
        _stop(closed: true);
      }
    }
  }

  void _stop({bool closed = false}) {
    if (_stopped) return;
    _stopped = true;
    _commands.close();
    _host.send(<String, Object?>{'ev': closed ? 'closed' : 'stopped'});
  }

  /// 界面走了：顺手把 PTY 收掉，免得 Rust 侧的句柄越攒越多。
  void _closeQuietly() {
    final termId = _termId;
    _stopped = true;
    _commands.close();
    _termId = null;
    if (termId == null) return;
    try {
      _callNow('termClose', <String, dynamic>{'termId': termId});
    } catch (_) {
      // 会话已经没了，句柄会被 disconnect 一并清掉
    }
  }

  void _fail(Object error) {
    if (_stopped) return;
    _stopped = true;
    _commands.close();
    _host.send(<String, Object?>{
      'ev': 'error',
      'message': error is KwException ? error.message : '$error',
      'code': error is KwException ? error.code : null,
    });
  }
}

Map<String, dynamic> _callNow(String method, Map<String, dynamic> args) {
  final raw = KwFfi.instance.callRaw(method, args);
  return raw is Map ? Map<String, dynamic>.from(raw) : <String, dynamic>{};
}

/// 读取会阻塞到有数据或超时，所以放到临时 isolate，别卡住写命令。
Future<Map<String, dynamic>> _call(
  String method,
  Map<String, dynamic> args,
) async {
  final raw = await Isolate.run(() => KwFfi.instance.callRaw(method, args));
  return raw is Map ? Map<String, dynamic>.from(raw) : <String, dynamic>{};
}
