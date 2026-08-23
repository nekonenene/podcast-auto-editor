// 波形表示と出力範囲の選択 UI。
// 元ファイルを asset プロトコル経由の <audio> で再生しながら、
// 左右のハンドルをドラッグして出力範囲を決められる

import { useCallback, useEffect, useRef, useState } from "react";

interface WaveformProps {
  /** convertFileSrc 済みの再生用 URL */
  src: string;
  peaks: number[];
  durationMs: number;
  trimStartMs: number;
  trimEndMs: number;
  onTrimChange: (startMs: number, endMs: number) => void;
  disabled: boolean;
}

const HEIGHT = 72;
/** ハンドルをつかめる範囲 (px) */
const GRAB_PX = 10;

function formatTime(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
}

type DragTarget = "start" | "end" | null;

export function Waveform({
  src,
  peaks,
  durationMs,
  trimStartMs,
  trimEndMs,
  onTrimChange,
  disabled,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const audioRef = useRef<HTMLAudioElement>(null);
  const dragRef = useRef<DragTarget>(null);

  const [playing, setPlaying] = useState(false);
  const [playheadMs, setPlayheadMs] = useState(0);
  const [width, setWidth] = useState(0);

  // コンテナ幅に追従して canvas を再描画する
  useEffect(() => {
    const wrapper = wrapperRef.current;
    if (!wrapper) return;
    const observer = new ResizeObserver(() => {
      setWidth(wrapper.clientWidth);
    });
    observer.observe(wrapper);
    return () => observer.disconnect();
  }, []);

  const msToX = useCallback(
    (ms: number) => (durationMs > 0 ? (ms / durationMs) * width : 0),
    [durationMs, width],
  );
  const xToMs = useCallback(
    (x: number) =>
      width > 0
        ? Math.round(Math.min(Math.max(x / width, 0), 1) * durationMs)
        : 0,
    [durationMs, width],
  );

  // 波形と選択範囲・再生位置の描画
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width === 0) return;
    const scale = window.devicePixelRatio || 1;
    canvas.width = width * scale;
    canvas.height = HEIGHT * scale;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(scale, scale);
    ctx.clearRect(0, 0, width, HEIGHT);

    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    const startX = msToX(trimStartMs);
    const endX = msToX(trimEndMs);

    // 波形バー。選択範囲の外は薄く描く
    const barWidth = width / peaks.length;
    for (let i = 0; i < peaks.length; i++) {
      const x = i * barWidth;
      const inside = x >= startX && x <= endX;
      const barHeight = Math.max(1, peaks[i] * (HEIGHT - 8));
      ctx.fillStyle = inside
        ? "#0a84ff"
        : dark
          ? "rgba(255,255,255,0.18)"
          : "rgba(0,0,0,0.15)";
      ctx.fillRect(x, (HEIGHT - barHeight) / 2, Math.max(1, barWidth - 0.5), barHeight);
    }

    // 選択範囲のハンドル
    ctx.fillStyle = "#0a84ff";
    for (const x of [startX, endX]) {
      ctx.fillRect(x - 1.5, 0, 3, HEIGHT);
      ctx.beginPath();
      ctx.arc(x, HEIGHT / 2, 5, 0, Math.PI * 2);
      ctx.fill();
    }

    // 再生位置
    const playX = msToX(playheadMs);
    ctx.fillStyle = dark ? "#ffffff" : "#1c1c1e";
    ctx.fillRect(playX - 0.5, 0, 1, HEIGHT);
  }, [peaks, width, trimStartMs, trimEndMs, playheadMs, msToX]);

  // 再生中は再生位置を追従させ、選択範囲の終端で止める
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const audio = audioRef.current;
      if (audio) {
        const ms = audio.currentTime * 1000;
        setPlayheadMs(ms);
        if (ms >= trimEndMs) {
          audio.pause();
          setPlaying(false);
          return;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, trimEndMs]);

  // 編集開始などで操作不能になったら再生も止める
  useEffect(() => {
    if (disabled && playing) {
      audioRef.current?.pause();
      setPlaying(false);
    }
  }, [disabled, playing]);

  const seekTo = useCallback((ms: number) => {
    const audio = audioRef.current;
    if (audio) audio.currentTime = ms / 1000;
    setPlayheadMs(ms);
  }, []);

  // 指定位置へシークしてそのまま再生する。境界の確認用
  const playFrom = useCallback(
    async (ms: number) => {
      const audio = audioRef.current;
      if (!audio) return;
      seekTo(ms);
      await audio.play();
      setPlaying(true);
    },
    [seekTo],
  );

  const togglePlay = useCallback(async () => {
    const audio = audioRef.current;
    if (!audio) return;
    if (playing) {
      audio.pause();
      setPlaying(false);
    } else {
      // 再生位置が範囲外なら範囲の先頭から
      const ms = audio.currentTime * 1000;
      if (ms < trimStartMs || ms >= trimEndMs) {
        seekTo(trimStartMs);
      }
      await audio.play();
      setPlaying(true);
    }
  }, [playing, trimStartMs, trimEndMs, seekTo]);

  const onPointerDown = (e: React.PointerEvent) => {
    if (disabled) return;
    const rect = wrapperRef.current?.getBoundingClientRect();
    if (!rect) return;
    const x = e.clientX - rect.left;
    const startX = msToX(trimStartMs);
    const endX = msToX(trimEndMs);

    if (Math.abs(x - startX) <= GRAB_PX) {
      dragRef.current = "start";
    } else if (Math.abs(x - endX) <= GRAB_PX) {
      dragRef.current = "end";
    } else {
      // ハンドル以外のクリックは再生位置のジャンプ
      dragRef.current = null;
      seekTo(xToMs(x));
      return;
    }
    (e.target as Element).setPointerCapture(e.pointerId);
  };

  const onPointerMove = (e: React.PointerEvent) => {
    if (!dragRef.current) return;
    const rect = wrapperRef.current?.getBoundingClientRect();
    if (!rect) return;
    const ms = xToMs(e.clientX - rect.left);
    // 範囲の裏返りを防ぎつつ、最低1秒は残す
    if (dragRef.current === "start") {
      onTrimChange(Math.min(ms, trimEndMs - 1000), trimEndMs);
    } else {
      onTrimChange(trimStartMs, Math.max(ms, trimStartMs + 1000));
    }
  };

  const onPointerUp = () => {
    dragRef.current = null;
  };

  const trimmed = trimStartMs > 0 || trimEndMs < durationMs;

  return (
    <div className="waveform">
      {/* preload=metadata で開いた直後の全読み込みを避ける */}
      <audio ref={audioRef} src={src} preload="metadata" />
      <div
        ref={wrapperRef}
        className="waveform-canvas"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        <canvas
          ref={canvasRef}
          style={{ width: "100%", height: HEIGHT, display: "block" }}
        />
      </div>
      <div className="waveform-controls">
        <button className="play" onClick={togglePlay} disabled={disabled}>
          {playing ? "⏸ 停止" : "▶ 再生"}
        </button>
        {/* 再生しながら「本編が始まった」と思った瞬間に押して境界を決める */}
        <button
          className="mark"
          onClick={() =>
            onTrimChange(
              Math.min(Math.round(playheadMs), trimEndMs - 1000),
              trimEndMs,
            )
          }
          disabled={disabled}
          title="現在の再生位置を出力範囲の開始にします"
        >
          開始位置を指定
        </button>
        <button
          className="mark"
          onClick={() =>
            onTrimChange(
              trimStartMs,
              Math.max(Math.round(playheadMs), trimStartMs + 1000),
            )
          }
          disabled={disabled}
          title="現在の再生位置を出力範囲の終了にします"
        >
          終了位置を指定
        </button>
        <span className="waveform-time">
          {formatTime(playheadMs)} / {formatTime(durationMs)}
        </span>
        <span className="waveform-range">
          出力範囲: {formatTime(trimStartMs)} 〜 {formatTime(trimEndMs)}
          <button
            className="mark"
            onClick={() => void playFrom(trimStartMs)}
            disabled={disabled}
            title="出力範囲の開始位置から再生して、境界を確認します"
          >
            ▶ 開始から
          </button>
          <button
            className="mark"
            onClick={() =>
              void playFrom(Math.max(trimStartMs, trimEndMs - 5000))
            }
            disabled={disabled}
            title="出力範囲の終了5秒前から再生します。終端で自動停止します"
          >
            ▶ 終了前5秒
          </button>
          {trimmed && (
            <button
              className="reset-trim"
              onClick={() => onTrimChange(0, durationMs)}
              disabled={disabled}
            >
              リセット
            </button>
          )}
        </span>
      </div>
    </div>
  );
}
