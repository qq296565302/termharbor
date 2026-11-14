import { describe, expect, it } from "vitest";

import {
  appendField,
  currentToken,
  mergeCandidates,
  replaceToken,
  resolvePathPrefix,
  splitFields,
} from "./commandFields";

describe("splitFields", () => {
  it("普通分词并保留原文下标", () => {
    const line = "ls -la /tmp";
    const fields = splitFields(line);
    expect(fields.map((f) => f.text)).toEqual(["ls", "-la", "/tmp"]);
    expect(fields[0]).toEqual({ text: "ls", start: 0, end: 2 });
    expect(fields[2]).toEqual({ text: "/tmp", start: 7, end: 11 });
    // text 必须恒等于原文切片，字段选择才能原样插回输入框
    for (const f of fields) expect(f.text).toBe(line.slice(f.start, f.end));
  });

  it("单引号把整段当一个字段：grep -E 是 3 个字段", () => {
    expect(splitFields("grep -E 'a|b'").map((f) => f.text)).toEqual(["grep", "-E", "'a|b'"]);
  });

  it('双引号内含空格不切：echo "a b" c 是 3 个字段', () => {
    expect(splitFields('echo "a b" c').map((f) => f.text)).toEqual(["echo", '"a b"', "c"]);
  });

  it("反斜杠转义的空格不切", () => {
    expect(splitFields("cp a\\ b c").map((f) => f.text)).toEqual(["cp", "a\\ b", "c"]);
  });

  it("双引号内的转义引号不提前闭合", () => {
    expect(splitFields('echo "a\\" b" c').map((f) => f.text)).toEqual([
      "echo",
      '"a\\" b"',
      "c",
    ]);
  });

  it("单引号内的反斜杠是字面量，不吞掉后面的引号", () => {
    expect(splitFields("echo 'a\\ b'").map((f) => f.text)).toEqual(["echo", "'a\\ b'"]);
  });

  it("空串与全空白都没有字段", () => {
    expect(splitFields("")).toEqual([]);
    expect(splitFields("   \t  ")).toEqual([]);
  });

  it("连续多个空格只当作一个分隔", () => {
    expect(splitFields("  ls   -la  ").map((f) => f.text)).toEqual(["ls", "-la"]);
  });
});

describe("currentToken", () => {
  it("光标在末尾取最后一个 token", () => {
    expect(currentToken("ls /tm", 6)).toEqual({ text: "/tm", start: 3, end: 6 });
  });

  it("光标在中间取它前面的 token", () => {
    expect(currentToken("echo hi world", 7)).toEqual({ text: "hi", start: 5, end: 7 });
  });

  it("光标紧跟空白时返回空词头，位置就是光标", () => {
    expect(currentToken("ls  ", 4)).toEqual({ text: "", start: 4, end: 4 });
  });

  it("空输入返回零长度词头", () => {
    expect(currentToken("", 0)).toEqual({ text: "", start: 0, end: 0 });
  });

  it("光标超出长度会被夹住", () => {
    expect(currentToken("ls", 99)).toEqual({ text: "ls", start: 0, end: 2 });
  });
});

describe("replaceToken", () => {
  it("替换后文本正确且光标落在替换内容之后", () => {
    expect(replaceToken("ls /tm", 3, 6, "/tmp")).toEqual({ text: "ls /tmp", cursor: 7 });
  });

  it("与 currentToken 配合完成一次补全", () => {
    const line = "cd /et";
    const token = currentToken(line, line.length);
    const result = replaceToken(line, token.start, token.end, "/etc");
    expect(result.text).toBe("cd /etc");
    expect(result.cursor).toBe(7);
  });

  it("空词头时是纯插入", () => {
    expect(replaceToken("ls ", 3, 3, "-la")).toEqual({ text: "ls -la", cursor: 6 });
  });
});

describe("appendField", () => {
  it("空串追加直接返回字段", () => {
    expect(appendField("", "ls")).toBe("ls");
  });

  it("已有内容时补一个空格", () => {
    expect(appendField("ls -la", "/tmp")).toBe("ls -la /tmp");
  });

  it("末尾已有空白不重复补空格", () => {
    expect(appendField("ls ", "-la")).toBe("ls -la");
  });

  it("空字段不改变原文", () => {
    expect(appendField("ls -la", "")).toBe("ls -la");
  });
});

describe("mergeCandidates", () => {
  it("跨组按 value 去重，保留首次出现的来源标签", () => {
    const result = mergeCandidates(
      [
        { label: "历史命令", values: ["ls -la", "cd /tmp"] },
        { label: "访问过的路径", values: ["/tmp", "ls -la"] },
      ],
      30,
    );
    expect(result).toEqual([
      { label: "历史命令", value: "ls -la" },
      { label: "历史命令", value: "cd /tmp" },
      { label: "访问过的路径", value: "/tmp" },
    ]);
  });

  it("按 limit 截断", () => {
    const result = mergeCandidates([{ label: "历史命令", values: ["a", "b", "c", "d"] }], 2);
    expect(result.map((r) => r.value)).toEqual(["a", "b"]);
  });

  it("limit 为 0 时返回空", () => {
    expect(mergeCandidates([{ label: "x", values: ["a"] }], 0)).toEqual([]);
  });

  it("忽略空 value", () => {
    expect(mergeCandidates([{ label: "x", values: ["", "a"] }], 10)).toEqual([
      { label: "x", value: "a" },
    ]);
  });
});

describe("resolvePathPrefix", () => {
  it("根目录 + 相对词头不产生 //", () => {
    expect(resolvePathPrefix("/", "etc")).toBe("/etc");
    expect(resolvePathPrefix("/", "")).toBe("/");
  });

  it("绝对路径不被 cwd 覆盖", () => {
    expect(resolvePathPrefix("/home/u", "/var/log")).toBe("/var/log");
  });

  it("相对词头拼到 cwd 下，并去掉 cwd 尾部斜杠", () => {
    expect(resolvePathPrefix("/home/u/", "proj")).toBe("/home/u/proj");
  });

  it("cwd 为空时退回根", () => {
    expect(resolvePathPrefix("", "proj")).toBe("/proj");
  });
});
