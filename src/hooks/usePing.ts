import { useCallback, useEffect, useState } from "react";

import { AppError } from "../lib/errors";
import { call } from "../lib/ipc";
import type { Pong } from "../lib/types";

export interface PingResult {
  pong: Pong | null;
  error: AppError | null;
  loading: boolean;
  refresh: () => void;
}

/** 拉一次后端自检信息。侧边栏状态灯、状态栏、自检页共用同一份数据。 */
export function usePing(): PingResult {
  const [pong, setPong] = useState<Pong | null>(null);
  const [error, setError] = useState<AppError | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);

    call<Pong>("ping")
      .then((result) => {
        if (cancelled) return;
        setPong(result);
        setError(null);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setPong(null);
        setError(err as AppError);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [nonce]);

  const refresh = useCallback(() => setNonce((n) => n + 1), []);

  return { pong, error, loading, refresh };
}
