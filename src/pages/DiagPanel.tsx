import { useEffect, useState } from "react";

import { formatTime } from "../lib/files";
import type { DiagResult } from "../lib/types";

interface Props {
  title: string;
  loading: boolean;
  error: string | null;
  result: DiagResult | null;
  onRefresh: () => void;
  onClose: () => void;
}

/**
 * 诊断结果浮层：把命令输出渲染成表格。
 *
 * 存在的理由：`ss -tulnp` 那种输出，运维靠训练能扫出重点，其他人只能逐列硬看。
 * 这里换成「一句话结论 + 表格 + 可展开的原始输出」。
 */
export default function DiagPanel({
  title,
  loading,
  error,
  result,
  onRefresh,
  onClose,
}: Props) {
  const [showRaw, setShowRaw] = useState(false);

  // 换一条诊断时收起原始输出，否则会停在上一份的状态上
  useEffect(() => {
    setShowRaw(false);
  }, [result?.kind, result?.fetchedAt]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <>
      <div className="diag-backdrop" onClick={onClose} />

      <section className="diag" role="dialog" aria-label={title}>
        <header className="diag__head">
          {/* 标题前的方框图标：没有任何视觉锚点时，浮层标题和表格之间的关系
              只能靠读文字建立，多份诊断叠开就分不清哪个是哪个 */}
          <span className="diag__glyph" aria-hidden="true">
            <svg viewBox="0 0 16 16">
              <rect x="2" y="3" width="12" height="10" rx="1.6" />
              <path d="M2 6.6h12M6.4 6.6V13" />
            </svg>
          </span>
          <span className="diag__title">{title}</span>
          {result ? (
            <span className="diag__time">{formatTime(result.fetchedAt)}</span>
          ) : null}
          <span className="diag__spacer" />
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={onRefresh}
            disabled={loading}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M13 8a5 5 0 1 1-1.6-3.7" />
              <path d="M13 2.6V5h-2.4" />
            </svg>
            {loading ? "刷新中" : "刷新"}
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--icon btn--sm diag__close"
            onClick={onClose}
            title="关闭（Esc）"
            aria-label="关闭"
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M4.2 4.2l7.6 7.6M11.8 4.2l-7.6 7.6" />
            </svg>
          </button>
        </header>

        {error ? <p className="diag__error">{error}</p> : null}

        {loading && !result ? <p className="diag__hint">正在执行…</p> : null}

        {result ? (
          <>
            {/* 结论条：整块浮层最有价值的就是这一句，原来它和正文同字号同颜色，
                混在 40 行表格上方等于没写 */}
            <div className="diag__conclusion">
              <span className="diag__summary">{result.summary}</span>
            </div>

            {result.warning ? <p className="diag__warning">{result.warning}</p> : null}

            {result.rows.length > 0 ? (
              <div className="diag__table-wrap">
                <table className="diag__table">
                  <thead>
                    <tr>
                      {result.columns.map((column) => (
                        <th key={column}>{column}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {result.rows.map((row, rowIndex) => (
                      <tr key={`${row[0]}-${rowIndex}`}>
                        {row.map((cell, cellIndex) => {
                          // 进程树那列带 ├─ / │ 连线，必须 pre 才不会被吃掉间距
                          const isTree = result.kind === "process_tree" && cellIndex === 2;
                          return (
                            <td
                              key={`${cellIndex}-${cell}`}
                              className={isTree ? "is-tree" : cellIndex === 0 ? "is-key" : undefined}
                            >
                              {cell}
                            </td>
                          );
                        })}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ) : null}

            <div className="diag__foot">
              <code className="diag__cmd" title={result.command}>
                {result.command}
              </code>
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => setShowRaw((open) => !open)}
              >
                {showRaw ? "收起原始输出" : "原始输出"}
              </button>
            </div>

            {showRaw ? <pre className="diag__raw">{result.raw || "（空）"}</pre> : null}
          </>
        ) : null}
      </section>
    </>
  );
}
