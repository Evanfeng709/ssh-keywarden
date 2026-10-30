'use strict';
/**
 * 服务器 sshd 登录方式：检测与切换「仅密钥登录」。
 *
 * 注意两件事：
 *  1. 光把 PasswordAuthentication 设成 no 不够 —— 现代 sshd 还能通过
 *     KbdInteractiveAuthentication（PAM）用密码登录，必须一起关。
 *  2. 改 sshd_config 会直接影响能不能再登进来，所以调用方必须先过安全闸门。
 */

const ssh = require('./ssh');

const CONFIG_PATH = '/etc/ssh/sshd_config';

const WANTED = [
  'passwordauthentication',
  'pubkeyauthentication',
  'permitrootlogin',
  'kbdinteractiveauthentication',
  'challengeresponseauthentication',
];

/** 从一份 sshd_config 文本里取出关心的项（同一文件内首次出现生效） */
function parseConfigText(text) {
  const values = {};
  for (const raw of String(text || '').split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const m = line.match(/^([A-Za-z]+)[\s=]+(.+)$/);
    if (!m) continue;
    const key = m[1].toLowerCase();
    if (!WANTED.includes(key)) continue;
    if (values[key] === undefined) values[key] = m[2].trim().toLowerCase();
  }
  return values;
}

/**
 * 汇总判定。默认值按 OpenSSH 的约定：
 * PasswordAuthentication / PubkeyAuthentication / KbdInteractiveAuthentication 默认 yes。
 */
function summarize(values) {
  const passwordExplicit = values.passwordauthentication;
  const kbdExplicit = values.kbdinteractiveauthentication;
  const chalExplicit = values.challengeresponseauthentication;

  const passwordAuth = passwordExplicit !== 'no';
  // 键盘交互（PAM）若开着，即使 PasswordAuthentication no 也可能用密码登进来
  const kbdInteractive = kbdExplicit === undefined ? true : kbdExplicit !== 'no';
  // ChallengeResponseAuthentication 是历史选项，现代 sshd 已被 KbdInteractive 取代，
  // 只有显式写成 yes 才算它开着
  const challengeResponse = chalExplicit === undefined ? false : chalExplicit !== 'no';
  const pubkeyAuth = values.pubkeyauthentication !== 'no';

  const pwAllowed = passwordAuth || kbdInteractive || challengeResponse;

  // 四种状态，别用一句话糊弄：
  //   both          公钥和密码都能登（默认）
  //   key-only      只认密钥（目标状态）
  //   password-only 只认密码（钥匙被关了，等于降级）
  //   none          两个都关了（谁都进不来）
  let state;
  if (pubkeyAuth && pwAllowed) state = 'both';
  else if (pubkeyAuth && !pwAllowed) state = 'key-only';
  else if (!pubkeyAuth && pwAllowed) state = 'password-only';
  else state = 'none';

  return {
    passwordAuth,
    pubkeyAuth,
    kbdInteractive,
    challengeResponse,
    pwAllowed,
    state,
    keyOnly: state === 'key-only',
    rootLogin: values.permitrootlogin || 'prohibit-password',
    explicit: {
      passwordAuth: passwordExplicit === undefined ? null : passwordExplicit,
      pubkeyAuth: values.pubkeyauthentication === undefined ? null : values.pubkeyauthentication,
      kbdInteractive: kbdExplicit === undefined ? null : kbdExplicit,
    },
  };
}

/** 检测服务器当前的登录方式 */
async function inspect(client) {
  const script = [
    'echo "###UID $(id -u 2>/dev/null)"',
    'echo "###SUDO $(sudo -n true 2>/dev/null && echo yes || echo no)"',
    `echo "###EXISTS $([ -f ${CONFIG_PATH} ] && echo yes || echo no)"`,
    `echo "###READ $([ -r ${CONFIG_PATH} ] && echo yes || echo no)"`,
    `if [ -r ${CONFIG_PATH} ]; then echo "###BEGIN main"; cat ${CONFIG_PATH} 2>/dev/null; fi`,
    'for f in /etc/ssh/sshd_config.d/*.conf; do',
    '  [ -f "$f" ] || continue',
    '  [ -r "$f" ] || continue',
    '  echo "###BEGIN $f"',
    '  cat "$f" 2>/dev/null',
    'done',
    'echo "###END"',
  ].join('\n');

  const b64 = Buffer.from(script, 'utf8').toString('base64');
  const res = await ssh.exec(client, `echo '${b64}' | base64 -d | sh -s`, { timeout: 20000 });

  let uid = '';
  let sudo = false;
  let exists = false;
  let readable = false;
  const sections = [];
  let cur = null;

  for (const line of res.stdout.split(/\r?\n/)) {
    if (line.startsWith('###UID ')) {
      uid = line.slice(7).trim();
      continue;
    }
    if (line.startsWith('###SUDO ')) {
      sudo = line.slice(8).trim() === 'yes';
      continue;
    }
    if (line.startsWith('###EXISTS ')) {
      exists = line.slice(10).trim() === 'yes';
      continue;
    }
    if (line.startsWith('###READ ')) {
      readable = line.slice(8).trim() === 'yes';
      continue;
    }
    if (line.startsWith('###BEGIN ')) {
      cur = { file: line.slice(8).trim(), lines: [] };
      sections.push(cur);
      continue;
    }
    if (line.startsWith('###END')) {
      cur = null;
      continue;
    }
    if (cur) cur.lines.push(line);
  }

  const isRoot = uid === '0';
  const canWrite = isRoot || sudo;

  if (!exists) {
    return {
      available: false,
      reason: `服务器上找不到 ${CONFIG_PATH}`,
      isRoot,
      canWrite,
      sudo,
    };
  }
  if (!readable) {
    return {
      available: false,
      reason: `没有读取 ${CONFIG_PATH} 的权限`,
      isRoot,
      canWrite,
      sudo,
    };
  }

  // drop-in 通常通过 Include 在主配置顶部生效，所以后者覆盖前者
  const merged = {};
  const source = {};
  for (const sec of sections) {
    const values = parseConfigText(sec.lines.join('\n'));
    for (const [k, v] of Object.entries(values)) {
      merged[k] = v;
      source[k] = sec.file;
    }
  }

  const summary = summarize(merged);
  return {
    available: true,
    ...summary,
    files: sections.map((s) => s.file),
    source,
    configPath: sections[0] ? sections[0].file : CONFIG_PATH,
    isRoot,
    canWrite,
    sudo,
  };
}

/**
 * 切换「仅密钥登录」。
 * 同时改 PasswordAuthentication 与 KbdInteractiveAuthentication —— 只改前者会留后门。
 * @param {boolean} enable true=仅密钥登录；false=重新允许密码登录
 */
async function setKeyOnly(client, enable, { canWrite, isRoot, sudo } = {}) {
  const pwVal = enable ? 'no' : 'yes';
  const kbdVal = enable ? 'no' : 'yes';
  // 开启「仅密钥登录」时必须顺手确保公钥登录是开的，
  // 否则 pubkey=no + password=no 会变成「谁也进不去」
  const pubVal = enable ? 'yes' : '';

  const script = `#!/bin/sh
set -e
CFG=${CONFIG_PATH}
PW="${pwVal}"
KBD="${kbdVal}"
PUB="${pubVal}"
STAMP=$(date +%Y%m%d%H%M%S)
BACKUPS=""

[ -f "$CFG" ] || { echo "__ERR__ 找不到 $CFG"; exit 1; }

# append=yes → 替换已有行 + 没有就追加（主配置必须出现这三条）
# append=no  → 只替换已有行（drop-in 里没有的项不碰，让主配置的设置生效）
modify_file() {
  f="$1"; append="$2"
  cp -p "$f" "$f.bak.$STAMP" || { echo "__ERR__ 备份 $f 失败，已中止"; exit 1; }
  BACKUPS="$BACKUPS $f.bak.$STAMP"
  T=$(mktemp)
  awk -v pw="$PW" -v kbd="$KBD" -v pub="$PUB" -v ap="$append" '
    BEGIN { dp = 0; dk = 0; dpu = 0 }
    tolower($1) == "passwordauthentication" {
      if (!dp) { print "PasswordAuthentication " pw; dp = 1; next }
    }
    tolower($1) == "kbdinteractiveauthentication" || tolower($1) == "challengeresponseauthentication" {
      if (!dk) { print "KbdInteractiveAuthentication " kbd; dk = 1; next }
    }
    pub != "" && tolower($1) == "pubkeyauthentication" {
      if (!dpu) { print "PubkeyAuthentication " pub; dpu = 1; next }
    }
    { print }
    END {
      if (ap == "yes") {
        if (!dp)  print "PasswordAuthentication " pw
        if (!dk)  print "KbdInteractiveAuthentication " kbd
        if (pub != "" && !dpu) print "PubkeyAuthentication " pub
      }
    }
  ' "$f" > "$T"
  cat "$T" > "$f"   # 用 cat 覆盖，保留原文件的权限与所有者
  rm -f "$T"
}

modify_file "$CFG" yes
chmod 600 "$CFG" 2>/dev/null || true

# drop-in（云厂商的 50-cloud-init.conf 之类）若写了这些键，会因 Include 在主配置顶部而抢先生效，必须一并改掉
for f in /etc/ssh/sshd_config.d/*.conf; do
  [ -f "$f" ] || continue
  if grep -qiE '^[[:space:]]*(passwordauthentication|kbdinteractiveauthentication|challengeresponseauthentication|pubkeyauthentication)[[:space:]]' "$f"; then
    modify_file "$f" no
  fi
done

ERR_LOG=$(mktemp)
if ! sshd -t 2>"$ERR_LOG"; then
  echo "__ERR__ 新配置语法校验未通过，已全部回滚"
  cat "$ERR_LOG" 2>/dev/null || true   # set -e 下 cat 失败会中断回滚，必须兜住
  rm -f "$ERR_LOG" 2>/dev/null || true
  for b in $BACKUPS; do
    cp -p "$b" "\${b%.bak.\$STAMP}"
  done
  exit 1
fi
rm -f "$ERR_LOG" 2>/dev/null || true

RELOADED="no"
if command -v systemctl >/dev/null 2>&1; then
  systemctl reload sshd 2>/dev/null && RELOADED="yes" || true
  [ "$RELOADED" = "no" ] && systemctl reload ssh 2>/dev/null && RELOADED="yes" || true
fi
if [ "$RELOADED" = "no" ] && command -v service >/dev/null 2>&1; then
  service ssh reload 2>/dev/null && RELOADED="yes" || true
  [ "$RELOADED" = "no" ] && service sshd reload 2>/dev/null && RELOADED="yes" || true
fi

echo "__OK__ $CFG.bak.$STAMP $RELOADED"
`;

  const b64 = Buffer.from(script, 'utf8').toString('base64');
  const prefix = isRoot ? '' : 'sudo -n ';
  const res = await ssh.exec(client, `echo '${b64}' | base64 -d | ${prefix}sh -s`, { timeout: 30000 });

  const out = `${res.stdout}\n${res.stderr}`.trim();

  if (out.includes('__OK__')) {
    const m = out.match(/__OK__\s+(\S+)\s+(\S+)/);
    return {
      ok: true,
      backup: m ? m[1] : '',
      reloaded: m ? m[2] === 'yes' : false,
    };
  }

  const errLine = out.split(/\r?\n/).find((l) => l.includes('__ERR__')) || out;
  throw new Error(errLine.replace('__ERR__', '').trim() || '修改 sshd 配置失败');
}

module.exports = { inspect, setKeyOnly, parseConfigText, summarize, CONFIG_PATH };
