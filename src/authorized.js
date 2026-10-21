'use strict';
/**
 * authorized_keys 解析 / 序列化
 *
 * 设计原则：**不重排、不美化、不丢行**。解析只用于展示与定位，
 * 落盘时按原始文本行做增删，避免把用户服务器上的特殊格式改坏。
 */

const { parsePublicKeyLine } = require('./keygen');

/**
 * @param {string} text 文件原文（可为空字符串 / null）
 * @returns {{ items: Array, rawLines: string[], existed: boolean }}
 */
function parseAuthorizedKeys(text) {
  const src = typeof text === 'string' ? text : '';
  const rawLines = src.length ? src.split(/\r?\n/) : [];
  // 末尾换行会产生一个空元素，标记出来，序列化时还原
  const endsWithNewline = /\r?\n$/.test(src);
  if (endsWithNewline && rawLines.length) rawLines.pop();

  const items = [];
  const seen = new Map();

  rawLines.forEach((raw, index) => {
    const line = raw.trim();
    if (!line) {
      items.push({ index, raw, kind: 'blank' });
      return;
    }
    if (line.startsWith('#')) {
      items.push({ index, raw, kind: 'comment', text: line.replace(/^#+\s*/, '') });
      return;
    }
    const parsed = parsePublicKeyLine(line);
    if (!parsed) {
      items.push({ index, raw, kind: 'malformed', reason: '无法识别的行格式（已原样保留）' });
      return;
    }
    const first = seen.get(parsed.fingerprint);
    if (first === undefined) seen.set(parsed.fingerprint, index);
    items.push({
      index,
      raw,
      kind: 'key',
      ...parsed,
      duplicateOf: first === undefined || first === index ? null : first,
    });
  });

  return { items, rawLines, endsWithNewline, existed: src.length > 0 };
}

/** 只取可用的密钥条目 */
function keyItems(items) {
  return items.filter((i) => i.kind === 'key');
}

/**
 * 按索引删除后重新拼装文本
 * @param {Array} parsed parseAuthorizedKeys 的结果
 * @param {number[]} dropIndexes 要删除的行索引
 */
function renderWithout(parsed, dropIndexes) {
  const drop = new Set(dropIndexes);
  const kept = parsed.rawLines.filter((_, i) => !drop.has(i));
  if (!kept.length) return '';
  return kept.join('\n') + '\n';
}

/**
 * 追加一行公钥；若指纹已存在则返回 unchanged
 * @returns {{ text: string, appended: boolean, reason?: string }}
 */
function appendKey(parsed, publicKeyLine) {
  const parsedLine = parsePublicKeyLine(publicKeyLine);
  if (!parsedLine) throw new Error('公钥格式无法识别，拒绝写入');
  if (!parsedLine.valid) throw new Error(`公钥无效：${parsedLine.error || '结构校验失败'}`);

  const exists = keyItems(parsed.items).find((i) => i.fingerprint === parsedLine.fingerprint);
  if (exists) {
    return { text: null, appended: false, reason: '该公钥已在服务器上（指纹相同），未重复写入', fingerprint: parsedLine.fingerprint };
  }

  const body = parsed.rawLines.join('\n');
  const text = body.length ? `${body}\n${publicKeyLine}\n` : `${publicKeyLine}\n`;
  return { text, appended: true, fingerprint: parsedLine.fingerprint, entry: parsedLine };
}

module.exports = { parseAuthorizedKeys, keyItems, renderWithout, appendKey };
