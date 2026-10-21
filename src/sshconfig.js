'use strict';
/**
 * 从 ~/.ssh/config 里读出现成的主机配置，首次启动时自动导入，
 * 省得用户对着配置文件手抄 HostName / User / IdentityFile。
 */

const fs = require('fs');
const os = require('os');
const path = require('path');

function expandTilde(p, home = os.homedir()) {
  if (!p) return p;
  if (p === '~') return home;
  if (p.startsWith('~/') || p.startsWith('~\\')) return path.join(home, p.slice(2));
  return p;
}

/**
 * @param {string} text ssh_config 文本
 * @returns {Array<{name,host,port,username,identityFile,remotePath}>}
 */
function parseSshConfig(text, home = os.homedir()) {
  const hosts = [];
  let cur = null;

  for (const rawLine of String(text || '').split(/\r?\n/)) {
    const line = rawLine.replace(/#.*$/, '').trim();
    if (!line) continue;

    const m = line.match(/^(\S+)\s+(.*)$/);
    if (!m) continue;

    const key = m[1].toLowerCase();
    const rawVal = m[2].trim();
    const val = rawVal.replace(/^"(.*)"$/, '$1');

    if (key === 'host') {
      // 含通配符的段（Host *）不是具体主机，跳过
      const names = val.split(/\s+/).filter((n) => n && !/[*?!]/.test(n));
      if (!names.length) {
        cur = null;
        continue;
      }
      cur = { name: names[0], host: '', port: 22, username: '', identityFile: '', remotePath: '' };
      hosts.push(cur);
      continue;
    }

    if (!cur) continue;
    if (key === 'hostname') cur.host = val;
    else if (key === 'user') cur.username = val;
    else if (key === 'port') cur.port = Number(val) || 22;
    else if (key === 'identityfile') cur.identityFile = expandTilde(val, home);
  }

  return hosts
    .map((h) => ({ ...h, host: h.host || h.name }))
    .filter((h) => h.host && h.username);
}

function importFromSshConfig(file = path.join(os.homedir(), '.ssh', 'config')) {
  try {
    if (!fs.existsSync(file)) return [];
    return parseSshConfig(fs.readFileSync(file, 'utf8'));
  } catch {
    return [];
  }
}

module.exports = { parseSshConfig, importFromSshConfig, expandTilde };
