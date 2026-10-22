'use strict';
/**
 * 版本号单一来源。
 *
 * 规则：`v<日期>.<当天第几次更新>` —— 例如 v2026.10.19.4 表示 2026-10-19 当天第 4 次更新。
 * 每次改动：先看 CHANGELOG 里今天已有的最大序号，+1，然后同步三处：
 *   1. 本文件 APP_VERSION
 *   2. CHANGELOG.md 新增一条
 *   3. package.json 的 version（semver 不认 4 段号，用 `2026.10.19-4` 这种写法）
 */

const APP_VERSION = 'v2026.10.22.1';

/** npm 用的 semver 兼容形式：把最后一个点换成短横线 */
const APP_VERSION_SEMVER = APP_VERSION.replace(/^v/, '').replace(/\.(\d+)$/, '-$1');

module.exports = { APP_VERSION, APP_VERSION_SEMVER };
