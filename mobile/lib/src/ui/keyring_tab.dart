import 'dart:async';

import 'package:flutter/material.dart';

import '../app_state.dart';
import '../models.dart';
import 'common.dart';
import 'theme.dart';

/// 密钥包 + 微软账号（OneDrive 应用专属文件夹）同步。
class KeyringTab extends StatefulWidget {
  const KeyringTab({super.key, required this.state});

  final AppState state;

  @override
  State<KeyringTab> createState() => _KeyringTabState();
}

class _KeyringTabState extends State<KeyringTab> {
  MsStatus _status = MsStatus.empty;
  bool _loaded = false;

  AppState get state => widget.state;

  @override
  void initState() {
    super.initState();
    _refreshStatus();
  }

  Future<void> _refreshStatus() async {
    try {
      final st = await state.msStatus();
      if (!mounted) return;
      setState(() {
        _status = st;
        _loaded = true;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() => _loaded = true);
      showError(context, e);
    }
  }

  @override
  Widget build(BuildContext context) {
    final bootstrap = state.bootstrap;
    final privateKeys = bootstrap?.privateKeys ?? const <LocalKeyEntry>[];

    return ListView(
      padding: const EdgeInsets.all(12),
      children: [
        _msPanel(context),
        const SizedBox(height: 10),
        panel(child: _keyPack(context, bootstrap, privateKeys)),
      ],
    );
  }

  /* ------------------------------------------------------------------ */
  /* 微软账号同步                                                        */
  /* ------------------------------------------------------------------ */

  Widget _msPanel(BuildContext context) {
    final st = _status;
    final online = st.loggedIn;
    return panel(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                online ? Icons.cloud_done_outlined : Icons.cloud_outlined,
                size: 16,
                color: online ? KwColors.ok : KwColors.dim,
              ),
              const SizedBox(width: 7),
              Text(
                '微软账号同步',
                style: TextStyle(
                  fontSize: 14,
                  fontWeight: FontWeight.w600,
                  color: KwColors.text,
                ),
              ),
              const Spacer(),
              if (online)
                chip('已登录', color: KwColors.ok, background: KwColors.okSoft),
              const SizedBox(width: 6),
              IconButton(
                tooltip: '刷新状态',
                visualDensity: VisualDensity.compact,
                padding: EdgeInsets.zero,
                constraints: const BoxConstraints(minWidth: 30, minHeight: 30),
                onPressed: _refreshStatus,
                icon: Icon(
                  Icons.refresh_rounded,
                  size: 17,
                  color: KwColors.dim,
                ),
              ),
            ],
          ),
          const SizedBox(height: 3),
          _statusLine(),
          const SizedBox(height: 10),
          ..._actions(context),
          const SizedBox(height: 9),
          Text(
            '云端只存一个端到端加密的同步包：主机地址、用户名、钥匙备注、主机指纹。'
            '私钥永远不上传，微软也拿不到口令。',
            style: TextStyle(fontSize: 11, height: 1.55, color: KwColors.faint),
          ),
        ],
      ),
    );
  }

  Widget _statusLine() {
    if (!_loaded) {
      return Text(
        '正在读取同步状态…',
        style: TextStyle(fontSize: 11.5, color: KwColors.faint),
      );
    }
    final st = _status;
    if (!st.configured) {
      return Text(
        '还没有配置 Azure 客户端 ID。同步走你自己的应用注册，不借用任何公共客户端。',
        style: TextStyle(fontSize: 11.5, height: 1.5, color: KwColors.warn),
      );
    }
    final facts = <String>[
      '客户端 ${_shortId(st.clientId)}',
      st.loggedIn ? '上次同步 ${_fmtTime(st.lastSyncAt)}' : '未登录',
      st.hasSyncKey ? '已记住同步口令' : '未记住同步口令',
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          st.loggedIn ? st.accountLabel : '尚未登录微软账号',
          style: TextStyle(fontSize: 12.5, color: KwColors.text),
        ),
        const SizedBox(height: 2),
        Text(
          facts.join('  ·  '),
          style: TextStyle(
            fontSize: 10.5,
            fontFamily: 'monospace',
            color: KwColors.faint,
            height: 1.5,
          ),
        ),
        if (st.loggedIn &&
            st.accountEmail.isNotEmpty &&
            st.accountEmail != st.accountName)
          Text(
            st.accountEmail,
            style: TextStyle(fontSize: 10.5, color: KwColors.faint),
          ),
      ],
    );
  }

  List<Widget> _actions(BuildContext context) {
    final st = _status;
    if (!_loaded) return const [];
    if (!st.configured) {
      return [
        SizedBox(
          width: double.infinity,
          child: FilledButton.icon(
            onPressed: () => _configure(context),
            icon: const Icon(Icons.settings_outlined, size: 16),
            label: const Text('配置 Azure 客户端 ID'),
          ),
        ),
      ];
    }
    if (!st.loggedIn) {
      return [
        SizedBox(
          width: double.infinity,
          child: FilledButton.icon(
            onPressed: () => _login(context),
            icon: const Icon(Icons.login_rounded, size: 16),
            label: const Text('登录微软账号'),
          ),
        ),
        const SizedBox(height: 2),
        TextButton.icon(
          onPressed: () => _configure(context),
          icon: const Icon(Icons.settings_outlined, size: 15),
          label: const Text('修改配置', style: TextStyle(fontSize: 12)),
        ),
      ];
    }
    return [
      Row(
        children: [
          Expanded(
            child: FilledButton.icon(
              onPressed: () => _sync(context),
              icon: const Icon(Icons.sync_rounded, size: 16),
              label: const Text('立即同步'),
            ),
          ),
          const SizedBox(width: 8),
          OutlinedButton.icon(
            onPressed: () => _logout(context),
            icon: const Icon(Icons.logout_rounded, size: 15),
            label: const Text('退出'),
          ),
        ],
      ),
      const SizedBox(height: 2),
      Row(
        children: [
          TextButton(
            onPressed: () => _configure(context),
            child: const Text('修改配置', style: TextStyle(fontSize: 12)),
          ),
          if (st.hasSyncKey)
            TextButton(
              onPressed: () => _clearKey(context),
              child: Text(
                '清除同步口令',
                style: TextStyle(fontSize: 12, color: KwColors.warn),
              ),
            ),
        ],
      ),
    ];
  }

  Future<void> _configure(BuildContext context) async {
    final st = _status;
    final id = TextEditingController(text: st.clientId);
    final key = TextEditingController();
    var clearKey = false;

    final saved = await showDialog<bool>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setLocal) => AlertDialog(
          title: const Text('Azure 应用注册'),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Container(
                  padding: const EdgeInsets.all(9),
                  decoration: BoxDecoration(
                    color: KwColors.surface2,
                    borderRadius: BorderRadius.circular(7),
                    border: Border.all(color: KwColors.border),
                  ),
                  child: Text(
                    'portal.azure.com → Microsoft Entra ID → 应用注册 → 新注册\n'
                    '1. 受支持的账户类型选「仅个人 Microsoft 账户」\n'
                    '2. 「身份验证」里启用「允许公共客户端流」\n'
                    '3. 「API 权限」加 Microsoft Graph 委托权限：\n'
                    '    Files.ReadWrite.AppFolder、User.Read、offline_access\n'
                    '4. 把「应用程序(客户端) ID」填到下面',
                    style: TextStyle(
                      fontSize: 11,
                      height: 1.6,
                      color: KwColors.dim,
                    ),
                  ),
                ),
                const SizedBox(height: 12),
                field(
                  id,
                  '客户端 ID',
                  hint: '00000000-0000-0000-0000-000000000000',
                ),
                field(
                  key,
                  '同步口令（可选）',
                  obscure: true,
                  helper: st.hasSyncKey
                      ? '本机已记住一份；这里留空就不改'
                      : '用来端到端加密同步包，至少 8 位',
                ),
                if (st.hasSyncKey)
                  CheckboxListTile(
                    contentPadding: EdgeInsets.zero,
                    dense: true,
                    value: clearKey,
                    onChanged: (v) => setLocal(() => clearKey = v ?? false),
                    title: Text(
                      '清除已记住的同步口令',
                      style: TextStyle(fontSize: 12.5, color: KwColors.text),
                    ),
                  ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('保存'),
            ),
          ],
        ),
      ),
    );

    final clientId = id.text.trim();
    final syncKey = key.text;
    id.dispose();
    key.dispose();
    if (saved != true || !context.mounted) return;

    try {
      final next = await state.msSaveConfig(
        clientId: clientId,
        syncKey: clearKey ? '' : (syncKey.isEmpty ? null : syncKey),
      );
      if (!context.mounted) return;
      setState(() => _status = next);
      showSnack(
        context,
        next.configured ? '配置已保存' : '客户端 ID 已清除',
        ok: next.configured,
      );
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  Future<void> _login(BuildContext context) async {
    DeviceLoginPrompt prompt;
    try {
      prompt = await state.msStartLogin(
        clientId: _status.clientId.isEmpty ? null : _status.clientId,
      );
    } catch (e) {
      if (context.mounted) showError(context, e);
      return;
    }
    if (!context.mounted) return;
    if (prompt.userCode.isEmpty || prompt.deviceCode.isEmpty) {
      showSnack(context, '微软没有返回设备码，稍后重试', danger: true);
      return;
    }

    final go = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('登录微软账号'),
        content: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '1. 打开下面的地址（电脑或手机浏览器都行）：',
                style: TextStyle(fontSize: 12, color: KwColors.dim),
              ),
              const SizedBox(height: 4),
              SelectableText(
                prompt.verificationUri,
                style: TextStyle(
                  fontSize: 12,
                  fontFamily: 'monospace',
                  color: KwColors.accent,
                ),
              ),
              TextButton.icon(
                style: TextButton.styleFrom(
                  padding: EdgeInsets.zero,
                  visualDensity: VisualDensity.compact,
                ),
                onPressed: () => copyText(ctx, prompt.verificationUri, '授权地址'),
                icon: const Icon(Icons.copy_rounded, size: 14),
                label: const Text('复制地址', style: TextStyle(fontSize: 12)),
              ),
              const SizedBox(height: 8),
              Text(
                '2. 输入这个代码：',
                style: TextStyle(fontSize: 12, color: KwColors.dim),
              ),
              const SizedBox(height: 6),
              Container(
                width: double.infinity,
                padding: const EdgeInsets.symmetric(
                  horizontal: 12,
                  vertical: 10,
                ),
                decoration: BoxDecoration(
                  color: KwColors.surface2,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(color: KwColors.border),
                ),
                child: Row(
                  children: [
                    Expanded(
                      child: SelectableText(
                        prompt.userCode,
                        style: TextStyle(
                          fontSize: 24,
                          fontWeight: FontWeight.w700,
                          letterSpacing: 3,
                          fontFamily: 'monospace',
                          color: KwColors.text,
                        ),
                      ),
                    ),
                    IconButton(
                      tooltip: '复制代码',
                      visualDensity: VisualDensity.compact,
                      onPressed: () => copyText(ctx, prompt.userCode, '设备代码'),
                      icon: Icon(
                        Icons.copy_rounded,
                        size: 17,
                        color: KwColors.dim,
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 8),
              Text(
                '代码 ${(prompt.expiresIn / 60).ceil()} 分钟内有效。授权范围只有'
                ' OneDrive 应用专属文件夹和昵称/邮箱读取，没有短信、邮件或云盘全盘权限。',
                style: TextStyle(
                  fontSize: 11,
                  height: 1.55,
                  color: KwColors.faint,
                ),
              ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('我已完成授权'),
          ),
        ],
      ),
    );
    if (go != true || !context.mounted) return;

    BuildContext? waitCtx;
    unawaited(
      showDialog<void>(
        context: context,
        barrierDismissible: false,
        builder: (ctx) {
          waitCtx = ctx;
          return PopScope(
            canPop: false,
            child: AlertDialog(
              title: const Text('等待微软授权'),
              content: Row(
                children: [
                  const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Text(
                      '浏览器那边点完「同意」后会自动继续，最多等 5 分钟…',
                      style: TextStyle(
                        fontSize: 12,
                        height: 1.5,
                        color: KwColors.dim,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          );
        },
      ),
    );

    try {
      final st = await state.msCompleteLogin(
        prompt.deviceCode,
        timeoutSec: prompt.expiresIn.clamp(5, 300),
      );
      if (waitCtx != null && waitCtx!.mounted) Navigator.of(waitCtx!).pop();
      if (!context.mounted) return;
      setState(() => _status = st);
      showSnack(context, '已登录 ${st.accountLabel}', ok: true);
    } catch (e) {
      if (waitCtx != null && waitCtx!.mounted) Navigator.of(waitCtx!).pop();
      if (context.mounted) showError(context, e);
    }
  }

  Future<void> _logout(BuildContext context) async {
    final ok = await confirm(
      context,
      title: '退出微软账号',
      message: '只会删掉本机的令牌；云端同步包不动，下次登录还能接着用。',
      confirmText: '退出',
      danger: true,
    );
    if (!ok || !context.mounted) return;
    try {
      await state.msLogout();
      if (!context.mounted) return;
      await _refreshStatus();
      if (context.mounted) showSnack(context, '已退出微软账号', ok: true);
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  Future<void> _clearKey(BuildContext context) async {
    final ok = await confirm(
      context,
      title: '清除同步口令',
      message: '只忘掉本机记住的口令，云端同步包不变。下次同步需要重新输入。',
      confirmText: '清除',
      danger: true,
    );
    if (!ok || !context.mounted) return;
    try {
      final next = await state.msSaveConfig(syncKey: '');
      if (!context.mounted) return;
      setState(() => _status = next);
      showSnack(context, '已清除同步口令', ok: true);
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  Future<void> _sync(BuildContext context) async {
    final st = _status;
    final pass = TextEditingController();
    var remember = st.hasSyncKey;

    final go = await showDialog<bool>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setLocal) => AlertDialog(
          title: const Text('同步到 OneDrive'),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  '拉云端 → 合并进本机 → 把合并结果重新加密上传。'
                  '云端与电脑、手机用同一个口令，口令不对就解不开。',
                  style: TextStyle(
                    fontSize: 12,
                    height: 1.55,
                    color: KwColors.dim,
                  ),
                ),
                const SizedBox(height: 12),
                field(
                  pass,
                  st.hasSyncKey ? '同步口令（留空用已记住的）' : '同步口令',
                  obscure: true,
                  helper: st.hasSyncKey ? '本机已记住一份，可以留空' : '没有口令无法解密云端内容',
                ),
                CheckboxListTile(
                  contentPadding: EdgeInsets.zero,
                  dense: true,
                  value: remember,
                  onChanged: (v) => setLocal(() => remember = v ?? false),
                  title: Text(
                    '在这台手机上记住口令',
                    style: TextStyle(fontSize: 12.5, color: KwColors.text),
                  ),
                  subtitle: Text(
                    '存在应用私有目录，不进云端、不进日志',
                    style: TextStyle(fontSize: 10.5, color: KwColors.faint),
                  ),
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('同步'),
            ),
          ],
        ),
      ),
    );
    final passphrase = pass.text;
    pass.dispose();
    if (go != true || !context.mounted) return;
    if (passphrase.isEmpty && !st.hasSyncKey) {
      showSnack(context, '先输入同步口令', danger: true);
      return;
    }

    try {
      if (passphrase.isNotEmpty) {
        // 新口令必须和云端保持一致；不记住就顺手清掉旧口令，避免下次用错
        await state.msSaveConfig(syncKey: remember ? passphrase : '');
      }
      var result = await state.msSync(passphrase: passphrase);
      if (!context.mounted) return;

      final conflicts = result.report['knownHostConflicts'];
      if (conflicts is List && conflicts.isNotEmpty) {
        final trust = await _askConflicts(context, conflicts);
        if (!context.mounted) return;
        if (trust == true) {
          final keys = conflicts
              .whereType<Map>()
              .map((c) => c['key']?.toString() ?? '')
              .where((k) => k.isNotEmpty)
              .toList();
          result = await state.msSync(
            passphrase: passphrase,
            trustChanged: keys,
          );
          if (!context.mounted) return;
        } else {
          await _refreshStatus();
          if (!context.mounted) return;
          showSnack(
            context,
            '已拉取云端其余变更；${conflicts.length} 个指纹冲突保留本机，本次未上传',
            ok: true,
          );
          return;
        }
      }

      await _refreshStatus();
      if (!context.mounted) return;
      showSnack(context, _resultText(result), ok: true);
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  Future<bool?> _askConflicts(BuildContext context, List conflicts) {
    return showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('主机指纹不一致'),
        content: SizedBox(
          width: double.maxFinite,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '以下主机两边记录的指纹不同，默认保留本机、不覆盖云端。'
                '只有确认对方服务器确实重装或更换过主机密钥时才应覆盖。',
                style: TextStyle(
                  fontSize: 11.5,
                  height: 1.55,
                  color: KwColors.dim,
                ),
              ),
              const SizedBox(height: 10),
              ...conflicts.map((raw) {
                final c = raw is Map ? raw : const {};
                return Padding(
                  padding: const EdgeInsets.only(bottom: 8),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        c['key']?.toString() ?? '',
                        style: TextStyle(
                          fontSize: 12,
                          fontFamily: 'monospace',
                          color: KwColors.text,
                        ),
                      ),
                      Text(
                        '本机 ${c['localFingerprint'] ?? ''}',
                        style: TextStyle(
                          fontSize: 10.5,
                          fontFamily: 'monospace',
                          color: KwColors.faint,
                        ),
                      ),
                      Text(
                        '云端 ${c['remoteFingerprint'] ?? ''}',
                        style: TextStyle(
                          fontSize: 10.5,
                          fontFamily: 'monospace',
                          color: KwColors.warn,
                        ),
                      ),
                    ],
                  ),
                );
              }),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('保留本机'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: KwColors.warn),
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('全部覆盖'),
          ),
        ],
      ),
    );
  }

  String _resultText(MsSyncResult r) {
    final report = r.report;
    int n(String key) => (report[key] as num?)?.toInt() ?? 0;
    final parts = <String>[];
    if (r.pulled) parts.add('已拉取云端');
    if (r.pushed) parts.add('已上传本机');
    if (parts.isEmpty) parts.add('云端和本机都已是最新');
    if (n('addedHosts') + n('updatedHosts') + n('deletedHosts') > 0) {
      parts.add(
        '主机 +${n('addedHosts')}/改${n('updatedHosts')}/删${n('deletedHosts')}',
      );
    }
    if (n('syncedNotes') > 0) parts.add('备注 ${n('syncedNotes')} 条');
    if (n('addedKnownHosts') + n('updatedKnownHosts') > 0) {
      parts.add('指纹 +${n('addedKnownHosts')}/覆盖${n('updatedKnownHosts')}');
    }
    return parts.join('；');
  }

  /* ------------------------------------------------------------------ */
  /* 随身密钥包                                                          */
  /* ------------------------------------------------------------------ */

  Widget _keyPack(
    BuildContext context,
    Bootstrap? bootstrap,
    List<LocalKeyEntry> privateKeys,
  ) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Text(
              '手机随身密钥包',
              style: TextStyle(
                fontSize: 14,
                fontWeight: FontWeight.w600,
                color: KwColors.text,
              ),
            ),
            const Spacer(),
            TextButton.icon(
              onPressed: () => _generateKey(context),
              icon: const Icon(Icons.add, size: 16),
              label: const Text('生成', style: TextStyle(fontSize: 12)),
            ),
          ],
        ),
        const SizedBox(height: 2),
        Text(
          '保存在应用沙盒（${bootstrap?.keysDir ?? '…'}），私钥 0600；'
          '支持 Ed25519 / RSA 3072 与 passphrase。',
          style: TextStyle(fontSize: 11, height: 1.5, color: KwColors.faint),
        ),
        Divider(height: 18, color: KwColors.border),
        if (privateKeys.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 10),
            child: Text(
              '还没有密钥。生成一把，或在服务器上「新增密钥」时自动写入这里。',
              style: TextStyle(fontSize: 12, color: KwColors.dim),
            ),
          )
        else
          ...privateKeys.map((k) => _keyRow(context, k)),
        if ((bootstrap?.scanErrors ?? const []).isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              '扫描警告：${bootstrap!.scanErrors.join('；')}',
              style: TextStyle(fontSize: 10.5, color: KwColors.warn),
            ),
          ),
      ],
    );
  }

  Widget _keyRow(BuildContext context, LocalKeyEntry key) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 9),
      child: Row(
        children: [
          Icon(
            key.encrypted ? Icons.lock_rounded : Icons.vpn_key_rounded,
            size: 16,
            color: key.encrypted ? KwColors.warn : KwColors.ok,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  key.name,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontFamily: 'monospace',
                    color: KwColors.text,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  '${key.type.isEmpty ? '未知类型' : key.type} · '
                  '${key.fingerprint.isEmpty ? '指纹未知' : shortFp(key.fingerprint)}'
                  '${key.encrypted ? ' · 已加密' : ''}',
                  style: TextStyle(
                    fontSize: 10.5,
                    fontFamily: 'monospace',
                    color: KwColors.dim,
                  ),
                ),
              ],
            ),
          ),
          if (key.fingerprint.isNotEmpty)
            IconButton(
              tooltip: '复制指纹',
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.copy_rounded, size: 15),
              color: KwColors.faint,
              onPressed: () => copyText(context, key.fingerprint, 'SHA256 指纹'),
            ),
        ],
      ),
    );
  }

  Future<void> _generateKey(BuildContext context) async {
    final label = TextEditingController(text: 'mobile');
    final comment = TextEditingController();
    final pass = TextEditingController();
    var algorithm = 'ed25519';
    final submitted = await showDialog<bool>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setLocal) => AlertDialog(
          title: const Text('生成随身密钥'),
          content: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                DropdownButtonFormField<String>(
                  initialValue: algorithm,
                  decoration: const InputDecoration(labelText: '算法'),
                  dropdownColor: KwColors.bgElev,
                  items: const [
                    DropdownMenuItem(
                      value: 'ed25519',
                      child: Text('Ed25519（推荐）'),
                    ),
                    DropdownMenuItem(value: 'rsa', child: Text('RSA 3072')),
                  ],
                  onChanged: (v) => setLocal(() => algorithm = v ?? 'ed25519'),
                ),
                const SizedBox(height: 12),
                field(label, '文件名前缀', hint: 'mobile'),
                field(comment, '注释', hint: 'mobile@phone'),
                field(pass, 'passphrase（可选）', obscure: true),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: const Text('生成'),
            ),
          ],
        ),
      ),
    );
    final l = label.text;
    final c = comment.text;
    final p = pass.text;
    label.dispose();
    comment.dispose();
    pass.dispose();
    if (submitted != true || !context.mounted) return;

    try {
      final key = await state.generateKey(
        algorithm: algorithm,
        label: l,
        comment: c,
        passphrase: p.isEmpty ? null : p,
      );
      if (!context.mounted) return;
      await showDialog<void>(
        context: context,
        builder: (ctx) => AlertDialog(
          title: const Text('已生成'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '${key['type']} · ${key['encrypted'] == true ? '已加密' : '未加密'}',
                style: TextStyle(fontSize: 13, color: KwColors.text),
              ),
              const SizedBox(height: 8),
              SelectableText(
                key['fingerprint']?.toString() ?? '',
                style: TextStyle(
                  fontSize: 11,
                  fontFamily: 'monospace',
                  color: KwColors.dim,
                ),
              ),
              const SizedBox(height: 8),
              Text(
                key['privateKeyPath']?.toString() ?? '',
                style: TextStyle(fontSize: 11, color: KwColors.faint),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () =>
                  copyText(ctx, key['publicKeyLine']?.toString() ?? '', '公钥'),
              child: const Text('复制公钥'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('完成'),
            ),
          ],
        ),
      );
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }
}

String _shortId(String id) {
  if (id.length <= 13) return id;
  return '${id.substring(0, 8)}…${id.substring(id.length - 4)}';
}

String _fmtTime(int ms) {
  if (ms <= 0) return '从未同步';
  final t = DateTime.fromMillisecondsSinceEpoch(ms);
  String two(int v) => v.toString().padLeft(2, '0');
  return '${t.year}-${two(t.month)}-${two(t.day)} ${two(t.hour)}:${two(t.minute)}';
}
