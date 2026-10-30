import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'theme.dart';

String shortFp(String fp, {int head = 18, int tail = 6}) {
  if (fp.length <= head + tail + 1) return fp;
  return '${fp.substring(0, head)}…${fp.substring(fp.length - tail)}';
}

Future<void> copyText(BuildContext context, String text, String label) async {
  await Clipboard.setData(ClipboardData(text: text));
  if (!context.mounted) return;
  showSnack(context, '$label已复制');
}

void showSnack(
  BuildContext context,
  String message, {
  bool danger = false,
  bool ok = false,
}) {
  final messenger = ScaffoldMessenger.maybeOf(context);
  if (messenger == null) return;
  messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(
      SnackBar(
        content: Text(
          message,
          style: TextStyle(
            color: danger
                ? KwColors.danger
                : ok
                ? KwColors.ok
                : KwColors.text,
          ),
        ),
        duration: const Duration(seconds: 3),
      ),
    );
}

void showError(BuildContext context, Object error) {
  showSnack(context, error.toString(), danger: true);
}

Future<bool> confirm(
  BuildContext context, {
  required String title,
  required String message,
  String confirmText = '确定',
  bool danger = false,
}) async {
  final result = await showDialog<bool>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text(title),
      content: Text(message),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx, false),
          child: const Text('取消'),
        ),
        FilledButton(
          style: danger
              ? FilledButton.styleFrom(backgroundColor: KwColors.danger)
              : null,
          onPressed: () => Navigator.pop(ctx, true),
          child: Text(confirmText),
        ),
      ],
    ),
  );
  return result == true;
}

/// 小标签，三种语气：中性 / 成功 / 警告 / 危险。
Widget chip(String text, {Color? color, Color? background}) {
  final fg = color ?? KwColors.dim;
  final bg = background ?? KwColors.surface3;
  return Container(
    padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
    decoration: BoxDecoration(
      color: bg,
      borderRadius: BorderRadius.circular(5),
      border: Border.all(color: fg.withValues(alpha: 0.45)),
    ),
    child: Text(
      text,
      style: TextStyle(fontSize: 10.5, fontFamily: 'monospace', color: fg),
    ),
  );
}

Widget panel({required Widget child, EdgeInsets? padding}) {
  return Container(
    padding: padding ?? const EdgeInsets.all(12),
    decoration: BoxDecoration(
      color: KwColors.panel,
      borderRadius: BorderRadius.circular(10),
      border: Border.all(color: KwColors.border),
    ),
    child: child,
  );
}

/// 输入框统一规格，避免每个弹窗各写一套。
Widget field(
  TextEditingController controller,
  String label, {
  String? hint,
  bool obscure = false,
  TextInputType? keyboard,
  int maxLines = 1,
  String? helper,
}) {
  return Padding(
    padding: const EdgeInsets.only(bottom: 12),
    child: TextField(
      controller: controller,
      obscureText: obscure,
      keyboardType: keyboard,
      maxLines: maxLines,
      style: TextStyle(fontSize: 14, color: KwColors.text),
      decoration: InputDecoration(
        labelText: label,
        hintText: hint,
        helperText: helper,
        helperStyle: TextStyle(fontSize: 11, color: KwColors.faint),
      ),
    ),
  );
}
