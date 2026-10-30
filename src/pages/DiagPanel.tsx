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
          <span className="diag__title">{title}</span>
          {result ? (
            <span className="diag__time">{formatTime(result.fetchedAt)}</span>
          ) : null}
          <span className="diag__spacer" />
          <button
            type="button"
            className="btn btn--sm"
            onClick={onRefresh}
            disabled={loading}
          >
            刷新
          </button>
          <button type="button" className="diag__close" onClick={onClose} title="关闭（Esc）">
            ×
          </button>
        </header>

        {error ? <p className="diag__error">{error}</p> : null}

        {loading && !result ? <p className="diag__hint">正在执行…</p> : null}

        {result ? (
          <>
            <p className="diag__summary">{result.summary}</p>

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
              <code className="diag__cmd">{result.command}</code>
              <button
                type="button"
                className="files-link"
                onClick={() => setShowRaw((open) => !open)}
              >
                {showRaw ? "收起原始输出" : "查看原始输出"}
              </button>
            </div>

            {showRaw ? <pre className="diag__raw">{result.raw || "（空）"}</pre> : null}
          </>
        ) : null}
      </section>
    </>
  );
}
