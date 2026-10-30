package io.github.evanfeng709.kw_mobile

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * 只有一条通道：kw_mobile/paths，给 Rust 侧提供可写目录（下面建 data/ 与 keys/）。
 *
 * 刻意不引 path_provider：Windows 构建插件需要开发者模式开符号链接，
 * 而一段平台代码就够了。
 */
class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "kw_mobile/paths")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "appSupportDir" -> result.success(filesDir.absolutePath)
                    else -> result.notImplemented()
                }
            }
    }
}
