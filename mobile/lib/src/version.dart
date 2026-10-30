/// 手机端版本，与桌面端（`Cargo.toml`）**独立编号**。
///
/// 改这里时同步改 `mobile/pubspec.yaml` 的 `version:`：
///   - `versionName` 用日期（系统设置里显示的就是它，如 `2026.10.30`）
///   - `build` 是 `YYYYMMDDNN` 形式的 versionCode，**必须单调递增** ——
///     只改日期不涨序号，某些 ROM 会当成「相同版本」拒绝安装
///
/// 顶栏展示用这个常量，而不是后端 bootstrap 里的版本：那是 Rust 核心的号，
/// 跟手机 App 的发布节奏不是一回事。
const String kMobileVersion = 'v2026.10.30.1';
