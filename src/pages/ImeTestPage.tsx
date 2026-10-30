import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useCallback, useEffect, useRef, useState } from "react";

import { fromUnknown } from "../lib/errors";
import { call } from "../lib/ipc";

interface ImeEcho {
  bytes: number;
  text: string;
  valid_utf8: boolean;
}

interface CaptureEvent {
  id: number;
  charCount: number;
  bytes: number;
  hex: string;
  decoded: string;
  validUtf8: boolean;
  match: boolean;
}

function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}

function readableHex(hex: string): string {
  return hex.replace(/(.{2})/g, "$1 ").trim();
}

let seq = 0;

/**
 * S3：中文输入法取证页（仅开发构建可见）。
 *
 * 终端里敲的字会经过 xterm `onData` → IPC → Rust 再回来，
 * 页面上逐条列出「字符数 / 字节数 / 原始十六进制 / Rust 解码结果」。
 * 任何一个字的字节数或十六进制不对，都能在这里直接看出来。
 */
export default function ImeTestPage() {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const [events, setEvents] = useState<CaptureEvent[]>([]);
  const [error, setError] = useState<string | null>(null);

  const handleInput = useCallback(async (raw: string) => {
    const bytes = new TextEncoder().encode(raw);
    const hex = toHex(bytes);
    const charCount = [...raw].length;

    let event: CaptureEvent = {
      id: ++seq,
      charCount,
      bytes: bytes.length,
      hex,
      decoded: "",
      validUtf8: false,
      match: false,
    };

    try {
      const echoed = await call<ImeEcho>("ime_echo", { dataHex: hex });
      event = {
        ...event,
        decoded: echoed.text,
        validUtf8: echoed.valid_utf8,
        match: echoed.text === raw && echoed.bytes === bytes.length,
      };
      setError(null);
    } catch (err) {
      setError(fromUnknown(err).message);
    }

    setEvents((prev) => [event, ...prev].slice(0, 12));
    termRef.current?.write(raw);
  }, []);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new Terminal({
      fontFamily: 'ui-monospace, SFMono-Regular, Consolas, "Microsoft YaHei", monospace',
      fontSize: 14,
      cursorBlink: true,
      scrollback: 2000,
      theme: { background: "#0f1b2a", foreground: "#d8e2ef" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();
    termRef.current = term;

    term.writeln("S3 输入法取证");
    term.writeln("1. 切换到中文输入法，输入「你好世界」再按回车");
    term.writeln("2. 再试「终端港 TermHarbor」中英混排、以及带标点的一句话");
    term.writeln("3. 看右侧表格：bytes 应为字符数×3（中文）或×1（ASCII）");
    term.write("\r\n> ");

    const sub = term.onData((data) => {
      void handleInput(data);
      if (data.includes("\r")) {
        term.write("\r\n> ");
      }
    });

    const onResize = () => fit.fit();
    window.addEventListener("resize", onResize);

    return () => {
      window.removeEventListener("resize", onResize);
      sub.dispose();
      term.dispose();
      termRef.current = null;
    };
  }, [handleInput]);

  const total = events.length;
  const mismatched = events.filter((e) => !e.match).length;

  return (
    <div className="page page--wide">
      <section className="card">
        <header className="card__head">
          <h2>中文输入法取证</h2>
          <span className="badge badge--dev">仅开发构建</span>
        </header>
        <p className="card__hint">
          走的是真实链路：xterm <code>onData</code> → Tauri Command → Rust。左侧输入，右侧是每帧的原始字节。
        </p>

        {error ? <div className="alert alert--danger">{error}</div> : null}

        <div className="ime">
          <div className="ime__term" ref={hostRef} />
          <div className="ime__log">
            <div className="ime__summary">
              <span>事件 {total}</span>
              <span className={mismatched > 0 ? "is-bad" : "is-good"}>
                {mismatched > 0 ? `不一致 ${mismatched}` : "全部一致"}
              </span>
            </div>
            {events.length === 0 ? (
              <p className="ime__empty">还没有输入</p>
            ) : (
              <ul className="ime__list">
                {events.map((e) => (
                  <li key={e.id} className={e.match ? "" : "is-bad"}>
                    <div className="ime__line">
                      <span className="ime__text">{e.decoded || "（解码失败）"}</span>
                      <span className="ime__meta">
                        {e.charCount} 字 / {e.bytes} 字节
                        {!e.validUtf8 ? " · 非法 UTF-8" : ""}
                      </span>
                    </div>
                    <code className="ime__hex">{readableHex(e.hex)}</code>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      </section>

      <section className="card">
        <header className="card__head">
          <h2>判定标准</h2>
        </header>
        <ul className="checklist">
          <li>
            <span className="checklist__mark" aria-hidden="true" />
            输入 4 个汉字应产生 1 条或 2 条事件，字节数合计 12
          </li>
          <li>
            <span className="checklist__mark" aria-hidden="true" />
            <code>decoded</code> 与左侧输入完全一致，不能出现 <code>�</code>
          </li>
          <li>
            <span className="checklist__mark" aria-hidden="true" />
            中英混排时 ASCII 各占 1 字节，汉字各占 3 字节
          </li>
          <li>
            <span className="checklist__mark" aria-hidden="true" />
            退格、方向键应产出 <code>1b 5b …</code> 形式的转义序列
          </li>
        </ul>
      </section>
    </div>
  );
}
