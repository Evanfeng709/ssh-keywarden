import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kw_mobile/src/terminal_emulator.dart';
import 'package:kw_mobile/src/terminal_painter.dart';

void main() {
  group('行模型：最后一列不能被吃掉', () {
    test('41 列 ASCII 正好占满一行，末字符保留', () {
      final screen = TerminalScreen(cols: 41, rows: 3);
      screen.feed(utf8.encode('x' * 41));
      final line = screen.textLines[0];
      expect(line.length, 41, reason: '41 个字符应恰好占满 41 列');
      expect(line[40], 'x', reason: '第 41 列不能被截断（截图上 from->fro 的问题）');
      expect(screen.grid[0].length, 41);
    });

    test('宽字符占两格：第二格是续格', () {
      final screen = TerminalScreen(cols: 10, rows: 2);
      screen.feed(utf8.encode('你'));
      expect(screen.grid[0][0].rune, 0x4f60);
      expect(screen.grid[0][0].isContinuation, isFalse);
      expect(screen.grid[0][1].isContinuation, isTrue);
      // 取文本时不重复产出：一行里只有一个「你」
      expect(screen.textLines[0].startsWith('你'), isTrue);
    });
  });

  group('逐格渲染：不抛异常、光标可画', () {
    Future<List<TermCell>> rowOf(String text) async {
      final screen = TerminalScreen(cols: 41, rows: 2);
      screen.feed(utf8.encode(text));
      return screen.grid[0];
    }

    Widget harness(List<TermCell> row, {int cursorX = -1}) {
      return MaterialApp(
        home: Center(
          child: SizedBox(
            width: 41 * 8.0,
            height: 18,
            child: CustomPaint(
              painter: TerminalRowPainter(
                row: row,
                cursorX: cursorX,
                cellW: 8,
                cellH: 18,
                fontSize: 13,
                lineHeight: 1.3,
                fontFamily: 'monospace',
                defaultFg: const Color(0xFFE6EDF3),
                cursorBg: const Color(0xFF4C8DFF),
                cursorFg: const Color(0xFF0D1117),
              ),
            ),
          ),
        ),
      );
    }

    testWidgets('满行 ASCII 渲染无异常', (tester) async {
      final row = await rowOf('Last login: Fri Oct 30 09:33:25 2026 fro');
      await tester.pumpWidget(harness(row));
      expect(tester.takeException(), isNull);
      expect(find.byType(CustomPaint), findsWidgets);
    });

    testWidgets('带光标 / 宽字符 / 反显都不崩', (tester) async {
      final row = await rowOf('中中中 abc');
      await tester.pumpWidget(harness(row, cursorX: 0));
      expect(tester.takeException(), isNull);
    });
  });
}
