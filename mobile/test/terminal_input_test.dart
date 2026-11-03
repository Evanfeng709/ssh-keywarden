import 'package:flutter_test/flutter_test.dart';
import 'package:kw_mobile/src/terminal_input.dart';

void main() {
  const s = kImeSentinel;

  group('diffIme：输入框变化 → 该发什么', () {
    test('敲一个字符：新增文本', () {
      final edit = diffIme(s, '${s}a');
      expect(edit, isA<TextEdit>());
      expect((edit as TextEdit).text, 'a');
    });

    test('连着敲：只发新增部分', () {
      final edit = diffIme('${s}p', '${s}pu');
      expect((edit as TextEdit).text, 'u');
    });

    test('空按退格：哨兵被删 → 补一个退格（这是原来删不掉的关键）', () {
      final edit = diffIme(s, '');
      expect(edit, isA<BackspaceEdit>());
      expect((edit as BackspaceEdit).count, 1);
    });

    test('输入框里有字时退格：按差值发多个退格', () {
      final edit = diffIme('${s}abc', '${s}a');
      expect(edit, isA<BackspaceEdit>());
      expect((edit as BackspaceEdit).count, 2);
    });

    test('整段粘贴：一次发全文', () {
      final edit = diffIme(s, '${s}docker ps');
      expect((edit as TextEdit).text, 'docker ps');
    });

    test('整段替换：发新正文', () {
      final edit = diffIme('${s}abc', '${s}xy');
      expect((edit as TextEdit).text, 'xy');
    });

    test('没有变化：什么都不发', () {
      expect(diffIme(s, s), isNull);
      expect(diffIme('${s}a', '${s}a'), isNull);
    });
  });
}
