/// 手机端终端的网格仿真器（ANSI / VT 子集）。
///
/// 为什么自己写：这台机器取不到现成的 Dart 终端包（pub.dev 被本地拦截，
/// 国内镜像没有 dartxterm），所以按「手机要能跑常规命令」的范围实现一个子集。
///
/// 支持：
///  * 可打印字符（UTF-8 跨块解码）、退格、制表位、CR/LF、延迟自动换行
///  * 中日韩宽字符占两格（续格在取文本时跳过，不会多出空格）
///  * SGR：0/1/2/4/7/21/22/24/27、16 色 / 256 色 / 真彩前景背景、39/49
///  * 光标：A B C D E F G H f d、ESC 7/8 与 CSI s/u 保存恢复、?25h/l 显隐
///  * 清除：ED J（0/1/2/3，3 顺带清空回滚）、EL K（0/1/2）
///  * 行内与行间：@ P 插/删字符，L M 插/删行，S T 滚动
///  * 滚动区域 r、自动换行开关 ?7、DECCKM ?1、备用屏 ?47 / ?1047 / ?1049
///  * ESC D/E/M（索引、新行、反索引）、ESC c 全复位、OSC 0/2 标题、BEL 计数
///
/// 不支持（遇到会安静吃掉，不会把乱码塞进网格）：鼠标上报、左右边距、
/// 超链接、同步输出、iTerm 图片协议、IRM 插入模式。
library;

import 'dart:convert';

/// 一个格子的字形 + 画笔。`rune == 0` 表示宽字符的右半格（占位，不画字）。
class TermCell {
  const TermCell(this.rune, this.pen);

  final int rune;
  final TermPen pen;

  bool get isContinuation => rune == 0;
}

/// 画笔：颜色是 0xRRGGBB，-1 表示「用终端默认色」。
class TermPen {
  const TermPen({
    this.fg = -1,
    this.bg = -1,
    this.bold = false,
    this.dim = false,
    this.underline = false,
    this.inverse = false,
  });

  final int fg;
  final int bg;
  final bool bold;
  final bool dim;
  final bool underline;
  final bool inverse;

  static const TermPen blank = TermPen();

  bool sameStyle(TermPen other) =>
      fg == other.fg &&
      bg == other.bg &&
      bold == other.bold &&
      dim == other.dim &&
      underline == other.underline &&
      inverse == other.inverse;
}

const int _space = 0x20;
const int _historyLimit = 2000;

class TerminalScreen {
  TerminalScreen({int cols = 80, int rows = 24})
    : _cols = cols < 2 ? 2 : cols,
      _rows = rows < 2 ? 2 : rows {
    _main = _blankGrid(_rows, _cols);
    _alt = _blankGrid(_rows, _cols);
    _bottom = _rows - 1;
    _sink = const Utf8Decoder(allowMalformed: true)
        .startChunkedConversion(_RuneSink((int rune) => _parser.feed(rune)));
  }

  late final ByteConversionSink _sink;
  late final _Parser _parser = _Parser(this);

  int _cols;
  int _rows;
  int _top = 0;
  int _bottom = 0;
  int _cx = 0;
  int _cy = 0;

  late List<List<TermCell>> _main;
  late List<List<TermCell>> _alt;
  bool _usingAlt = false;

  TermPen _pen = TermPen.blank;
  bool _cursorVisible = true;
  bool _autoWrap = true;
  bool _wrapPending = false;
  bool _appCursor = false;
  bool _bracketedPaste = false;
  int _bellCount = 0;
  String _title = '';
  (int, int)? _savedCursor;
  (int, int)? _savedAltCursor;
  TermPen _savedPen = TermPen.blank;
  final List<List<TermCell>> _history = [];

  /* ------------------------------------------------------------------ */
  /* 对外读取                                                            */
  /* ------------------------------------------------------------------ */

  int get cols => _cols;
  int get rows => _rows;
  int get cursorX => _cx;
  int get cursorY => _cy;
  bool get cursorVisible => _cursorVisible;
  bool get usingAltScreen => _usingAlt;
  bool get applicationCursorKeys => _appCursor;
  bool get bracketedPaste => _bracketedPaste;
  int get bellCount => _bellCount;
  String get title => _title;
  List<List<TermCell>> get history => List.unmodifiable(_history);

  /// 当前可见网格。
  List<List<TermCell>> get grid => _usingAlt ? _alt : _main;

  /// 可见内容按行拼成的纯文本；宽字符的右半格不产出字符。
  List<String> get textLines => grid.map((row) {
    final buffer = StringBuffer();
    for (final cell in row) {
      if (cell.isContinuation) continue;
      buffer.writeCharCode(cell.rune);
    }
    return buffer.toString();
  }).toList();

  String get text => textLines.join('\n');

  /// 回滚区纯文本，用于「复制全部」。
  String get historyText => _history
      .map((row) {
        final buffer = StringBuffer();
        for (final cell in row) {
          if (cell.isContinuation) continue;
          buffer.writeCharCode(cell.rune);
        }
        return buffer.toString();
      })
      .join('\n');

  /* ------------------------------------------------------------------ */
  /* 输入 / 尺寸                                                         */
  /* ------------------------------------------------------------------ */

  /// 把 PTY 字节流喂进来；UTF-8 半截字节能等下一块补齐。
  void feed(List<int> bytes) => _sink.add(bytes);

  void resize(int cols, int rows) {
    final nextCols = cols < 2 ? 2 : cols;
    final nextRows = rows < 2 ? 2 : rows;
    if (nextCols == _cols && nextRows == _rows) return;
    _cols = nextCols;
    _rows = nextRows;
    _top = 0;
    _bottom = _rows - 1;
    _main = _reflow(_main, toHistory: true);
    _alt = _reflow(_alt, toHistory: false);
    _cx = _cx.clamp(0, _cols - 1);
    _cy = _cy.clamp(0, _rows - 1);
    _wrapPending = false;
  }

  List<List<TermCell>> _reflow(
    List<List<TermCell>> source, {
    required bool toHistory,
  }) {
    final shaped = source
        .map((row) => _reshapeRow(row, _cols))
        .toList(growable: true);
    while (shaped.length > _rows) {
      final popped = shaped.removeAt(0);
      if (toHistory) _pushHistory(popped);
    }
    while (shaped.length < _rows) {
      shaped.add(_blankRow(_cols));
    }
    return shaped;
  }

  /* ------------------------------------------------------------------ */
  /* 解析器回调的内部动作                                                */
  /* ------------------------------------------------------------------ */

  List<TermCell> get _row => grid[_cy];

  void _putRune(int rune) {
    if (_wrapPending && _autoWrap) {
      _wrapPending = false;
      _cx = 0;
      _lineFeed();
    }
    if (_cx >= _cols) return; // 关自动换行时，超出行尾的字符直接吃掉
    final row = _row;
    row[_cx] = TermCell(rune, _pen);
    if (isWideRune(rune) && _cx + 1 < _cols) {
      row[_cx + 1] = TermCell(0, _pen);
      _cx += 2;
      if (_cx >= _cols) {
        _cx = _cols - 1;
        _wrapPending = _autoWrap;
      }
      return;
    }
    if (_cx == _cols - 1) {
      _wrapPending = _autoWrap;
      return;
    }
    _cx++;
  }

  void _carriageReturn() {
    _cx = 0;
    _wrapPending = false;
  }

  void _backspace() {
    _wrapPending = false;
    if (_cx > 0) _cx--;
  }

  void _tab() {
    _wrapPending = false;
    final next = ((_cx ~/ 8) + 1) * 8;
    _cx = next < _cols ? next : _cols - 1;
  }

  void _lineFeed({bool toHome = false}) {
    _wrapPending = false;
    if (_cy == _bottom) {
      _scrollUp(1);
    } else if (_cy < _rows - 1) {
      _cy++;
    }
    if (toHome) _cx = 0;
  }

  void _reverseIndex() {
    _wrapPending = false;
    if (_cy == _top) {
      _scrollDown(1);
    } else if (_cy > 0) {
      _cy--;
    }
  }

  /// 区域内容向上滚，主屏滚出的行进回滚区。
  void _scrollUp(int n) {
    final area = grid;
    for (var i = 0; i < n; i++) {
      final popped = area.removeAt(_top);
      area.insert(_bottom.clamp(0, area.length), _blankRow(_cols));
      if (!_usingAlt) _pushHistory(popped);
    }
  }

  void _scrollDown(int n) {
    final area = grid;
    for (var i = 0; i < n; i++) {
      area.removeAt(_bottom.clamp(0, area.length - 1));
      area.insert(_top, _blankRow(_cols));
    }
  }

  void _pushHistory(List<TermCell> row) {
    _history.add(row);
    if (_history.length > _historyLimit) {
      _history.removeRange(0, _history.length - _historyLimit);
    }
  }

  void _moveTo(int x, int y) {
    _cx = x.clamp(0, _cols - 1);
    _cy = y.clamp(0, _rows - 1);
    _wrapPending = false;
  }

  void _moveCursorX(int delta) => _moveTo(_cx + delta, _cy);

  void _setPen(TermPen pen) => _pen = pen;

  TermPen get pen => _pen;
  int get topMargin => _top;
  int get bottomMargin => _bottom;

  TermCell get _blank => TermCell(_space, _pen);

  void _insertChars(int count) {
    final row = _row;
    final shifted = row.sublist(_cx).toList();
    for (var i = 0; i < count; i++) {
      shifted.insert(0, _blank);
    }
    for (var x = _cx; x < _cols; x++) {
      row[x] = shifted[x - _cx];
    }
  }

  void _deleteChars(int count) {
    final row = _row;
    final shifted = row.sublist(_cx).toList();
    for (var i = 0; i < count && shifted.isNotEmpty; i++) {
      shifted.removeAt(0);
    }
    while (shifted.length < _cols - _cx) {
      shifted.add(_blank);
    }
    for (var x = _cx; x < _cols; x++) {
      row[x] = shifted[x - _cx];
    }
  }

  void _insertLines(int count) {
    if (_cy < _top || _cy > _bottom) return;
    final area = grid;
    for (var i = 0; i < count; i++) {
      area.insert(_cy, _blankRow(_cols));
      area.removeAt(_bottom + 1);
    }
    _cx = 0;
    _wrapPending = false;
  }

  void _deleteLines(int count) {
    if (_cy < _top || _cy > _bottom) return;
    final area = grid;
    for (var i = 0; i < count; i++) {
      area.removeAt(_cy);
      area.insert(_bottom, _blankRow(_cols));
    }
    _cx = 0;
    _wrapPending = false;
  }

  void _eraseInLine(int mode) {
    final row = _row;
    switch (mode) {
      case 0:
        for (var x = _cx; x < _cols; x++) {
          row[x] = _blank;
        }
      case 1:
        for (var x = 0; x <= _cx && x < _cols; x++) {
          row[x] = _blank;
        }
      default:
        for (var x = 0; x < _cols; x++) {
          row[x] = _blank;
        }
    }
  }

  /// ED 不动光标（`clear` 会另外发 ESC[H）；模式 3 顺带清空回滚。
  void _eraseDisplay(int mode) {
    switch (mode) {
      case 0:
        _eraseInLine(0);
        for (var y = _cy + 1; y < _rows; y++) {
          _fillRow(y);
        }
      case 1:
        _eraseInLine(1);
        for (var y = 0; y < _cy; y++) {
          _fillRow(y);
        }
      default:
        for (var y = 0; y < _rows; y++) {
          _fillRow(y);
        }
        if (mode == 3) _history.clear();
    }
  }

  void _fillRow(int y) {
    final row = grid[y];
    for (var x = 0; x < _cols; x++) {
      row[x] = _blank;
    }
  }

  void _setScrollRegion(int topOneBased, int bottomOneBased) {
    final top = (topOneBased - 1).clamp(0, _rows - 1);
    final bottom = (bottomOneBased - 1).clamp(0, _rows - 1);
    if (bottom <= top) {
      _top = 0;
      _bottom = _rows - 1;
    } else {
      _top = top;
      _bottom = bottom;
    }
    _moveTo(0, _top);
  }

  void _enterAlt() {
    if (_usingAlt) return;
    _savedAltCursor = (_cx, _cy);
    _savedPen = _pen;
    _usingAlt = true;
    _alt = _blankGrid(_rows, _cols);
    _top = 0;
    _bottom = _rows - 1;
    _cx = 0;
    _cy = 0;
    _wrapPending = false;
  }

  void _exitAlt() {
    if (!_usingAlt) return;
    _usingAlt = false;
    _alt = _blankGrid(_rows, _cols);
    final saved = _savedAltCursor;
    if (saved != null) {
      _cx = saved.$1.clamp(0, _cols - 1);
      _cy = saved.$2.clamp(0, _rows - 1);
    }
    _pen = _savedPen;
    _top = 0;
    _bottom = _rows - 1;
    _wrapPending = false;
  }

  void _resetAll() {
    _pen = TermPen.blank;
    _cursorVisible = true;
    _autoWrap = true;
    _appCursor = false;
    _bracketedPaste = false;
    _top = 0;
    _bottom = _rows - 1;
    _cx = 0;
    _cy = 0;
    _wrapPending = false;
    _savedCursor = null;
    _savedAltCursor = null;
    if (_usingAlt) {
      _usingAlt = false;
      _alt = _blankGrid(_rows, _cols);
    } else {
      _main = _blankGrid(_rows, _cols);
    }
  }

  void _setCursorVisible(bool visible) => _cursorVisible = visible;

  void _setAutoWrap(bool enabled) {
    _autoWrap = enabled;
    _wrapPending = false;
  }

  void _setAppCursor(bool enabled) => _appCursor = enabled;
  void _setBracketedPaste(bool enabled) => _bracketedPaste = enabled;
  void _ringBell() => _bellCount++;
  void _setTitle(String value) => _title = value;
  void _saveCursor() => _savedCursor = (_cx, _cy);

  void _restoreCursor() {
    final saved = _savedCursor;
    if (saved != null) _moveTo(saved.$1, saved.$2);
  }
}

/// 中日韩与常用 emoji 视为双宽；其它按单宽。
bool isWideRune(int rune) {
  if (rune < 0x1100) return false;
  const ranges = <(int, int)>[
    (0x1100, 0x115f),
    (0x2e80, 0x303e),
    (0x3041, 0x33ff),
    (0x3400, 0x4dbf),
    (0x4e00, 0x9fff),
    (0xa000, 0xa4cf),
    (0xac00, 0xd7a3),
    (0xf900, 0xfaff),
    (0xfe30, 0xfe4f),
    (0xff00, 0xff60),
    (0xffe0, 0xffe6),
    (0x1f300, 0x1faff),
    (0x20000, 0x3fffd),
  ];
  for (final range in ranges) {
    if (rune >= range.$1 && rune <= range.$2) return true;
  }
  return false;
}

List<List<TermCell>> _blankGrid(int rows, int cols) =>
    List.generate(rows, (_) => _blankRow(cols), growable: true);

List<TermCell> _blankRow(int cols) => List<TermCell>.filled(
  cols,
  const TermCell(_space, TermPen.blank),
  growable: true,
);

List<TermCell> _reshapeRow(List<TermCell> row, int cols) {
  if (row.length == cols) return row;
  final out = List<TermCell>.from(row);
  if (out.length > cols) {
    out.removeRange(cols, out.length);
  } else {
    while (out.length < cols) {
      out.add(const TermCell(_space, TermPen.blank));
    }
  }
  return out;
}

/* ------------------------------------------------------------------ */
/* 解析器                                                              */
/* ------------------------------------------------------------------ */

enum _State { ground, escape, csi, osc, charset }

class _Parser {
  _Parser(this._s);

  final TerminalScreen _s;
  _State _state = _State.ground;
  final List<int> _params = <int>[];
  int _current = 0;
  bool _hasDigit = false;
  bool _private = false;
  bool _ignore = false;
  final StringBuffer _osc = StringBuffer();
  bool _oscSawEscape = false;

  void feed(int code) {
    switch (_state) {
      case _State.ground:
        _ground(code);
      case _State.escape:
        _escape(code);
      case _State.charset:
        // ESC ( B 之类：字符集指定，这里一律按默认字体吃掉
        _state = _State.ground;
      case _State.csi:
        _csi(code);
      case _State.osc:
        _oscFeed(code);
    }
  }

  void _ground(int code) {
    switch (code) {
      case 0x07:
        _s._ringBell();
      case 0x08:
        _s._backspace();
      case 0x09:
        _s._tab();
      case 0x0a || 0x0b || 0x0c:
        _s._lineFeed();
      case 0x0d:
        _s._carriageReturn();
      case 0x1b:
        _state = _State.escape;
      case 0x00:
        break;
      default:
        if (code < 0x20 || code == 0x7f) return;
        _s._putRune(code);
    }
  }

  void _escape(int code) {
    if (code == 0x1b) {
      _state = _State.escape; // 连打的 ESC：丢掉前一个
      return;
    }
    if (code == 0x18 || code == 0x1a) {
      _reset();
      return;
    }
    switch (code) {
      case 0x5b: // [
        _state = _State.csi;
        _params.clear();
        _current = 0;
        _hasDigit = false;
        _private = false;
        _ignore = false;
      case 0x5d: // ]
        _state = _State.osc;
        _osc.clear();
        _oscSawEscape = false;
      case 0x28: // (
      case 0x29: // )
      case 0x2a: // *
      case 0x2b: // +
        _state = _State.charset;
      case 0x44: // D 索引
        _s._lineFeed();
        _reset();
      case 0x45: // E 新行
        _s._lineFeed(toHome: true);
        _reset();
      case 0x4d: // M 反索引
        _s._reverseIndex();
        _reset();
      case 0x37: // 7 保存光标
        _s._saveCursor();
        _reset();
      case 0x38: // 8 恢复光标
        _s._restoreCursor();
        _reset();
      case 0x63: // c 全复位
        _s._resetAll();
        _reset();
      case 0x3d: // =
      case 0x3e: // >
        _reset();
      default:
        _reset();
    }
  }

  void _csi(int code) {
    if (code == 0x1b) {
      _state = _State.escape;
      return;
    }
    if (code == 0x18 || code == 0x1a) {
      _reset();
      return;
    }
    if (code >= 0x30 && code <= 0x39) {
      _current = _current * 10 + (code - 0x30);
      _hasDigit = true;
      return;
    }
    if (code == 0x3b || code == 0x3a) {
      // 分隔符：把当前累计值（没数字就是 0）结算成一个参数
      _params.add(_current);
      _current = 0;
      _hasDigit = false;
      return;
    }
    if (code >= 0x3c && code <= 0x3f) {
      // < = > ? 私有/扩展前缀（CSI >4;1m 这类）：整条只吃掉，不执行
      _private = true;
      return;
    }
    if (code >= 0x20 && code <= 0x2f) {
      // 中间字节（如 CSI ! p、CSI $ q）：整条按不支持吃掉
      _ignore = true;
      return;
    }
    if (code >= 0x40 && code <= 0x7e) {
      if (_hasDigit || _params.isNotEmpty) _params.add(_current);
      if (_ignore) {
        _reset();
      } else {
        _finishCsi(String.fromCharCode(code));
      }
      return;
    }
    _reset();
  }

  /// 取参数；未给出或给了 0 都返回 fallback（VT 的惯例）。
  int _p(int index, int fallback) =>
      index < _params.length && _params[index] != 0 ? _params[index] : fallback;

  void _finishCsi(String finalByte) {
    if (_private) {
      _decPrivate(finalByte);
      _reset();
      return;
    }
    switch (finalByte) {
      case 'm':
        // CSI m 不带参数等价于 CSI 0m（全部复位）
        _s._setPen(_applySgr(_params.isEmpty ? const <int>[0] : _params));
      case 'H' || 'f':
        _s._moveTo(_p(1, 1) - 1, _p(0, 1) - 1);
      case 'A':
        final n = _p(0, 1);
        _s._moveTo(
          _s.cursorX,
          n > _s.cursorY - _s._top ? _s._top : _s.cursorY - n,
        );
      case 'B':
        final n = _p(0, 1);
        final room = _s._bottom - _s.cursorY;
        _s._moveTo(_s.cursorX, n > room ? _s._bottom : _s.cursorY + n);
      case 'C':
        _s._moveCursorX(_p(0, 1));
      case 'D':
        _s._moveCursorX(-_p(0, 1));
      case 'E':
        _s._moveTo(0, _s.cursorY + _p(0, 1));
      case 'F':
        _s._moveTo(0, _s.cursorY - _p(0, 1));
      case 'G':
        _s._moveTo(_p(0, 1) - 1, _s.cursorY);
      case 'd':
        _s._moveTo(_s.cursorX, _p(0, 1) - 1);
      case 'J':
        _s._eraseDisplay(_p(0, 0));
      case 'K':
        _s._eraseInLine(_p(0, 0));
      case '@':
        _s._insertChars(_p(0, 1));
      case 'P':
        _s._deleteChars(_p(0, 1));
      case 'L':
        _s._insertLines(_p(0, 1));
      case 'M':
        _s._deleteLines(_p(0, 1));
      case 'S':
        _s._scrollUp(_p(0, 1));
      case 'T':
        _s._scrollDown(_p(0, 1));
      case 'r':
        _s._setScrollRegion(_p(0, 1), _p(1, _s.rows));
      case 's':
        _s._saveCursor();
      case 'u':
        _s._restoreCursor();
      case 'h':
      case 'l':
        break; // 非私有模式（IRM 等）不支持
      default:
        break;
    }
    _reset();
  }

  void _decPrivate(String finalByte) {
    if (finalByte != 'h' && finalByte != 'l') return;
    final on = finalByte == 'h';
    for (final number in _params) {
      switch (number) {
        case 1:
          _s._setAppCursor(on);
        case 7:
          _s._setAutoWrap(on);
        case 25:
          _s._setCursorVisible(on);
        case 2004:
          _s._setBracketedPaste(on);
        case 47 || 1047 || 1049:
          if (on) {
            _s._enterAlt();
          } else {
            _s._exitAlt();
          }
        default:
          break;
      }
    }
  }

  TermPen _applySgr(List<int> list) {
    var pen = _s._pen;
    var i = 0;
    while (i < list.length) {
      final code = list[i];
      switch (code) {
        case 0:
          pen = TermPen.blank;
        case 1:
          pen = _style(pen, bold: true);
        case 2:
          pen = _style(pen, dim: true);
        case 4:
          pen = _style(pen, underline: true);
        case 7:
          pen = _style(pen, inverse: true);
        case 21 || 22:
          pen = _style(pen, bold: false, dim: false);
        case 24:
          pen = _style(pen, underline: false);
        case 27:
          pen = _style(pen, inverse: false);
        case 39:
          pen = _fg(pen, -1);
        case 49:
          pen = _bg(pen, -1);
        case 38 || 48:
          final kind = i + 1 < list.length ? list[i + 1] : -1;
          if (kind == 5 && i + 2 < list.length) {
            final color = palette(list[i + 2]);
            pen = code == 38 ? _fg(pen, color) : _bg(pen, color);
            i += 2;
          } else if (kind == 2 && i + 4 < list.length) {
            final color =
                ((list[i + 2] & 0xff) << 16) |
                ((list[i + 3] & 0xff) << 8) |
                (list[i + 4] & 0xff);
            pen = code == 38 ? _fg(pen, color) : _bg(pen, color);
            i += 4;
          }
        case >= 30 && <= 37:
          pen = _fg(pen, palette(code - 30));
        case >= 90 && <= 97:
          pen = _fg(pen, palette(code - 90 + 8));
        case >= 40 && <= 47:
          pen = _bg(pen, palette(code - 40));
        case >= 100 && <= 107:
          pen = _bg(pen, palette(code - 100 + 8));
        default:
          break;
      }
      i++;
    }
    return pen;
  }

  static TermPen _style(
    TermPen pen, {
    bool? bold,
    bool? dim,
    bool? underline,
    bool? inverse,
  }) => TermPen(
    fg: pen.fg,
    bg: pen.bg,
    bold: bold ?? pen.bold,
    dim: dim ?? pen.dim,
    underline: underline ?? pen.underline,
    inverse: inverse ?? pen.inverse,
  );

  static TermPen _fg(TermPen pen, int fg) => TermPen(
    fg: fg,
    bg: pen.bg,
    bold: pen.bold,
    dim: pen.dim,
    underline: pen.underline,
    inverse: pen.inverse,
  );

  static TermPen _bg(TermPen pen, int bg) => TermPen(
    fg: pen.fg,
    bg: bg,
    bold: pen.bold,
    dim: pen.dim,
    underline: pen.underline,
    inverse: pen.inverse,
  );

  void _oscFeed(int code) {
    if (code == 0x07) {
      _finishOsc();
      return;
    }
    if (_oscSawEscape) {
      _oscSawEscape = false;
      if (code == 0x5c) {
        // ESC \ 是 OSC 的 ST 收尾
        _finishOsc();
        return;
      }
      _osc.writeCharCode(0x1b);
    }
    if (code == 0x1b) {
      _oscSawEscape = true;
      return;
    }
    if (_osc.length < 512) _osc.writeCharCode(code);
  }

  void _finishOsc() {
    final value = _osc.toString();
    _osc.clear();
    _state = _State.ground;
    final sep = value.indexOf(';');
    if (sep < 0) return;
    final id = value.substring(0, sep);
    if (id == '0' || id == '2') _s._setTitle(value.substring(sep + 1));
  }

  void _reset() {
    _state = _State.ground;
    _params.clear();
    _current = 0;
    _hasDigit = false;
    _private = false;
    _ignore = false;
  }
}

/// 256 色 → 0xRRGGBB。0-15 固定色板，16-231 是 6x6x6，232-255 灰阶。
int palette(int index) {
  const basic = <int>[
    0x6c7079,
    0xe06b6b,
    0x5fbf7a,
    0xd9b45a,
    0x5a9de0,
    0xb07ad9,
    0x4fbfbf,
    0xc9ccd1,
    0x8b9099,
    0xf08a8a,
    0x7ad994,
    0xe8c96b,
    0x7ab3f0,
    0xc994f0,
    0x7ae0e0,
    0xf2f4f8,
  ];
  if (index >= 0 && index < 16) return basic[index];
  if (index >= 16 && index < 232) {
    final value = index - 16;
    final r = value ~/ 36;
    final g = (value % 36) ~/ 6;
    final b = value % 6;
    int channel(int x) => x == 0 ? 0 : 55 + x * 40;
    return (channel(r) << 16) | (channel(g) << 8) | channel(b);
  }
  if (index >= 232 && index < 256) {
    final level = 8 + (index - 232) * 10;
    return (level << 16) | (level << 8) | level;
  }
  return basic[7];
}

/// UTF-8 解码结果直接按码点交给解析器；半截字节由 `Utf8Decoder` 自己等下一块。
class _RuneSink implements Sink<String> {
  _RuneSink(this._onRune);

  final void Function(int rune) _onRune;

  @override
  void add(String text) {
    for (final rune in text.runes) {
      _onRune(rune);
    }
  }

  @override
  void close() {}
}
