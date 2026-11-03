/// 隐藏输入框里常驻的「哨兵」字符（零宽空格）。
///
/// 它让"退格"永远可被检测到：输入框永远非空（含一个零宽字符），用户按退格时
/// 文本从 `\u200b` 变成空串，我们据此发一个 0x7F 再把它补回去 —— 不再靠猜
/// "删了几个字"去判断。
const String kImeSentinel = '\u200b';

/// 输入框前后两次文本 → 用户实际做了哪种编辑。
sealed class InputEdit {
  const InputEdit();
}

/// 要发往 PTY 的一段文本（可能是用户刚敲的、粘贴的、或输入法上屏的）。
class TextEdit extends InputEdit {
  const TextEdit(this.text);
  final String text;
}

/// 要发往 PTY 的退格数（每个 = 0x7F）。
class BackspaceEdit extends InputEdit {
  const BackspaceEdit(this.count);
  final int count;
}

/// 比较输入框的 `previous` 与 `current`，算出该发什么。
///
/// - 追加（`current` 以 `previous` 开头）：发新增的文本。
/// - 变短（`previous` 以 `current` 开头）：每个少的字符发一个退格。
/// - 哨兵被删掉（空按退格）：补发一个退格。
/// - 其它（整段替换/全选删除）：发当前正文，空正文则补一个退格。
///
/// 返回 null 表示没有变化，什么都不发。
InputEdit? diffIme(String previous, String current) {
  if (current == previous) return null;
  if (current.startsWith(previous)) {
    final added = current.substring(previous.length);
    return added.isEmpty ? null : TextEdit(added);
  }
  if (previous.startsWith(current)) {
    final removed = previous.length - current.length;
    return removed > 0 ? BackspaceEdit(removed) : null;
  }
  final body = current.startsWith(kImeSentinel)
      ? current.substring(kImeSentinel.length)
      : current;
  return body.isEmpty ? const BackspaceEdit(1) : TextEdit(body);
}
