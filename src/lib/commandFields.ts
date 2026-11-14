/**
 * 命令输入栏用到的纯字符串处理。
 *
 * 为什么单独抽出来：分词要遵守 shell 的引号/转义语义，边界一旦算错，
 * 「历史命令字段选择」会切错字、「补全替换」会插错位置。纯函数才能用单测
 * 把这些边界钉死，组件里只管拼 UI 和发请求。
 */

/** 一个 shell 字段。`text` 是原文子串（保留引号与反斜杠），`start/end` 是原文下标（end 不含）。 */
export interface Field {
  text: string;
  start: number;
  end: number;
}

function isSpace(ch: string): boolean {
  return ch === " " || ch === "\t" || ch === "\n" || ch === "\r";
}

/**
 * 按 shell 语义切词。
 *
 * 引号内的空格不切；单引号内反斜杠是普通字符（shell 规则），双引号内和
 * 引号外的反斜杠会把下一个字符一起吞掉，所以 `a\ b` 是一个字段。
 * text 特意保留引号和转义符本身 —— 这样字段能原样插回输入框，不破坏命令语义
 * （若把 `"a b"` 去掉引号再插入，就会变成两个 token，命令直接走样）。
 */
export function splitFields(line: string): Field[] {
  const fields: Field[] = [];
  let i = 0;
  const n = line.length;

  while (i < n) {
    while (i < n && isSpace(line[i])) i += 1;
    if (i >= n) break;

    const start = i;
    let quote: "'" | '"' | "" = "";
    while (i < n) {
      const ch = line[i];
      // 单引号内一切字面量，不处理反斜杠；双引号内与引号外才认转义
      if (ch === "\\" && quote !== "'") {
        i += 2;
        continue;
      }
      if (quote) {
        if (ch === quote) quote = "";
        i += 1;
        continue;
      }
      if (ch === "'" || ch === '"') {
        quote = ch;
        i += 1;
        continue;
      }
      if (isSpace(ch)) break;
      i += 1;
    }
    fields.push({ text: line.slice(start, i), start, end: i });
  }

  return fields;
}

/**
 * 取光标处正在输入的那个词头（用于补全）。
 *
 * 只看光标之前的文本：光标正好落在某个字段末尾时把它当词头；字段后已经有
 * 空白说明用户在敲下一个词，返回空词头（start=end=光标）。返回的 start/end
 * 是完整 line 的下标，可直接交给 replaceToken 做整段替换。
 */
export function currentToken(
  line: string,
  cursor: number,
): { text: string; start: number; end: number } {
  const at = Math.max(0, Math.min(cursor, line.length));
  const head = line.slice(0, at);
  const fields = splitFields(head);
  const last = fields[fields.length - 1];
  if (last && last.end === at) {
    return { text: last.text, start: last.start, end: last.end };
  }
  return { text: "", start: at, end: at };
}

/** 用 replacement 替换 [start,end) 段，返回新文本和应该落下的光标位置。 */
export function replaceToken(
  line: string,
  start: number,
  end: number,
  replacement: string,
): { text: string; cursor: number } {
  return {
    text: line.slice(0, start) + replacement + line.slice(end),
    cursor: start + replacement.length,
  };
}

/** 末尾追加一个字段；只有原文末尾没有空白时才补一个空格。 */
export function appendField(line: string, field: string): string {
  if (!field) return line;
  if (!line) return field;
  return isSpace(line[line.length - 1]) ? line + field : `${line} ${field}`;
}

/** 按 groups 顺序合并候选：跨组按 value 去重，先出现的保留其来源标签，最多 limit 条。 */
export function mergeCandidates(
  groups: { label: string; values: string[] }[],
  limit: number,
): { label: string; value: string }[] {
  const out: { label: string; value: string }[] = [];
  const seen = new Set<string>();
  for (const group of groups) {
    for (const value of group.values) {
      if (out.length >= limit) return out;
      if (!value || seen.has(value)) continue;
      seen.add(value);
      out.push({ label: group.label, value });
    }
  }
  return out;
}

/**
 * 拼出交给 path_complete 的 prefix。
 * 绝对路径原样用；相对词头拼到 cwd 下。cwd 是根目录（"/"）时不能拼出 "//"。
 */
export function resolvePathPrefix(cwd: string, token: string): string {
  if (token.startsWith("/")) return token;
  const base = cwd.replace(/\/+$/, "");
  if (!token) return base || "/";
  return base ? `${base}/${token}` : `/${token}`;
}
