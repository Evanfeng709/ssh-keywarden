import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../app_state.dart';
import '../models.dart';
import '../terminal_emulator.dart';
import '../terminal_runner.dart';
import 'common.dart';
import 'theme.dart';

/// 纯终端页：PTY 在后台 isolate 里持续读，界面只渲染网格。
class TerminalTab extends StatefulWidget {
  const TerminalTab({super.key, required this.state, required this.active});

  final AppState state;

  /// 只有这一页被选中时才去开 PTY、拉输出。
  final bool active;

  @override
  State<TerminalTab> createState() => _TerminalTabState();
}

const double _fontSize = 13;
const double _lineHeight = 1.3;

// 不能做成常量缓存：切主题后文字颜色要跟着变。
TextStyle get _mono => TextStyle(
  fontFamily: 'monospace',
  fontSize: _fontSize,
  height: _lineHeight,
  color: KwColors.text,
);

class _TerminalTabState extends State<TerminalTab> {
  TerminalScreen _screen = TerminalScreen();
  final TextEditingController _input = TextEditingController();
  final ScrollController _scroll = ScrollController();
  late final FocusNode _focus = FocusNode(
    debugLabel: 'kw-terminal-input',
    onKeyEvent: _onKey,
  );

  TerminalRunner? _runner;
  StreamSubscription<TerminalEvent>? _events;
  String? _sid;
  String? _termId;
  String? _error;
  bool _closed = false;
  bool _opening = false;

  String _lastInput = '';
  bool _clearScheduled = false;

  double _cellW = 8;
  double _cellH = _fontSize * _lineHeight;
  int _cols = 80;
  int _rows = 24;
  Timer? _resizeTimer;
  int _pendingCols = 0;
  int _pendingRows = 0;

  bool _stickToBottom = true;

  @override
  void initState() {
    super.initState();
    _measure();
    widget.state.addListener(_onStateChanged);
    _syncSession();
  }

  @override
  void didUpdateWidget(TerminalTab oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.active != widget.active) {
      _syncSession();
    }
  }

  @override
  void dispose() {
    widget.state.removeListener(_onStateChanged);
    _resizeTimer?.cancel();
    _events?.cancel();
    _runner?.dispose();
    _input.dispose();
    _scroll.dispose();
    _focus.dispose();
    super.dispose();
  }

  /* ------------------------------------------------------------------ */
  /* 会话与终端生命周期                                                  */
  /* ------------------------------------------------------------------ */

  void _onStateChanged() {
    _syncSession();
    if (mounted) setState(() {});
  }

  void _syncSession() {
    final sid = widget.state.session?.sid;
    if (sid != _sid) {
      _teardown();
      _sid = sid;
      _closed = false;
      _error = null;
      if (mounted) setState(() {});
    }
    _syncActive();
  }

  /// PTY 跨页保活：切走只暂停拉取，切回来接着用；第一次进这一页才真正开。
  void _syncActive() {
    final sid = _sid;
    if (sid == null) return;
    if (!widget.active) {
      _runner?.pause();
      return;
    }
    final runner = _runner;
    if (runner != null) {
      runner.resume();
      return;
    }
    if (_closed || _opening) return;
    _open(sid);
  }

  void _teardown() {
    _events?.cancel();
    _events = null;
    _runner?.dispose();
    _runner = null;
    _termId = null;
  }

  Future<void> _open(String sid) async {
    _opening = true;
    final screen = TerminalScreen(cols: _cols, rows: _rows);
    setState(() {
      _screen = screen;
      _closed = false;
      _error = null;
      _termId = null;
    });
    try {
      final runner = await TerminalRunner.open(
        sid: sid,
        cols: _cols,
        rows: _rows,
      );
      if (!mounted || _sid != sid) {
        runner.dispose();
        return;
      }
      _runner = runner;
      _events = runner.events.listen(_onEvent);
      if (!widget.active) runner.pause();
      setState(() {});
    } catch (error) {
      if (!mounted) return;
      setState(() => _error = error.toString());
    } finally {
      _opening = false;
    }
  }

  void _onEvent(TerminalEvent event) {
    if (!mounted) return;
    switch (event) {
      case TerminalOpened(:final termId, :final cols, :final rows):
        setState(() {
          _termId = termId;
          _screen.resize(cols, rows);
        });
      case TerminalOutput(:final bytes, :final dropped):
        if (dropped) {
          // 中间丢过数据，旧状态不可信，直接重画一张空白的
          _screen = TerminalScreen(cols: _screen.cols, rows: _screen.rows);
        }
        _screen.feed(bytes);
        setState(() {});
        _followBottom();
      case TerminalResized(:final cols, :final rows):
        setState(() => _screen.resize(cols, rows));
      case TerminalClosed():
        setState(() => _closed = true);
      case TerminalFailed(:final message, :final code):
        setState(() {
          _closed = true;
          _error = code == 'SESSION_LOST' ? '会话已断开，终端随之结束' : message;
        });
    }
  }

  void _followBottom() {
    if (!_stickToBottom || !_scroll.hasClients) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_scroll.hasClients) return;
      _scroll.jumpTo(_scroll.position.maxScrollExtent);
    });
  }

  /* ------------------------------------------------------------------ */
  /* 输入                                                                */
  /* ------------------------------------------------------------------ */

  void _sendBytes(List<int> bytes) {
    if (bytes.isEmpty) return;
    _runner?.input(bytes);
  }

  void _sendText(String text) {
    if (text.isEmpty) return;
    _sendBytes(utf8.encode(text.replaceAll('\n', '\r')));
  }

  void _onInputChanged(String value) {
    final previous = _lastInput;
    _lastInput = value;
    if (value == previous) return;
    if (value.startsWith(previous)) {
      _sendText(value.substring(previous.length));
    } else if (previous.startsWith(value)) {
      _sendBytes(List<int>.filled(previous.length - value.length, 0x7f));
    } else {
      _sendText(value);
    }
    _scheduleClear();
  }

  /// 输入框只是中转站：发完就清空，避免内容越攒越多。
  void _scheduleClear() {
    if (_clearScheduled) return;
    _clearScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _clearScheduled = false;
      if (!mounted) return;
      if (_input.text.isEmpty) return;
      _input.clear();
      _lastInput = '';
    });
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text == null || text.isEmpty) return;
    final body = text.replaceAll('\r\n', '\r');
    if (_screen.bracketedPaste) {
      _sendBytes(utf8.encode('\x1b[200~$body\x1b[201~'));
    } else {
      _sendText(body);
    }
  }

  KeyEventResult _onKey(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    final bytes = _encodeKey(event.logicalKey);
    if (bytes == null) return KeyEventResult.ignored;
    _sendBytes(bytes);
    return KeyEventResult.handled;
  }

  List<int>? _encodeKey(LogicalKeyboardKey key) {
    final app = _screen.applicationCursorKeys;
    if (key == LogicalKeyboardKey.escape) return <int>[0x1b];
    if (key == LogicalKeyboardKey.tab) return <int>[0x09];
    if (key == LogicalKeyboardKey.arrowUp) {
      return utf8.encode(app ? '\x1bOA' : '\x1b[A');
    }
    if (key == LogicalKeyboardKey.arrowDown) {
      return utf8.encode(app ? '\x1bOB' : '\x1b[B');
    }
    if (key == LogicalKeyboardKey.arrowRight) {
      return utf8.encode(app ? '\x1bOC' : '\x1b[C');
    }
    if (key == LogicalKeyboardKey.arrowLeft) {
      return utf8.encode(app ? '\x1bOD' : '\x1b[D');
    }
    if (key == LogicalKeyboardKey.home) return utf8.encode('\x1b[H');
    if (key == LogicalKeyboardKey.end) return utf8.encode('\x1b[F');
    if (key == LogicalKeyboardKey.delete) return utf8.encode('\x1b[3~');
    if (key == LogicalKeyboardKey.pageUp) return utf8.encode('\x1b[5~');
    if (key == LogicalKeyboardKey.pageDown) return utf8.encode('\x1b[6~');
    if (key == LogicalKeyboardKey.f1) return utf8.encode('\x1bOP');
    if (key == LogicalKeyboardKey.f2) return utf8.encode('\x1bOQ');
    if (key == LogicalKeyboardKey.f3) return utf8.encode('\x1bOR');
    if (key == LogicalKeyboardKey.f4) return utf8.encode('\x1bOS');
    return null;
  }

  /* ------------------------------------------------------------------ */
  /* 尺寸                                                                */
  /* ------------------------------------------------------------------ */

  void _measure() {
    final painter = TextPainter(
      text: TextSpan(text: 'M' * 32, style: _mono),
      textDirection: TextDirection.ltr,
    )..layout();
    _cellW = painter.width / 32;
    _cellH = painter.height;
    painter.dispose();
    if (_cellW <= 0) _cellW = 8;
    if (_cellH <= 0) _cellH = _fontSize * _lineHeight;
  }

  void _scheduleLayout(double width, double height) {
    if (width <= 0 || height <= 0) return;
    final cols = (width / _cellW).floor().clamp(20, 400);
    final rows = (height / _cellH).floor().clamp(5, 200);
    if (cols == _cols && rows == _rows) return;
    _cols = cols;
    _rows = rows;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      setState(() => _screen.resize(cols, rows));
      // 旋转/键盘弹起时会连着来几次，攒一下再告诉服务端
      _pendingCols = cols;
      _pendingRows = rows;
      _resizeTimer?.cancel();
      _resizeTimer = Timer(Duration(milliseconds: 180), () {
        _runner?.resize(_pendingCols, _pendingRows);
      });
    });
  }

  /* ------------------------------------------------------------------ */
  /* 渲染                                                                */
  /* ------------------------------------------------------------------ */

  @override
  Widget build(BuildContext context) {
    final session = widget.state.session;
    if (session == null) return _placeholder();
    return Padding(
      padding: EdgeInsets.fromLTRB(10, 10, 10, 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _header(session),
          SizedBox(height: 8),
          Expanded(child: _body()),
          SizedBox(height: 6),
          _keyBar(),
          _inputBar(),
        ],
      ),
    );
  }

  Widget _placeholder() {
    return ListView(
      padding: EdgeInsets.all(14),
      children: [
        panel(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(
                    Icons.terminal_outlined,
                    size: 18,
                    color: KwColors.faint,
                  ),
                  SizedBox(width: 8),
                  Text(
                    '尚未连接',
                    style: TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                      color: KwColors.text,
                    ),
                  ),
                ],
              ),
              SizedBox(height: 10),
              Text(
                '先在「服务器密钥」里连接一台主机，再回来打开终端。'
                '终端和密钥管理共用同一条 SSH 会话。',
                style: TextStyle(
                  fontSize: 12.5,
                  height: 1.6,
                  color: KwColors.dim,
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _header(SessionView session) {
    final state = _closed
        ? (_error == null ? '终端已结束' : _error!)
        : _termId == null
        ? '正在打开终端…'
        : '${_screen.cols}×${_screen.rows}';
    final color = _closed
        ? (_error == null ? KwColors.faint : KwColors.danger)
        : _termId == null
        ? KwColors.warn
        : KwColors.ok;
    return Row(
      children: [
        Container(
          width: 6,
          height: 6,
          decoration: BoxDecoration(color: color, shape: BoxShape.circle),
        ),
        SizedBox(width: 8),
        Expanded(
          child: Text(
            session.endpoint,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(
              fontSize: 12.5,
              fontWeight: FontWeight.w600,
              color: KwColors.text,
            ),
          ),
        ),
        chip(state, color: color),
        SizedBox(width: 6),
        IconButton(
          tooltip: '复制全部',
          visualDensity: VisualDensity.compact,
          onPressed: _copyAll,
          icon: Icon(Icons.copy_rounded, size: 17, color: KwColors.dim),
        ),
        IconButton(
          tooltip: _closed ? '重新打开' : '关闭终端',
          visualDensity: VisualDensity.compact,
          onPressed: _toggleTerminal,
          icon: Icon(
            _closed ? Icons.refresh_rounded : Icons.close_rounded,
            size: 17,
            color: KwColors.dim,
          ),
        ),
      ],
    );
  }

  void _copyAll() {
    final screen = _screen;
    final history = screen.historyText;
    final body = history.isEmpty
        ? screen.text.trimRight()
        : '${history.trimRight()}\n${screen.text.trimRight()}';
    if (body.trim().isEmpty) {
      showSnack(context, '终端里还没有内容');
      return;
    }
    copyText(context, body, '终端内容');
  }

  void _toggleTerminal() {
    final sid = _sid;
    if (sid == null) return;
    if (_closed) {
      _teardown();
      _open(sid);
    } else {
      _runner?.close();
    }
  }

  Widget _body() {
    return Container(
      decoration: BoxDecoration(
        color: KwColors.bg,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: KwColors.border),
      ),
      clipBehavior: Clip.antiAlias,
      child: LayoutBuilder(
        builder: (context, constraints) {
          _scheduleLayout(constraints.maxWidth, constraints.maxHeight);
          final screen = _screen;
          final history = screen.history;
          final grid = screen.grid;
          final cursorX = screen.cursorVisible ? screen.cursorX : -1;
          final cursorY = screen.cursorVisible ? screen.cursorY : -1;
          return NotificationListener<ScrollNotification>(
            onNotification: (notification) {
              if (notification is ScrollUpdateNotification ||
                  notification is ScrollEndNotification) {
                _stickToBottom = _atBottom();
              }
              return false;
            },
            child: ListView.builder(
              controller: _scroll,
              padding: EdgeInsets.zero,
              itemExtent: _cellH,
              itemCount: history.length + grid.length,
              itemBuilder: (context, index) {
                final isGrid = index >= history.length;
                final row = isGrid
                    ? grid[index - history.length]
                    : history[index];
                final rowCursor = isGrid && index - history.length == cursorY
                    ? cursorX
                    : -1;
                return _row(row, cursorX: rowCursor);
              },
            ),
          );
        },
      ),
    );
  }

  bool _atBottom() {
    if (!_scroll.hasClients) return true;
    final position = _scroll.position;
    return position.pixels >= position.maxScrollExtent - _cellH * 2;
  }

  Widget _row(List<TermCell> row, {int cursorX = -1}) {
    final spans = <InlineSpan>[];
    var i = 0;
    while (i < row.length) {
      final cell = row[i];
      if (cell.isContinuation) {
        i++;
        continue;
      }
      if (i == cursorX) {
        spans.add(
          TextSpan(
            text: String.fromCharCode(cell.rune),
            style: _penStyle(cell.pen, cursor: true),
          ),
        );
        i++;
        continue;
      }
      final buffer = StringBuffer()..writeCharCode(cell.rune);
      var j = i + 1;
      while (j < row.length &&
          j != cursorX &&
          !row[j].isContinuation &&
          cell.pen.sameStyle(row[j].pen)) {
        buffer.writeCharCode(row[j].rune);
        j++;
      }
      spans.add(TextSpan(text: buffer.toString(), style: _penStyle(cell.pen)));
      i = j;
    }
    return Text.rich(
      TextSpan(children: spans),
      style: _mono,
      maxLines: 1,
      softWrap: false,
      overflow: TextOverflow.clip,
    );
  }

  TextStyle _penStyle(TermPen pen, {bool cursor = false}) {
    var fg = pen.fg >= 0 ? Color(pen.fg) : KwColors.text;
    var bg = pen.bg >= 0 ? Color(pen.bg) : null;
    if (pen.inverse) {
      final swapped = bg ?? KwColors.text;
      bg = fg;
      fg = swapped;
    }
    if (pen.dim) fg = fg.withValues(alpha: 0.65);
    if (cursor) {
      bg = KwColors.accent;
      fg = KwColors.bg;
    }
    return TextStyle(
      fontFamily: _mono.fontFamily,
      fontSize: _fontSize,
      height: _lineHeight,
      color: fg,
      backgroundColor: bg,
      fontWeight: pen.bold ? FontWeight.w700 : FontWeight.w400,
      decoration: pen.underline ? TextDecoration.underline : null,
      decorationColor: fg,
    );
  }

  Widget _keyBar() {
    final app = _screen.applicationCursorKeys;
    return SizedBox(
      height: 34,
      child: ListView(
        scrollDirection: Axis.horizontal,
        children: [
          _key('Esc', () => _sendBytes(<int>[0x1b])),
          _key('Tab', () => _sendBytes(<int>[0x09])),
          _key('Ctrl+C', () => _sendBytes(<int>[0x03])),
          _key('Ctrl+D', () => _sendBytes(<int>[0x04])),
          _key('Ctrl+Z', () => _sendBytes(<int>[0x1a])),
          _key('←', () => _sendBytes(utf8.encode(app ? '\x1bOD' : '\x1b[D'))),
          _key('↑', () => _sendBytes(utf8.encode(app ? '\x1bOA' : '\x1b[A'))),
          _key('↓', () => _sendBytes(utf8.encode(app ? '\x1bOB' : '\x1b[B'))),
          _key('→', () => _sendBytes(utf8.encode(app ? '\x1bOC' : '\x1b[C'))),
          _key('粘贴', _paste, icon: Icons.content_paste_rounded),
        ],
      ),
    );
  }

  Widget _key(String label, VoidCallback onTap, {IconData? icon}) {
    return Padding(
      padding: EdgeInsets.only(right: 6),
      child: GestureDetector(
        onTap: onTap,
        child: Container(
          alignment: Alignment.center,
          padding: EdgeInsets.symmetric(horizontal: icon == null ? 12 : 9),
          decoration: BoxDecoration(
            color: KwColors.surface3,
            borderRadius: BorderRadius.circular(6),
            border: Border.all(color: KwColors.border),
          ),
          child: icon == null
              ? Text(
                  label,
                  style: TextStyle(
                    fontSize: 12,
                    fontFamily: 'monospace',
                    color: KwColors.dim,
                  ),
                )
              : Icon(icon, size: 16, color: KwColors.dim),
        ),
      ),
    );
  }

  Widget _inputBar() {
    return Container(
      margin: EdgeInsets.only(top: 6),
      padding: EdgeInsets.symmetric(horizontal: 10),
      decoration: BoxDecoration(
        color: KwColors.panel,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: KwColors.border),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.center,
        children: [
          Icon(Icons.keyboard_rounded, size: 16, color: KwColors.faint),
          SizedBox(width: 8),
          Expanded(
            child: TextField(
              controller: _input,
              focusNode: _focus,
              minLines: 1,
              maxLines: 3,
              keyboardType: TextInputType.multiline,
              textInputAction: TextInputAction.newline,
              style: _mono,
              decoration: InputDecoration(
                hintText: '在这里输入，回车发送',
                hintStyle: TextStyle(fontSize: 12.5, color: KwColors.faint),
                border: InputBorder.none,
                isDense: true,
                contentPadding: EdgeInsets.symmetric(vertical: 10),
              ),
              onChanged: _onInputChanged,
            ),
          ),
          IconButton(
            tooltip: '粘贴',
            visualDensity: VisualDensity.compact,
            onPressed: _paste,
            icon: Icon(
              Icons.content_paste_rounded,
              size: 17,
              color: KwColors.dim,
            ),
          ),
        ],
      ),
    );
  }
}
