import 'package:flutter/material.dart';

import 'terminal_emulator.dart';

/// 一行文字的逐格自绘：每格固定 `i * cellW`，宽字符占两格，光标画成实心块。
///
/// 为什么不用 `Text.rich`：它依赖整行文本的自然宽度，字体实际字宽比测量值大一点
/// （CJK、emoji、字体回退都会）时，41 列就溢出容器、被 clip 掉最后一列
/// （截图里 `from` 变成 `fro`）。这里把每格钉死在固定坐标，累积误差归零，
/// 最后一列再也不会被挤掉。
class TerminalRowPainter extends CustomPainter {
  TerminalRowPainter({
    required this.row,
    required this.cursorX,
    required this.cellW,
    required this.cellH,
    required this.fontSize,
    required this.lineHeight,
    required this.fontFamily,
    required this.defaultFg,
    required this.cursorBg,
    required this.cursorFg,
    this.selStart = -1,
    this.selEnd = -1,
    this.selColor = const Color(0x554C8DFF),
  });

  final List<TermCell> row;
  final int cursorX;
  final double cellW;
  final double cellH;
  final double fontSize;
  final double lineHeight;
  final String? fontFamily;
  final Color defaultFg;
  final Color cursorBg;
  final Color cursorFg;

  /// 本行被选中的列区间（含端点）；-1 表示没有选区。
  final int selStart;
  final int selEnd;
  final Color selColor;

  /// 画笔的前景/背景色（处理反显与暗色）。
  (Color, Color?) _colors(TermPen pen) {
    var fg = pen.fg >= 0 ? Color(pen.fg) : defaultFg;
    Color? bg = pen.bg >= 0 ? Color(pen.bg) : null;
    if (pen.inverse) {
      final swapped = bg ?? defaultFg;
      bg = fg;
      fg = swapped;
    }
    if (pen.dim) fg = fg.withValues(alpha: 0.65);
    return (fg, bg);
  }

  TextStyle _style(TermPen pen, {bool isCursor = false}) {
    final (fg, _) = _colors(pen);
    return TextStyle(
      fontFamily: fontFamily,
      fontSize: fontSize,
      height: lineHeight,
      color: isCursor ? cursorFg : fg,
      fontWeight: pen.bold ? FontWeight.w700 : FontWeight.w400,
    );
  }

  @override
  void paint(Canvas canvas, Size size) {
    canvas.clipRect(Offset.zero & size);
    if (row.isEmpty || cellW <= 0) return;

    final bgPaint = Paint()..style = PaintingStyle.fill;

    // 1) 背景块（vim/htop 的状态栏、选中行都靠它）
    for (var i = 0; i < row.length; i++) {
      final cell = row[i];
      if (cell.isContinuation) continue;
      final (_, bg) = _colors(cell.pen);
      if (bg == null) continue;
      final span = isWideRune(cell.rune) ? 2 : 1;
      bgPaint.color = bg;
      canvas.drawRect(
        Rect.fromLTWH(i * cellW, 0, span * cellW, cellH),
        bgPaint,
      );
    }

    // 2) 选区高亮（长按拖动选中）
    if (selStart >= 0 && selEnd >= selStart && row.isNotEmpty) {
      final s = selStart.clamp(0, row.length - 1);
      final e = selEnd.clamp(0, row.length - 1);
      canvas.drawRect(
        Rect.fromLTWH(s * cellW, 0, (e - s + 1) * cellW, cellH),
        Paint()..color = selColor,
      );
    }

    // 3) 光标实心块（先画块，再把字以背景色补在上面）
    final hasCursor = cursorX >= 0 && cursorX < row.length;
    if (hasCursor) {
      bgPaint.color = cursorBg;
      canvas.drawRect(
        Rect.fromLTWH(cursorX * cellW, 0, cellW, cellH),
        bgPaint,
      );
    }

    // 4) 逐格画字形，x 固定为 i * cellW（不随字宽累积漂移）
    for (var i = 0; i < row.length; i++) {
      final cell = row[i];
      final isCursor = i == cursorX;
      if (cell.isContinuation) continue;
      if (cell.rune != 0x20) {
        final tp = TextPainter(
          text: TextSpan(
            text: String.fromCharCode(cell.rune),
            style: _style(cell.pen, isCursor: isCursor),
          ),
          textDirection: TextDirection.ltr,
          maxLines: 1,
        )..layout();
        final dy = ((cellH - tp.height) / 2).clamp(0.0, cellH);
        tp.paint(canvas, Offset(i * cellW, dy));
        tp.dispose();
      }
      if (cell.pen.underline && !isCursor) {
        final (fg, _) = _colors(cell.pen);
        canvas.drawRect(
          Rect.fromLTWH(i * cellW, cellH - 1.5, cellW, 1),
          Paint()..color = fg,
        );
      }
    }
  }

  @override
  bool shouldRepaint(TerminalRowPainter old) {
    // 终端只在 setState / 滚动时重建，这里直接重绘最省心
    return true;
  }
}
