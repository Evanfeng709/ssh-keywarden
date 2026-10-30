import 'package:flutter/material.dart';

/// 一套配色。桌面端是 6 套 styles.css 主题，这里保持同样的数量与语气：
/// 3 套深色（曜石 / 午夜 / 石墨）+ 3 套浅色（云白 / 亚麻 / 雾青）。
class KwPalette {
  const KwPalette({
    required this.id,
    required this.name,
    required this.brightness,
    required this.bg,
    required this.bgElev,
    required this.panel,
    required this.surface2,
    required this.surface3,
    required this.border,
    required this.text,
    required this.dim,
    required this.faint,
    required this.accent,
    required this.accentSoft,
    required this.ok,
    required this.okSoft,
    required this.warn,
    required this.warnSoft,
    required this.danger,
    required this.dangerSoft,
  });

  final String id;
  final String name;
  final Brightness brightness;
  final Color bg;
  final Color bgElev;
  final Color panel;
  final Color surface2;
  final Color surface3;
  final Color border;
  final Color text;
  final Color dim;
  final Color faint;
  final Color accent;
  final Color accentSoft;
  final Color ok;
  final Color okSoft;
  final Color warn;
  final Color warnSoft;
  final Color danger;
  final Color dangerSoft;

  bool get isDark => brightness == Brightness.dark;
}

const List<KwPalette> kwPalettes = [
  KwPalette(
    id: 'obsidian',
    name: '曜石',
    brightness: Brightness.dark,
    bg: Color(0xFF0D1117),
    bgElev: Color(0xFF151B23),
    panel: Color(0xFF131920),
    surface2: Color(0xFF0F151D),
    surface3: Color(0xFF1A2330),
    border: Color(0xFF232C38),
    text: Color(0xFFE6EDF3),
    dim: Color(0xFF9AA7B4),
    faint: Color(0xFF66717E),
    accent: Color(0xFF4C8DFF),
    accentSoft: Color(0xFF16243A),
    ok: Color(0xFF3FB950),
    okSoft: Color(0xFF16281C),
    warn: Color(0xFFD6A034),
    warnSoft: Color(0xFF2A2418),
    danger: Color(0xFFF85149),
    dangerSoft: Color(0xFF2A1618),
  ),
  KwPalette(
    id: 'midnight',
    name: '午夜',
    brightness: Brightness.dark,
    bg: Color(0xFF0A1020),
    bgElev: Color(0xFF101A2E),
    panel: Color(0xFF0E1728),
    surface2: Color(0xFF0B1322),
    surface3: Color(0xFF17233A),
    border: Color(0xFF22304A),
    text: Color(0xFFE4ECF8),
    dim: Color(0xFF97A8C0),
    faint: Color(0xFF64748F),
    accent: Color(0xFF6AA6FF),
    accentSoft: Color(0xFF17274A),
    ok: Color(0xFF4CC38A),
    okSoft: Color(0xFF14291F),
    warn: Color(0xFFE0B04A),
    warnSoft: Color(0xFF2B2415),
    danger: Color(0xFFFF6B6B),
    dangerSoft: Color(0xFF2B1719),
  ),
  KwPalette(
    id: 'graphite',
    name: '石墨',
    brightness: Brightness.dark,
    bg: Color(0xFF121212),
    bgElev: Color(0xFF1A1A1A),
    panel: Color(0xFF161616),
    surface2: Color(0xFF101010),
    surface3: Color(0xFF232323),
    border: Color(0xFF2E2E2E),
    text: Color(0xFFE8E8E8),
    dim: Color(0xFFA0A0A0),
    faint: Color(0xFF6E6E6E),
    accent: Color(0xFFA78BFA),
    accentSoft: Color(0xFF26203B),
    ok: Color(0xFF6CCB8A),
    okSoft: Color(0xFF1D2A21),
    warn: Color(0xFFD9A94E),
    warnSoft: Color(0xFF2B2517),
    danger: Color(0xFFF0736A),
    dangerSoft: Color(0xFF2C1918),
  ),
  KwPalette(
    id: 'cloud',
    name: '云白',
    brightness: Brightness.light,
    bg: Color(0xFFF3F6FA),
    bgElev: Color(0xFFFFFFFF),
    panel: Color(0xFFFFFFFF),
    surface2: Color(0xFFF7F9FC),
    surface3: Color(0xFFEAEFF6),
    border: Color(0xFFD7DEE8),
    text: Color(0xFF1B2430),
    dim: Color(0xFF4C5A6B),
    faint: Color(0xFF8794A5),
    accent: Color(0xFF2F6FE4),
    accentSoft: Color(0xFFDCE8FD),
    ok: Color(0xFF1F8A4C),
    okSoft: Color(0xFFDCF3E5),
    warn: Color(0xFF8A6100),
    warnSoft: Color(0xFFFBF0D6),
    danger: Color(0xFFC43D33),
    dangerSoft: Color(0xFFFBE3E1),
  ),
  KwPalette(
    id: 'linen',
    name: '亚麻',
    brightness: Brightness.light,
    bg: Color(0xFFF6F1E7),
    bgElev: Color(0xFFFDFBF6),
    panel: Color(0xFFFFFDF9),
    surface2: Color(0xFFF8F3EA),
    surface3: Color(0xFFEFE7D8),
    border: Color(0xFFDED3C0),
    text: Color(0xFF2B2318),
    dim: Color(0xFF5F5342),
    faint: Color(0xFF8F8371),
    accent: Color(0xFFB0662A),
    accentSoft: Color(0xFFF5E3CE),
    ok: Color(0xFF2E7D4F),
    okSoft: Color(0xFFE3F1E7),
    warn: Color(0xFF8A6100),
    warnSoft: Color(0xFFF8EDD2),
    danger: Color(0xFFB93B32),
    dangerSoft: Color(0xFFF7E1DD),
  ),
  KwPalette(
    id: 'mist',
    name: '雾青',
    brightness: Brightness.light,
    bg: Color(0xFFEDF3F1),
    bgElev: Color(0xFFF9FCFB),
    panel: Color(0xFFFDFFFE),
    surface2: Color(0xFFF2F7F5),
    surface3: Color(0xFFE1EBE7),
    border: Color(0xFFCFDCD7),
    text: Color(0xFF17231F),
    dim: Color(0xFF4B5C56),
    faint: Color(0xFF7C8E88),
    accent: Color(0xFF2E7D6B),
    accentSoft: Color(0xFFDCEDE8),
    ok: Color(0xFF2F7D4F),
    okSoft: Color(0xFFE0F1E6),
    warn: Color(0xFF8A6100),
    warnSoft: Color(0xFFF4EBD3),
    danger: Color(0xFFB34238),
    dangerSoft: Color(0xFFF6E2DF),
  ),
];

/// 全局取色器：界面里 100 多处 `KwColors.xxx` 都读当前调色板。
/// 切主题只需要 `KwColors.select(i)` + 顶层 setState。
class KwColors {
  static int _index = 0;

  static KwPalette get palette => kwPalettes[_index];

  static void select(int index) {
    _index = index % kwPalettes.length;
    if (_index < 0) _index += kwPalettes.length;
  }

  static Color get bg => palette.bg;
  static Color get bgElev => palette.bgElev;
  static Color get panel => palette.panel;
  static Color get surface2 => palette.surface2;
  static Color get surface3 => palette.surface3;
  static Color get border => palette.border;
  static Color get text => palette.text;
  static Color get dim => palette.dim;
  static Color get faint => palette.faint;
  static Color get accent => palette.accent;
  static Color get accentSoft => palette.accentSoft;
  static Color get ok => palette.ok;
  static Color get okSoft => palette.okSoft;
  static Color get warn => palette.warn;
  static Color get warnSoft => palette.warnSoft;
  static Color get danger => palette.danger;
  static Color get dangerSoft => palette.dangerSoft;
}

class KwTheme {
  static ThemeData build() {
    final p = KwColors.palette;
    final scheme = ColorScheme(
      brightness: p.brightness,
      primary: p.accent,
      onPrimary: Colors.white,
      secondary: p.accent,
      onSecondary: Colors.white,
      error: p.danger,
      onError: Colors.white,
      surface: p.panel,
      onSurface: p.text,
    );

    return ThemeData(
      useMaterial3: true,
      brightness: p.brightness,
      colorScheme: scheme,
      scaffoldBackgroundColor: p.bg,
      canvasColor: p.bgElev,
      dividerColor: p.border,
      visualDensity: VisualDensity.compact,
      appBarTheme: AppBarTheme(
        backgroundColor: p.bgElev,
        surfaceTintColor: Colors.transparent,
        elevation: 0,
        centerTitle: false,
        toolbarHeight: 50,
        titleTextStyle: TextStyle(
          color: p.text,
          fontSize: 15,
          fontWeight: FontWeight.w600,
        ),
        iconTheme: IconThemeData(color: p.dim, size: 20),
      ),
      navigationBarTheme: NavigationBarThemeData(
        backgroundColor: p.bgElev,
        indicatorColor: p.accentSoft,
        labelTextStyle: WidgetStateProperty.all(
          TextStyle(fontSize: 10.5, color: p.dim),
        ),
        iconTheme: WidgetStateProperty.resolveWith((states) {
          final selected = states.contains(WidgetState.selected);
          return IconThemeData(size: 20, color: selected ? p.accent : p.faint);
        }),
      ),
      inputDecorationTheme: InputDecorationTheme(
        filled: true,
        fillColor: p.surface2,
        hintStyle: TextStyle(color: p.faint, fontSize: 12.5),
        labelStyle: TextStyle(color: p.dim, fontSize: 12.5),
        contentPadding: const EdgeInsets.symmetric(
          horizontal: 11,
          vertical: 11,
        ),
        border: OutlineInputBorder(
          borderRadius: BorderRadius.circular(8),
          borderSide: BorderSide(color: p.border),
        ),
        enabledBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(8),
          borderSide: BorderSide(color: p.border),
        ),
        focusedBorder: OutlineInputBorder(
          borderRadius: BorderRadius.circular(8),
          borderSide: BorderSide(color: p.accent),
        ),
      ),
      dialogTheme: DialogThemeData(
        backgroundColor: p.bgElev,
        surfaceTintColor: Colors.transparent,
        titleTextStyle: TextStyle(
          color: p.text,
          fontSize: 15,
          fontWeight: FontWeight.w600,
        ),
        contentTextStyle: TextStyle(color: p.dim, fontSize: 13, height: 1.5),
      ),
      snackBarTheme: SnackBarThemeData(
        backgroundColor: p.surface3,
        contentTextStyle: TextStyle(color: p.text, fontSize: 12.5),
        behavior: SnackBarBehavior.floating,
      ),
      bottomSheetTheme: BottomSheetThemeData(
        backgroundColor: p.bgElev,
        surfaceTintColor: Colors.transparent,
        modalBackgroundColor: p.bgElev,
      ),
      textButtonTheme: TextButtonThemeData(
        style: TextButton.styleFrom(foregroundColor: p.accent),
      ),
      filledButtonTheme: FilledButtonThemeData(
        style: FilledButton.styleFrom(
          backgroundColor: p.accent,
          foregroundColor: Colors.white,
          minimumSize: const Size(0, 38),
          textStyle: const TextStyle(fontSize: 13),
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
        ),
      ),
      outlinedButtonTheme: OutlinedButtonThemeData(
        style: OutlinedButton.styleFrom(
          foregroundColor: p.text,
          side: BorderSide(color: p.border),
          minimumSize: const Size(0, 38),
          textStyle: const TextStyle(fontSize: 13),
          shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
        ),
      ),
      progressIndicatorTheme: ProgressIndicatorThemeData(color: p.accent),
      listTileTheme: ListTileThemeData(
        iconColor: p.dim,
        textColor: p.text,
        dense: true,
      ),
    );
  }
}

/// 主题选择面板：色块 + 名称，点一下立即生效。
Widget paletteSheet(
  BuildContext context, {
  required int current,
  required ValueChanged<int> onSelect,
}) {
  return SafeArea(
    // 手机上横屏或小窗时整块会超出弹层高度，滚动兜底。
    child: SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(16, 14, 16, 10),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            '外观主题',
            style: TextStyle(
              fontSize: 14,
              fontWeight: FontWeight.w600,
              color: KwColors.text,
            ),
          ),
          const SizedBox(height: 4),
          Text(
            '深色 3 套、浅色 3 套；只影响手机端，桌面端在设置里各自选。',
            style: TextStyle(fontSize: 11.5, color: KwColors.faint),
          ),
          const SizedBox(height: 12),
          ...List.generate(kwPalettes.length, (i) {
            final p = kwPalettes[i];
            final selected = i == current;
            return InkWell(
              onTap: () => onSelect(i),
              borderRadius: BorderRadius.circular(8),
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 7),
                child: Row(
                  children: [
                    Container(
                      width: 42,
                      height: 26,
                      decoration: BoxDecoration(
                        color: p.bg,
                        borderRadius: BorderRadius.circular(6),
                        border: Border.all(color: p.border),
                      ),
                      alignment: Alignment.center,
                      child: Container(
                        width: 14,
                        height: 14,
                        decoration: BoxDecoration(
                          color: p.accent,
                          shape: BoxShape.circle,
                        ),
                      ),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        p.name,
                        style: TextStyle(
                          fontSize: 13,
                          color: KwColors.text,
                          fontWeight: selected
                              ? FontWeight.w600
                              : FontWeight.w400,
                        ),
                      ),
                    ),
                    Text(
                      p.isDark ? '深色' : '浅色',
                      style: TextStyle(
                        fontSize: 11,
                        fontFamily: 'monospace',
                        color: KwColors.faint,
                      ),
                    ),
                    const SizedBox(width: 10),
                    if (selected)
                      Icon(Icons.check_rounded, size: 17, color: p.accent),
                  ],
                ),
              ),
            );
          }),
        ],
      ),
    ),
  );
}