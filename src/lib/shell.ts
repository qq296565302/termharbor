/**
 * 从终端里「看」用户在敲什么，用来让文件面板跟着 shell 的当前目录走。
 *
 * 为什么不是读 shell 的真实 cwd：SSH 协议里拿不到对端 shell 的 pid，
 * OSC 7 又要用户自己在 .bashrc/.zshrc 里配。所以退一步 —— 只认用户敲出来的行，
 * 靠「解析 + 远端验证」保证不错跳：解析出来的路径必须在 SFTP 上真能列出来才切。
 *
 * 已知认不出的情况（一律不跳，宁可不动）：`cd` 别名、`pushd`、子 shell、
 * `cd $(xxx)` 这类带变量/命令替换的、`~user`。方向键翻历史之后也认不出
 * —— 那时本地记录的行和真实命令行已经不一致了，标记成 dirty 直接放弃。
 */

/** 终端里当前正在输入的那一行。`dirty` 表示"看不见真实内容了，别猜"。 */
export interface LineState {
  text: string;
  dirty: boolean;
}

export const EMPTY_LINE: LineState = { text: "", dirty: false };

/** 一行最多记这么多字符。长粘贴（比如整段脚本）没必要记全，超了就当不可知。 */
const MAX_LINE = 4096;

/**
 * 喂一段用户输入（xterm 的 `onData` 原样给过来的字节），返回新的行状态和
 * 「这一下刚好提交了哪几行」。submit 里是空串时表示这行内容不可知。
 */
export function feedLine(
  state: LineState,
  data: string,
): { state: LineState; submitted: string[] } {
  let text = state.text;
  let dirty = state.dirty;
  const submitted: string[] = [];

  // 远端开了括号粘贴（bracketed paste）时，粘贴内容会被 `ESC[200~ … ESC[201~` 包起来。
  // 不剥掉的话，那个 ESC 会把整行判成"不可知"，粘贴的 `cd /xxx` 就永远跟不了。
  for (const ch of data.replace(/\x1b\[20[01]~/g, "")) {
    if (ch === "\r" || ch === "\n") {
      submitted.push(dirty ? "" : text);
      text = "";
      dirty = false;
    } else if (ch === "\x7f" || ch === "\b") {
      // 退格
      text = text.slice(0, -1);
    } else if (ch === "\x03" || ch === "\x15") {
      // Ctrl+C / Ctrl+U：整行作废
      text = "";
      dirty = false;
    } else if (ch === "\t") {
      // Tab 补全：补出来的字符本地看不到。忽略这一下，但不弄脏 ——
      // 没补全的普通 `cd /tmp` 还能正常认出来。
    } else if (ch < " ") {
      // 其余控制字符（含方向键的 ESC 序列开头）：命令内容已经不可知
      dirty = true;
    } else {
      if (text.length >= MAX_LINE) {
        dirty = true;
      } else {
        text += ch;
      }
    }
  }

  return { state: { text, dirty }, submitted };
}

/**
 * 从一行命令里认出 `cd`，返回它的参数原样（可能是 `~`、`-`、相对、绝对）。
 *
 * 返回 null 表示"这行不是 cd"。带引号的路径会被去掉引号。
 */
export function parseCd(line: string): string | null {
  const head = firstSegment(line).trim();
  // `cd  /tmp` 这种多空格也算；`cdrecord` 不算
  const match = /^cd(?:\s+(.*))?$/.exec(head);
  if (!match) return null;

  const rest = (match[1] ?? "").trim();
  // 裸 `cd` 是回家目录
  if (!rest) return "~";
  return firstArgument(rest);
}

/**
 * 取第一个命令：截断到第一个没被引号包住的 `|`、`;`、`&`。
 *
 * 不这么做的后果很直观：`cd /tmp && ls` 被当成整个参数，路径里带个 `&&` 就列不出来。
 */
function firstSegment(line: string): string {
  let quote = "";
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === "|" || ch === ";" || ch === "&") {
      return line.slice(0, i);
    }
  }
  return line;
}

/** 取第一个参数，支持 `"带 空格"` 与 `'单引号'`。 */
function firstArgument(rest: string): string {
  const quote = rest[0];
  if (quote === '"' || quote === "'") {
    const end = rest.indexOf(quote, 1);
    return end < 0 ? rest.slice(1) : rest.slice(1, end);
  }
  const space = rest.search(/\s/);
  return space < 0 ? rest : rest.slice(0, space);
}

/**
 * 把 `cd` 的参数算成绝对路径。算不出来（`-`、`~user`、带变量/通配符）返回 null。
 *
 * `cwd` 传 shell 当前的目录（不是面板当前显示的目录 —— 面板可以自己点着逛，
 * 那是两回事）。
 */
export function resolveCdPath(target: string, cwd: string, home: string): string | null {
  // `cd -` 要去查 OLDPWD，读不到；变量展开、通配符同理，一律不猜
  if (!target || target === "-") return null;
  if (/[$`*?{}]/.test(target)) return null;

  if (target === "~") return normalizePath(home || "/");
  if (target.startsWith("~/")) return normalizePath(`${home || ""}/${target.slice(2)}`);
  if (target.startsWith("~")) return null; // ~user
  if (target.startsWith("/")) return normalizePath(target);

  return normalizePath(`${cwd || "/"}/${target}`);
}

/**
 * 归一化远端路径：折叠 `.`、`..` 与重复斜杠，保留开头的 `/`。
 *
 * Windows OpenSSH 的路径长这样：`/C:/Users/me`，第一段是盘符，`..` 到它就不再往上了。
 */
export function normalizePath(path: string): string {
  const parts: string[] = [];

  for (const part of path.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      const last = parts[parts.length - 1];
      // 盘符段不能被弹掉，否则 `/C:/..` 会跑到根
      if (parts.length > 0 && !last.endsWith(":")) {
        parts.pop();
        continue;
      }
      continue;
    }
    parts.push(part);
  }

  return `/${parts.join("/")}`;
}
