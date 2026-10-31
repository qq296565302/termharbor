import { describe, expect, it } from "vitest";

import {
  EMPTY_LINE,
  feedLine,
  normalizePath,
  parseCd,
  resolveCdPath,
  type LineState,
} from "./shell";

describe("feedLine", () => {
  it("回车把这一行交出来", () => {
    const first = feedLine(EMPTY_LINE, "cd /tmp");
    expect(first.submitted).toEqual([]);
    const second = feedLine(first.state, "\r");
    expect(second.submitted).toEqual(["cd /tmp"]);
    expect(second.state.text).toBe("");
  });

  it("退格删掉最后一个字符", () => {
    let state: LineState = EMPTY_LINE;
    for (const ch of "cd /tmpX\x7f") {
      state = feedLine(state, ch).state;
    }
    expect(state.text).toBe("cd /tmp");
  });

  it("Ctrl+C 与 Ctrl+U 清空当前行", () => {
    expect(feedLine({ text: "cd /tmp", dirty: false }, "\x03").state.text).toBe("");
    expect(feedLine({ text: "cd /tmp", dirty: false }, "\x15").state.text).toBe("");
  });

  it("按过方向键之后这一行就不可信了", () => {
    // 上箭头：ESC [ A，随后可能补出历史命令，本地拼不出真实内容
    let state: LineState = EMPTY_LINE;
    for (const ch of "\x1b[A") state = feedLine(state, ch).state;
    expect(state.dirty).toBe(true);
    expect(feedLine(state, "\r").submitted).toEqual([""]);
  });

  it("Tab 补全不弄脏这一行，也不把它算成字符", () => {
    let state: LineState = EMPTY_LINE;
    for (const ch of "cd /tm\t") state = feedLine(state, ch).state;
    expect(state).toEqual({ text: "cd /tm", dirty: false });
  });

  it("一次粘贴多行会交出多行", () => {
    const { submitted } = feedLine(EMPTY_LINE, "cd /a\rcd /b\r");
    expect(submitted).toEqual(["cd /a", "cd /b"]);
  });

  it("括号粘贴的标记不把这一行判成不可知", () => {
    // 远端开了 bracketed paste 时，xterm 会把粘贴内容包在 ESC[200~ / ESC[201~ 里
    const { submitted, state } = feedLine(EMPTY_LINE, "\x1b[200~cd /tmp\x1b[201~\r");
    expect(submitted).toEqual(["cd /tmp"]);
    expect(state).toEqual({ text: "", dirty: false });
  });
});

describe("parseCd", () => {
  it("认出普通 cd", () => {
    expect(parseCd("cd /tmp")).toBe("/tmp");
    expect(parseCd("  cd   /var/log  ")).toBe("/var/log");
  });

  it("裸 cd 等于回家目录", () => {
    expect(parseCd("cd")).toBe("~");
    expect(parseCd("cd   ")).toBe("~");
  });

  it("带引号的路径去掉引号", () => {
    expect(parseCd('cd "/var/my dir"')).toBe("/var/my dir");
    expect(parseCd("cd '/var/my dir'")).toBe("/var/my dir");
  });

  it("只看第一个命令段", () => {
    expect(parseCd("cd /tmp && ls")).toBe("/tmp");
    expect(parseCd("cd /tmp; ls -l")).toBe("/tmp");
    expect(parseCd("cd /tmp | tee x")).toBe("/tmp");
  });

  it("不是 cd 的一律返回 null", () => {
    expect(parseCd("ls -l /tmp")).toBeNull();
    expect(parseCd("echo cd /tmp")).toBeNull();
    expect(parseCd("sudo cd /tmp")).toBeNull();
    expect(parseCd("cdrecord -v")).toBeNull();
    expect(parseCd("")).toBeNull();
  });

  it("引号里的 && 不当作命令分隔", () => {
    expect(parseCd('cd "/tmp/a && b"')).toBe("/tmp/a && b");
  });
});

describe("resolveCdPath", () => {
  const home = "/home/dev";

  it("绝对路径直接用", () => {
    expect(resolveCdPath("/tmp", "/home/dev", home)).toBe("/tmp");
    expect(resolveCdPath("/", "/home/dev", home)).toBe("/");
  });

  it("相对路径基于 shell 当前目录", () => {
    expect(resolveCdPath("logs", "/var", home)).toBe("/var/logs");
    expect(resolveCdPath("..", "/var/log", home)).toBe("/var");
    expect(resolveCdPath("../../etc", "/var/log", home)).toBe("/etc");
  });

  it("相对路径基于的是 shell 的 cwd，不是面板正在看的目录", () => {
    expect(resolveCdPath("sub", "/tmp", home)).toBe("/tmp/sub");
  });

  it("~ 与 ~/x 按家目录展开", () => {
    expect(resolveCdPath("~", "/tmp", home)).toBe(home);
    expect(resolveCdPath("~/logs", "/tmp", home)).toBe("/home/dev/logs");
  });

  it("算不准的一律返回 null", () => {
    expect(resolveCdPath("-", "/tmp", home)).toBeNull();
    expect(resolveCdPath("~other", "/tmp", home)).toBeNull();
    expect(resolveCdPath("$HOME", "/tmp", home)).toBeNull();
    expect(resolveCdPath("$(pwd)/x", "/tmp", home)).toBeNull();
    expect(resolveCdPath("*", "/tmp", home)).toBeNull();
  });

  it("Windows OpenSSH 的 /C:/ 形式也能归一化", () => {
    expect(resolveCdPath("/C:/Users", "/", "C:/Users/me")).toBe("/C:/Users");
    expect(resolveCdPath("..", "/C:/Users", "C:/Users/me")).toBe("/C:");
  });
});

describe("normalizePath", () => {
  it("折叠重复斜杠与 .", () => {
    expect(normalizePath("/var//log/./x")).toBe("/var/log/x");
    expect(normalizePath("//a//b//")).toBe("/a/b");
  });

  it(".. 越过根就停在根", () => {
    expect(normalizePath("/../../etc")).toBe("/etc");
  });

  it("中文和空格原样保留", () => {
    expect(normalizePath("/srv/我的 文件")).toBe("/srv/我的 文件");
  });
});
