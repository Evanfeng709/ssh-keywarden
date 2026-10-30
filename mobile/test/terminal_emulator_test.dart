import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:kw_mobile/src/terminal_emulator.dart';

TerminalScreen grid({int cols = 20, int rows = 6}) =>
    TerminalScreen(cols: cols, rows: rows);

void type(TerminalScreen screen, String text) => screen.feed(utf8.encode(text));

String lineAt(TerminalScreen screen, int y) => screen.textLines[y];

String trimmed(TerminalScreen screen, int y) => lineAt(screen, y).trimRight();

void main() {
  test('可打印字符写入网格，满行后延迟换行', () {
    final screen = grid(cols: 10, rows: 3);
    type(screen, '0123456789ab');
    expect(lineAt(screen, 0), '0123456789');
    expect(lineAt(screen, 1), 'ab'.padRight(10));
    expect(screen.cursorX, 2);
    expect(screen.cursorY, 1);
  });

  test('恰好填满一行时光标停在行尾，下一个字符才换行', () {
    final screen = grid(cols: 4, rows: 2);
    type(screen, 'abcd');
    expect(screen.cursorX, 3);
    expect(screen.cursorY, 0);
    type(screen, 'e');
    expect(lineAt(screen, 1), 'e'.padRight(4));
    expect(screen.cursorY, 1);
  });

  test('CR / LF / BS / TAB 的基本光标动作', () {
    final screen = grid(cols: 20, rows: 4);
    type(screen, 'hello\rHELLO\r\nworld');
    expect(trimmed(screen, 0), 'HELLO');
    expect(trimmed(screen, 1), 'world');

    type(screen, '\b!');
    expect(trimmed(screen, 1), 'worl!');

    // LF 只下移不回行首，这是 VT 语义；程序要回首得自己发 CR。
    type(screen, '\n');
    expect((screen.cursorX, screen.cursorY), (5, 2));

    final tabs = grid(cols: 20, rows: 2);
    type(tabs, 'a\tb\tc');
    expect(lineAt(tabs, 0).indexOf('a'), 0);
    expect(lineAt(tabs, 0).indexOf('b'), 8);
    expect(lineAt(tabs, 0).indexOf('c'), 16);
  });

  test('SGR：颜色、粗体、反显与复位', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, '\x1B[31mR\x1B[0mP');
    expect(screen.grid[0][0].pen.fg, palette(1));
    expect(screen.grid[0][0].rune, 'R'.codeUnitAt(0));
    expect(screen.grid[0][1].pen, sameAs(TermPen.blank));

    type(screen, '\x1B[1;4m\x1B[H\x1B[2J');
    type(screen, '\x1B[1mB\x1B[24mU\x1B[7mI\x1B[27mN');
    expect(screen.grid[0][0].pen.bold, isTrue);
    expect(screen.grid[0][0].pen.underline, isTrue, reason: '24 之前写的字符仍有下划线');
    expect(screen.grid[0][1].pen.underline, isFalse, reason: '24 之后的字符没有下划线');
    expect(screen.grid[0][2].pen.inverse, isTrue);
    expect(screen.grid[0][3].pen.inverse, isFalse);
  });

  test('SGR：256 色、真彩与默认色 39/49', () {
    final screen = grid(cols: 10, rows: 2);
    type(
      screen,
      '\x1B[38;5;196mX\x1B[48;5;21m\x1B[38;2;10;20;30mY\x1B[39;49mZ',
    );
    expect(screen.grid[0][0].pen.fg, palette(196));
    expect(screen.grid[0][1].pen.fg, 0x0A141E);
    expect(screen.grid[0][1].pen.bg, palette(21));
    expect(screen.grid[0][2].pen.fg, -1);
    expect(screen.grid[0][2].pen.bg, -1);
  });

  test('EL 三种模式只动当前行', () {
    final screen = grid(cols: 8, rows: 2);
    type(screen, 'abcdefgh');
    type(screen, '\x1B[3G\x1B[K');
    expect(lineAt(screen, 0), 'ab${' ' * 6}');

    type(screen, '\x1B[2K');
    expect(lineAt(screen, 0), ' ' * 8);

    type(screen, '\x1B[H12345678\x1B[4G\x1B[1K');
    expect(lineAt(screen, 0), ' ' * 4 + '5678');
  });

  test('ED 不清光标所在行的前半部分，也不搬光标', () {
    final screen = grid(cols: 8, rows: 3);
    type(screen, 'line1\r\nline2\r\nline3');
    type(screen, '\x1B[2;3H\x1B[J');
    expect(trimmed(screen, 0), 'line1');
    expect(lineAt(screen, 1), 'li${' ' * 6}');
    expect(lineAt(screen, 2), ' ' * 8);
    expect(screen.cursorX, 2);
    expect(screen.cursorY, 1);

    type(screen, '\x1B[2;3H\x1B[1J');
    expect(lineAt(screen, 0), ' ' * 8);
    expect(trimmed(screen, 1), '');
    expect((screen.cursorX, screen.cursorY), (2, 1), reason: 'ED 1 也不搬光标');
  });

  test('ED 3 清空回滚，ED 2 不会', () {
    final screen = grid(cols: 8, rows: 2);
    for (var i = 0; i < 4; i++) {
      type(screen, 'row$i\r\n');
    }
    expect(screen.history, isNotEmpty);
    type(screen, '\x1B[2J');
    expect(screen.history, isNotEmpty);
    type(screen, '\x1B[3J');
    expect(screen.history, isEmpty);
  });

  test('光标定位与移动按边界夹住', () {
    final screen = grid(cols: 10, rows: 5);
    type(screen, '\x1B[3;5H');
    expect((screen.cursorX, screen.cursorY), (4, 2));
    type(screen, '\x1B[H');
    expect((screen.cursorX, screen.cursorY), (0, 0));
    type(screen, '\x1B[99;99H');
    expect((screen.cursorX, screen.cursorY), (9, 4));
    type(screen, '\x1B[A\x1B[A\x1B[D\x1B[D');
    expect((screen.cursorX, screen.cursorY), (7, 2));
    type(screen, '\x1B[99D\x1B[99A');
    expect((screen.cursorX, screen.cursorY), (0, 0));
    type(screen, '\x1B[5G');
    expect(screen.cursorX, 4);
    type(screen, '\x1B[4d');
    expect(screen.cursorY, 3);
    type(screen, '\x1B[E');
    expect((screen.cursorX, screen.cursorY), (0, 4));
    type(screen, '\x1B[F');
    expect((screen.cursorX, screen.cursorY), (0, 3));
  });

  test('滚动区外的换行只下移，滚到底部才挤行进回滚', () {
    final screen = grid(cols: 6, rows: 3);
    type(screen, 'aaa\r\nbbb\r\nccc');
    expect(screen.history, isEmpty);
    type(screen, '\n');
    expect(trimmed(screen, 0), 'bbb');
    expect(trimmed(screen, 2), '');
    expect(
      screen.history.map((row) => String.fromCharCodes(row.map((c) => c.rune))),
      contains('aaa   '),
    );
  });

  test('DECSTBM 划定滚动区域，区域内滚动不影响区域外', () {
    final screen = grid(cols: 6, rows: 4);
    type(screen, 'top\r\n');
    type(screen, '\x1B[2;3r');
    type(screen, 'aaa\r\nbbb');
    expect(trimmed(screen, 0), 'top');
    expect(trimmed(screen, 3), '');
    type(screen, '\r\n');
    expect(trimmed(screen, 1), 'bbb');
    expect(trimmed(screen, 3), '');
    expect(trimmed(screen, 0), 'top');
  });

  test('IL / DL 在滚动区内插入与删除行', () {
    final screen = grid(cols: 4, rows: 3);
    type(screen, '1\r\n2\r\n3');
    type(screen, '\x1B[2;1H\x1B[1L');
    expect(trimmed(screen, 0), '1');
    expect(trimmed(screen, 1), '');
    expect(trimmed(screen, 2), '2');
    type(screen, '\x1B[2;1H\x1B[1M');
    expect(trimmed(screen, 1), '2');
    expect(trimmed(screen, 2), '');
  });

  test('ICP / DCP 插入与删除字符，右侧被挤掉', () {
    final screen = grid(cols: 6, rows: 2);
    type(screen, 'abcdef');
    type(screen, '\x1B[2G\x1B[2@');
    expect(lineAt(screen, 0).trimRight(), 'a  bcd');
    expect(lineAt(screen, 0).length, 6);
    type(screen, '\x1B[2G\x1B[2P');
    expect(lineAt(screen, 0).trimRight(), 'abcd');
  });

  test('SU / SD 整屏上下滚', () {
    final screen = grid(cols: 4, rows: 3);
    type(screen, '1\r\n2\r\n3');
    type(screen, '\x1B[S');
    expect(trimmed(screen, 0), '2');
    expect(trimmed(screen, 1), '3');
    expect(trimmed(screen, 2), '');
    type(screen, '\x1B[T');
    expect(trimmed(screen, 0), '');
    expect(trimmed(screen, 1), '2');
    expect(trimmed(screen, 2), '3');
  });

  test('备用屏进出：主屏内容不动，回滚不被污染', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, 'main-line');
    final cursorBefore = (screen.cursorX, screen.cursorY);
    type(screen, '\x1B[?1049h');
    expect(screen.usingAltScreen, isTrue);
    expect(screen.text, '${' ' * 10}\n${' ' * 10}');
    type(screen, 'alt');
    expect(screen.history, isEmpty);
    type(screen, '\x1B[?1049l');
    expect(screen.usingAltScreen, isFalse);
    expect(trimmed(screen, 0), 'main-line');
    expect((screen.cursorX, screen.cursorY), cursorBefore);
  });

  test('宽字符占两格，取文本不会多出空格', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, '中文ab');
    expect(trimmed(screen, 0), '中文ab');
    expect(screen.cursorX, 6);
    expect(screen.grid[0][1].isContinuation, isTrue);
    expect(screen.grid[0][0].pen.sameStyle(screen.grid[0][1].pen), isTrue);
  });

  test('UTF-8 半个字节能等下一块补齐', () {
    final screen = grid(cols: 10, rows: 2);
    final bytes = utf8.encode('好x');
    screen.feed(bytes.sublist(0, 2));
    expect(screen.textLines[0].trimRight(), '');
    screen.feed(bytes.sublist(2));
    expect(trimmed(screen, 0), '好x');
  });

  test('OSC 标题支持 BEL 与 ST 两种收尾', () {
    final screen = grid();
    type(screen, '\x1B]0;first title\x07');
    expect(screen.title, 'first title');
    type(screen, '\x1B]2;second\u001B\\');
    expect(screen.title, 'second');
    type(screen, '\x1B]52;c;clipboard\u0007');
    expect(screen.title, 'second', reason: '剪贴板 OSC 不该改标题');
  });

  test('BEL 计数，未知控制字符不吃掉后面的文本', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, 'a\u0007b');
    expect(screen.bellCount, 1);
    expect(trimmed(screen, 0), 'ab');
    type(screen, '\x00\x01\x02');
    expect(trimmed(screen, 0), 'ab');
  });

  test('私有模式：光标显隐、自动换行、DECCKM、括号粘贴', () {
    final screen = grid();
    type(screen, '\x1B[?25l');
    expect(screen.cursorVisible, isFalse);
    type(screen, '\x1B[?25h');
    expect(screen.cursorVisible, isTrue);
    type(screen, '\x1B[?1h\x1B[?2004h');
    expect(screen.applicationCursorKeys, isTrue);
    expect(screen.bracketedPaste, isTrue);
    type(screen, '\x1B[?1l\x1B[?2004l');
    expect(screen.applicationCursorKeys, isFalse);
    expect(screen.bracketedPaste, isFalse);

    final nowrap = grid(cols: 4, rows: 2);
    type(nowrap, '\x1B[?7l');
    type(nowrap, 'abcdef');
    // 关掉自动换行后光标卡在最后一格，后续字符覆盖它（xterm 行为）
    expect(lineAt(nowrap, 0), 'abcf');
    expect(lineAt(nowrap, 1), '    ');
    type(nowrap, '\x1B[?7h');
    type(nowrap, '\r\nxy');
    expect(trimmed(nowrap, 1), 'xy');
  });

  test('ESC 7 / ESC 8 与 CSI s / u 保存恢复光标', () {
    final screen = grid(cols: 10, rows: 4);
    type(screen, '\x1B[2;3H\x1B7');
    type(screen, '\x1B[1;1H');
    expect((screen.cursorX, screen.cursorY), (0, 0));
    type(screen, '\x1B8');
    expect((screen.cursorX, screen.cursorY), (2, 1));
    type(screen, '\x1B[1;1H\x1B[s\x1B[4;4H\x1B[u');
    expect((screen.cursorX, screen.cursorY), (0, 0));
  });

  test('ESC M 在区域顶部反索引，ESC E 换行并回行首', () {
    final screen = grid(cols: 4, rows: 2);
    type(screen, '1\r\n2');
    type(screen, '\x1B[1;1H\x1BM');
    expect(trimmed(screen, 0), '');
    expect(trimmed(screen, 1), '1');
    type(screen, '\x1B[1;1H\x1BEabc');
    expect(trimmed(screen, 0), '');
    expect(trimmed(screen, 1), 'abc');
  });

  test('未知 CSI 被吃掉，不会留下乱码', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, 'a\x1B[?1000;1006h b\x1B[>4;1m c\x1B[?2026h d');
    expect(trimmed(screen, 0), 'a b c d');
  });

  test('字符集指定与安全复位', () {
    final screen = grid(cols: 10, rows: 2);
    type(screen, '\x1B(B\x1B)0abc');
    expect(trimmed(screen, 0), 'abc');
    type(screen, '\x1B[1;1H\x1B[31mX\x1Bc');
    expect(trimmed(screen, 0), '');
    expect(screen.grid[0][0].pen.fg, -1);
    expect((screen.cursorX, screen.cursorY), (0, 0));
  });

  test('改尺寸：窄化截断、变矮挤进回滚、变高补空行', () {
    final screen = grid(cols: 8, rows: 3);
    type(screen, 'aaa\r\nbbb\r\nccc');
    screen.resize(4, 2);
    expect(screen.cols, 4);
    expect(screen.rows, 2);
    expect(screen.textLines, ['bbb ', 'ccc ']);
    expect(screen.history.length, 1);

    screen.resize(8, 4);
    expect(screen.textLines, ['bbb     ', 'ccc     ', '        ', '        ']);
  });

  test('改尺寸后光标不会跑到网格外', () {
    final screen = grid(cols: 20, rows: 4);
    type(screen, '\x1B[4;20H');
    screen.resize(6, 2);
    expect((screen.cursorX, screen.cursorY), (5, 1));
  });

  test('回滚区按上限截断，最旧的先掉', () {
    final screen = grid(cols: 6, rows: 2);
    for (var i = 0; i < _historyProbe; i++) {
      type(screen, '$i\r\n');
    }
    final lines = screen.history
        .map(
          (row) =>
              String.fromCharCodes(row.map((cell) => cell.rune)).trimRight(),
        )
        .toList();
    expect(lines.length, 2000);
    expect(lines.first, isNot('0'));
    expect(int.parse(lines.last), greaterThan(int.parse(lines.first)));
  });
}

/// 只要超过回滚上限就能验证截断；行数控制在这里，避免测试跑太久。
const int _historyProbe = 2040;

Matcher sameAs(TermPen pen) =>
    predicate<TermPen>((value) => value.sameStyle(pen), '同样式');
