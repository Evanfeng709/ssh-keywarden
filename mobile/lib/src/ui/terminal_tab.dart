import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../app_paths.dart';
import '../app_state.dart';
import '../models.dart';
import '../terminal_emulator.dart';
import '../terminal_input.dart';
import '../terminal_painter.dart';
import '../terminal_runner.dart';
import 'common.dart';
import 'theme.dart';

/// 纯终端页：PTY 在后台 isolate 里持续读，界面按固定格子自己画。
///
/// 交互对齐 Termius：**没有输入框**，点终端自己弹键盘（隐藏的 TextField 只在
/// 输入法组合时兜底）；顶部是可折叠的快捷键工具条；长按选词、拖动扩选、可复制。
class TerminalTab extends StatefulWidget {
  const TerminalTab({
    super.key,
    required this.state,
    required this.active,
    this.onOpenHosts,
  });

  final AppState state;

  /// 只有这一页被选中时才去开 PTY、拉输出。
  final bool active;

  /// 跳回「主机·密钥」页的回调。
  final VoidCallback? onOpenHosts;

  @override
  State<TerminalTab> createState() => _TerminalTabState();
}

const double _lineHeight = 1.3;

/// 双指捏合的字号区间与初值。
const double _minFontSize = 9;
const double _maxFontSize = 22;
const double _defaultFontSize = 13;

/// 顶栏「⚡」抽屉里的高频运维命令。
class _Snippet {
  final String label;
  final String cmd;
  const _Snippet(this.label, this.cmd);
}

const List<_Snippet> _snippets = <_Snippet>[
  _Snippet('tmux', 'tmux attach || tmux new\r'),
  _Snippet('docker ps', 'docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"\r'),
  _Snippet('端口', 'ss -tulpn\r'),
  _Snippet('磁盘/内存', 'df -h && free -m\r'),
  _Snippet('负载', 'uptime && top -bn1 | head -n 15\r'),
  _Snippet('SSH 日志', 'journalctl -u ssh -u sshd -n 40 --no-pager\r'),
];

class _TerminalTabState extends State<TerminalTab> {
  TerminalScreen _screen = TerminalScreen();

  /// 字号（双指捏合可调，落盘记忆）。
  double _fontSize = _defaultFontSize;

  // 不能做成常量缓存：切主题后文字颜色要跟着变。
  TextStyle get _mono => TextStyle(
    fontFamily: 'monospace',
    fontSize: _fontSize,
    height: _lineHeight,
    color: KwColors.text,
  );
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

  /// 输入框当前文本（永远是哨兵 + 尚未提交的组合串），用于算增量。
  String _imeText = kImeSentinel;
  bool _suppressIme = false;

  double _cellW = 8;
  double _cellH = _defaultFontSize * _lineHeight;
  int _cols = 80;
  int _rows = 24;
  bool _hasLayout = false;
  Timer? _resizeTimer;
  int _pendingCols = 0;
  int _pendingRows = 0;

  bool _stickToBottom = true;

  /// 快捷命令抽屉是否展开。
  bool _showSnippets = false;

  /// 选区：锚点与焦点都是 (绝对行号, 列号)，绝对行号把 history 和 grid 连起来。
  (int, int)? _selAnchor;
  (int, int)? _selFocus;

  /// 字号落盘的文件 + 防抖计时器。
  File? _prefsFile;
  Timer? _prefsTimer;

  /// 触控追踪：双指间距用于捏合缩放，单指位移用于备用屏（vim/htop/less）翻页。
  /// 用 Listener 自己算，而不是 GestureDetector.onScale —— 后者会跟 ListView
  /// 抢手势，把普通的上下滚动吃掉。
  final Map<int, Offset> _pointers = <int, Offset>{};
  double _pinchStartDist = 0;
  double _pinchStartFont = _defaultFontSize;
  double _panAccumY = 0;

  bool get _hasSelection => _selAnchor != null && _selFocus != null;

  @override
  void initState() {
    super.initState();
    _measure();
    // 隐藏输入框常驻一个零宽哨兵，退格才能被检测到（见 diffIme 注释）。
    _input.value = const TextEditingValue(
      text: kImeSentinel,
      selection: TextSelection.collapsed(offset: 1),
    );
    _input.addListener(_onImeChanged);
    unawaited(_loadPrefs());
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
    _prefsTimer?.cancel();
    _events?.cancel();
    _runner?.dispose();
    _input.removeListener(_onImeChanged);
    _input.dispose();
    _scroll.dispose();
    _focus.dispose();
    super.dispose();
  }

  /* ------------------------------------------------------------------ */
  /* 设置持久化（字号）                                                  */
  /* ------------------------------------------------------------------ */

  Future<File?> _prefsFilePath() async {
    try {
      final dir = await AppPaths.supportDir();
      return File('$dir${Platform.pathSeparator}terminal-prefs.json');
    } catch (_) {
      return null;
    }
  }

  Future<void> _loadPrefs() async {
    final file = await _prefsFilePath();
    if (file == null) return;
    _prefsFile = file;
    try {
      if (!await file.exists()) return;
      final data = jsonDecode(await file.readAsString());
      final raw = data is Map ? data['fontSize'] : null;
      if (raw is num &&
          raw >= _minFontSize &&
          raw <= _maxFontSize &&
          mounted) {
        setState(() => _fontSize = raw.toDouble());
        _measure();
      }
    } catch (_) {
      // 读不到就用默认字号，别让设置文件坏了整个终端
    }
  }

  void _scheduleSavePrefs() {
    _prefsTimer?.cancel();
    _prefsTimer = Timer(const Duration(milliseconds: 400), () {
      final file = _prefsFile;
      if (file == null) return;
      try {
        file.writeAsStringSync(jsonEncode({'fontSize': _fontSize}));
      } catch (_) {
        // 写不进去下次再来
      }
    });
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
      _selAnchor = null;
      _selFocus = null;
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
    if (!_hasLayout || _closed || _opening) return;
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
          _screen.resize(_cols, _rows);
        });
        if (cols != _cols || rows != _rows) {
          _runner?.resize(_cols, _rows);
        }
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

  /// 输入框有变化：算增量再发，然后把输入框复位回「只剩哨兵」。
  ///
  /// - 组合输入（中文/日文输入法）进行中：先不发，等上屏后整段发。
  /// - 其余情况交给纯函数 [diffIme] 判断是「新增文本」还是「退格」。
  void _onImeChanged() {
    if (_suppressIme) return;
    final value = _input.value;
    // 输入法正在组合（拼音未上屏）：不要把它当普通字符发走
    if (value.composing.isValid && !value.composing.isCollapsed) return;

    final edit = diffIme(_imeText, value.text);
    if (edit == null) return;
    switch (edit) {
      case TextEdit(:final text):
        _sendText(text);
      case BackspaceEdit(:final count):
        _sendBytes(List<int>.filled(count, 0x7f));
    }
    _resetIme();
  }

  /// 把输入框复位成「只剩一个哨兵」，光标放末尾；复位期间的变更不算用户输入。
  void _resetIme() {
    _suppressIme = true;
    _input.value = const TextEditingValue(
      text: kImeSentinel,
      selection: TextSelection.collapsed(offset: 1),
    );
    _imeText = kImeSentinel;
    _suppressIme = false;
  }

  /// 点终端：收起选区并弹键盘。
  void _onTerminalTap() {
    if (_hasSelection) {
      setState(() {
        _selAnchor = null;
        _selFocus = null;
      });
    }
    if (_focus.hasFocus) {
      SystemChannels.textInput.invokeMethod<void>('TextInput.show');
    } else {
      _focus.requestFocus();
    }
    _followBottom();
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
    // 硬键盘的退格/回车直接翻成字节，别让输入框自己吞掉
    if (key == LogicalKeyboardKey.backspace) return <int>[0x7f];
    if (key == LogicalKeyboardKey.enter ||
        key == LogicalKeyboardKey.numpadEnter) {
      return <int>[0x0d];
    }
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
  /* 选区（长按选词 / 拖动扩选 / 复制）                                   */
  /* ------------------------------------------------------------------ */

  /// 屏幕坐标 → (绝对行号, 列号)。
  (int, int)? _cellAt(Offset local) {
    final total = _screen.history.length + _screen.grid.length;
    if (total == 0 || _cellH <= 0 || _cellW <= 0) return null;
    final raw = _scroll.hasClients ? _scroll.offset : 0.0;
    var row = ((raw + local.dy) / _cellH).floor();
    var col = (local.dx / _cellW).floor();
    if (row < 0) row = 0;
    if (row >= total) row = total - 1;
    if (col < 0) col = 0;
    if (col >= _cols) col = _cols - 1;
    return (row, col);
  }

  void _onSelStart(LongPressStartDetails d) {
    final cell = _cellAt(d.localPosition);
    if (cell == null) return;
    HapticFeedback.selectionClick();
    _selectWord(cell);
  }

  void _onSelMove(LongPressMoveUpdateDetails d) {
    final cell = _cellAt(d.localPosition);
    if (cell == null || cell == _selFocus) return;
    setState(() => _selFocus = cell);
  }

  /// 长按落在词上就整词选中，落在空格上就选一格。
  void _selectWord((int, int) cell) {
    final (row, col0) = cell;
    final rows = <List<TermCell>>[..._screen.history, ..._screen.grid];
    if (row < 0 || row >= rows.length) return;
    final cells = rows[row];
    if (cells.isEmpty) return;
    final c = col0.clamp(0, cells.length - 1);
    bool isWord(int i) => i >= 0 && i < cells.length && cells[i].rune != 0x20;
    if (!isWord(c)) {
      setState(() {
        _selAnchor = (row, c);
        _selFocus = (row, c);
      });
      return;
    }
    var s = c;
    var e = c;
    while (isWord(s - 1)) {
      s--;
    }
    while (isWord(e + 1)) {
      e++;
    }
    setState(() {
      _selAnchor = (row, s);
      _selFocus = (row, e);
    });
  }

  /// 把 (锚点, 焦点) 归一成左上 / 右下的顺序。
  (int, int, int, int) _ordered() {
    final a = _selAnchor!;
    final f = _selFocus!;
    if (a.$1 < f.$1 || (a.$1 == f.$1 && a.$2 <= f.$2)) {
      return (a.$1, a.$2, f.$1, f.$2);
    }
    return (f.$1, f.$2, a.$1, a.$2);
  }

  /// 某一行被选中的列区间（含端点），没有就返回 null。
  (int, int)? _selectionForRow(int absRow) {
    if (!_hasSelection) return null;
    final (sr, sc, er, ec) = _ordered();
    if (absRow < sr || absRow > er) return null;
    if (sr == er) return (sc, ec);
    if (absRow == sr) return (sc, _cols - 1);
    if (absRow == er) return (0, ec);
    return (0, _cols - 1);
  }

  String _selectionText() {
    if (!_hasSelection) return '';
    final (sr, sc, er, ec) = _ordered();
    final rows = <List<TermCell>>[..._screen.history, ..._screen.grid];
    final buf = StringBuffer();
    for (var i = sr; i <= er && i < rows.length; i++) {
      if (i < 0) continue;
      final cells = rows[i];
      final from = i == sr ? sc : 0;
      final to = i == er ? ec : cells.length - 1;
      final line = StringBuffer();
      for (var c = from; c <= to && c < cells.length; c++) {
        if (c < 0) continue;
        final cell = cells[c];
        if (cell.isContinuation) continue;
        line.writeCharCode(cell.rune);
      }
      buf.write(line.toString().trimRight());
      if (i < er) buf.write('\n');
    }
    return buf.toString();
  }

  void _copySelection() {
    final text = _selectionText();
    if (text.isEmpty) {
      showSnack(context, '没有选中内容');
      return;
    }
    copyText(context, text, '已选内容');
    setState(() {
      _selAnchor = null;
      _selFocus = null;
    });
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
    // 只按整列取，画的时候每格定死在 i*cellW，最后一列不会再被挤掉
    final cols = (width / _cellW).floor().clamp(10, 400);
    final rows = (height / _cellH).floor().clamp(5, 200);
    final firstLayout = !_hasLayout;
    if (!firstLayout &&
        cols == _cols &&
        rows == _rows &&
        _screen.cols == cols &&
        _screen.rows == rows) {
      return;
    }
    _hasLayout = true;
    _cols = cols;
    _rows = rows;
    _screen.resize(cols, rows);
    if (firstLayout) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _syncActive();
      });
      return;
    }
    _pendingCols = cols;
    _pendingRows = rows;
    _resizeTimer?.cancel();
    _resizeTimer = Timer(const Duration(milliseconds: 180), () {
      _runner?.resize(_pendingCols, _pendingRows);
    });
    _followBottom();
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
          if (_showSnippets) ...[
            SizedBox(height: 6),
            _snippetBar(),
          ],
          SizedBox(height: 6),
          Expanded(child: _body()),
        ],
      ),
    );
  }

  Widget _placeholder() {
    final hosts = widget.state.bootstrap?.hosts ?? <HostConfig>[];
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
                    color: KwColors.accent,
                  ),
                  SizedBox(width: 8),
                  Text(
                    '尚未建立 SSH 终端会话',
                    style: TextStyle(
                      fontSize: 14,
                      fontWeight: FontWeight.w600,
                      color: KwColors.text,
                    ),
                  ),
                ],
              ),
              SizedBox(height: 10),
              Text(
                hosts.isEmpty
                    ? '先在「主机·密钥」页添加并连接一台服务器，即可在此开启交互式 PTY 终端。'
                    : '在「主机·密钥」页点主机卡片上的「打开终端」，将自动复用同一条 SSH 会话进入终端。',
                style: TextStyle(
                  fontSize: 12.5,
                  height: 1.6,
                  color: KwColors.dim,
                ),
              ),
              if (widget.onOpenHosts != null) ...[
                SizedBox(height: 14),
                FilledButton.icon(
                  onPressed: widget.onOpenHosts,
                  icon: Icon(Icons.dns_rounded, size: 16),
                  label: Text(
                    hosts.isEmpty ? '去添加主机' : '前往主机库 (${hosts.length})',
                    style: TextStyle(fontSize: 13),
                  ),
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }

  Widget _header(SessionView session) {
    final String? statusText = _closed
        ? (_error == null ? '终端已结束' : _error!)
        : _termId == null
        ? '正在打开终端…'
        : null;
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
        if (statusText != null) chip(statusText, color: color),
        IconButton(
          tooltip: '快捷命令',
          visualDensity: VisualDensity.compact,
          onPressed: () => setState(() => _showSnippets = !_showSnippets),
          icon: Icon(
            Icons.bolt_rounded,
            size: 18,
            color: _showSnippets ? KwColors.accent : KwColors.dim,
          ),
        ),
        IconButton(
          tooltip: '粘贴',
          visualDensity: VisualDensity.compact,
          onPressed: _paste,
          icon: Icon(Icons.content_paste_rounded, size: 17, color: KwColors.dim),
        ),
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
          var visibleGridCount = grid.length;
          if (!screen.usingAltScreen && grid.isNotEmpty) {
            var last = (cursorY >= 0 ? cursorY + 1 : 1).clamp(1, grid.length);
            for (var i = grid.length - 1; i >= last; i--) {
              if (grid[i].any((c) => !c.isContinuation && c.rune != 0x20)) {
                last = i + 1;
                break;
              }
            }
            visibleGridCount = last;
          }
          final selColor = KwColors.accent.withValues(alpha: 0.32);
          return Stack(
            fit: StackFit.expand,
            children: [
              Listener(
                onPointerDown: (e) {
                  _pointers[e.pointer] = e.position;
                  if (_pointers.length == 2) {
                    final pts = _pointers.values.toList(growable: false);
                    _pinchStartDist = (pts[0] - pts[1]).distance;
                    _pinchStartFont = _fontSize;
                  }
                },
                onPointerMove: (e) {
                  if (!_pointers.containsKey(e.pointer)) return;
                  _pointers[e.pointer] = e.position;

                  // 双指：按间距比例缩放字号（9~22pt 无级）
                  if (_pointers.length >= 2) {
                    if (_pinchStartDist > 8) {
                      final pts = _pointers.values.toList(growable: false);
                      final dist = (pts[0] - pts[1]).distance;
                      final next = (_pinchStartFont * dist / _pinchStartDist)
                          .clamp(_minFontSize, _maxFontSize);
                      if ((next - _fontSize).abs() >= 0.2) {
                        setState(() => _fontSize = next);
                        _measure();
                        _scheduleSavePrefs();
                      }
                    }
                    return;
                  }

                  // 单指：只在备用屏里把上下滑动翻译成方向键（此时禁止列表滚动）
                  if (!_screen.usingAltScreen) return;
                  _panAccumY += e.delta.dy;
                  const step = 18.0;
                  final appApp = _screen.applicationCursorKeys;
                  while (_panAccumY.abs() >= step) {
                    final isUp = _panAccumY > 0;
                    _sendBytes(
                      utf8.encode(
                        isUp
                            ? (appApp ? '\x1bOA' : '\x1b[A')
                            : (appApp ? '\x1bOB' : '\x1b[B'),
                      ),
                    );
                    _panAccumY += isUp ? -step : step;
                  }
                },
                onPointerUp: (e) => _pointers.remove(e.pointer),
                onPointerCancel: (e) => _pointers.remove(e.pointer),
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTap: _onTerminalTap,
                  onLongPressStart: _onSelStart,
                  onLongPressMoveUpdate: _onSelMove,
                  child: NotificationListener<ScrollNotification>(
                    onNotification: (notification) {
                      if (notification is ScrollUpdateNotification ||
                          notification is ScrollEndNotification) {
                        _stickToBottom = _atBottom();
                      }
                      return false;
                    },
                    child: Scrollbar(
                      controller: _scroll,
                      thumbVisibility: false,
                      child: ListView.builder(
                        controller: _scroll,
                        padding: EdgeInsets.zero,
                        itemExtent: _cellH,
                        // 备用屏里滑动要发方向键，别让列表自己滚
                        physics: _screen.usingAltScreen
                            ? const NeverScrollableScrollPhysics()
                            : null,
                        itemCount: history.length + visibleGridCount,
                        itemBuilder: (context, index) {
                          final isGrid = index >= history.length;
                          final row = isGrid
                              ? grid[index - history.length]
                              : history[index];
                          final rowCursor =
                              isGrid && index - history.length == cursorY
                              ? cursorX
                              : -1;
                          final sel = _selectionForRow(index);
                          return _row(
                            row,
                            cursorX: rowCursor,
                            selStart: sel?.$1 ?? -1,
                            selEnd: sel?.$2 ?? -1,
                            selColor: selColor,
                          );
                        },
                      ),
                    ),
                  ),
                ),
              ),
              // 隐藏输入框：只在输入法/硬键盘输入时兜底，肉眼看不到
              _hiddenField(),
              if (_hasSelection)
                Positioned(
                  right: 10,
                  bottom: 10,
                  child: FilledButton.icon(
                    style: FilledButton.styleFrom(
                      visualDensity: VisualDensity.compact,
                      minimumSize: const Size(0, 32),
                      padding: const EdgeInsets.symmetric(horizontal: 12),
                    ),
                    onPressed: _copySelection,
                    icon: const Icon(Icons.copy_rounded, size: 15),
                    label: const Text('复制', style: TextStyle(fontSize: 12)),
                  ),
                ),
            ],
          );
        },
      ),
    );
  }

  Widget _hiddenField() {
    // 放在视口内但完全透明、且不接管触摸：这样输入法才会稳定挂上，
    // 又不会挡住终端的点击/长按选区。
    return Positioned(
      left: 0,
      top: 0,
      width: 320,
      height: 44,
      child: IgnorePointer(
        child: Opacity(
          opacity: 0,
          child: TextField(
            controller: _input,
            focusNode: _focus,
            maxLines: 1,
            showCursor: false,
            enableInteractiveSelection: false,
            keyboardType: TextInputType.text,
            textInputAction: TextInputAction.send,
            autocorrect: false,
            enableSuggestions: false,
            textCapitalization: TextCapitalization.none,
            smartDashesType: SmartDashesType.disabled,
            smartQuotesType: SmartQuotesType.disabled,
            style: const TextStyle(color: Colors.transparent, fontSize: 2),
            cursorColor: Colors.transparent,
            decoration: const InputDecoration.collapsed(hintText: null),
            onSubmitted: (_) {
              _sendBytes(<int>[0x0d]);
              _resetIme();
              _focus.requestFocus();
            },
          ),
        ),
      ),
    );
  }

  bool _atBottom() {
    if (!_scroll.hasClients) return true;
    final position = _scroll.position;
    return position.pixels >= position.maxScrollExtent - _cellH * 2;
  }

  /// 一行交给 [TerminalRowPainter] 逐格画：每格固定在 `i * cellW`，不依赖字体总宽。
  Widget _row(
    List<TermCell> row, {
    int cursorX = -1,
    int selStart = -1,
    int selEnd = -1,
    Color selColor = const Color(0x554C8DFF),
  }) {
    return SizedBox(
      height: _cellH,
      width: double.infinity,
      child: CustomPaint(
        painter: TerminalRowPainter(
          row: row,
          cursorX: cursorX,
          cellW: _cellW,
          cellH: _cellH,
          fontSize: _fontSize,
          lineHeight: _lineHeight,
          fontFamily: _mono.fontFamily,
          defaultFg: KwColors.text,
          cursorBg: KwColors.accent,
          cursorFg: KwColors.bg,
          selStart: selStart,
          selEnd: selEnd,
          selColor: selColor,
        ),
      ),
    );
  }

  /// 快捷命令条：点一下就把整条命令发到远端（末尾带回车）。
  Widget _snippetBar() {
    return SizedBox(
      height: 34,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        itemCount: _snippets.length,
        separatorBuilder: (context, index) => const SizedBox(width: 6),
        itemBuilder: (context, i) {
          final s = _snippets[i];
          return Center(
            child: GestureDetector(
              onTap: () {
                HapticFeedback.lightImpact();
                _sendText(s.cmd);
              },
              child: Container(
                padding: const EdgeInsets.symmetric(horizontal: 10),
                alignment: Alignment.center,
                decoration: BoxDecoration(
                  color: KwColors.accentSoft,
                  borderRadius: BorderRadius.circular(6),
                  border: Border.all(color: KwColors.border),
                ),
                child: Text(
                  s.label,
                  style: TextStyle(fontSize: 11.5, color: KwColors.accent),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}
