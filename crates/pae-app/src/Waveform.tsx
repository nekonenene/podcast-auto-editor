// 波形表示と出力範囲の選択 UI。
// 元ファイルを asset プロトコル経由の <audio> で再生しながら、
// 左右のハンドルをドラッグして出力範囲を決められる。
// ホイール操作でズーム・パンでき、長い録音でも境界を正確に選べる

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
/** ハンドルのノブ (丸) の中心 Y と描画半径 */
const KNOB_Y = 9;
const KNOB_R = 6;
/** ノブをつかめる半径 (px)。ノブより少し広めにして掴みやすくする */
const GRAB_PX = 11;
/** これ以上は拡大しない表示幅 (ms) */
const MIN_VIEW_MS = 2000;

function formatTime(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
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
  const minimapDragRef = useRef(false);

  const [playing, setPlaying] = useState(false);
  const [playheadMs, setPlayheadMs] = useState(0);
  const [width, setWidth] = useState(0);
  const [viewStartMs, setViewStartMs] = useState(0);
  const [viewEndMs, setViewEndMs] = useState(durationMs);

  const viewLenMs = Math.max(1, viewEndMs - viewStartMs);
  const zoomed = viewStartMs > 0 || viewEndMs < durationMs;

  // 別のファイルに切り替わったら表示範囲をリセットする
  useEffect(() => {
    setViewStartMs(0);
    setViewEndMs(durationMs);
    setPlayheadMs(0);
  }, [src, durationMs]);

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
    (ms: number) => ((ms - viewStartMs) / viewLenMs) * width,
    [viewStartMs, viewLenMs, width],
  );
  const xToMs = useCallback(
    (x: number) =>
      Math.round(viewStartMs + clamp(x / width, 0, 1) * viewLenMs),
    [viewStartMs, viewLenMs, width],
  );

  const setView = useCallback(
    (startMs: number, lenMs: number) => {
      const len = clamp(lenMs, MIN_VIEW_MS, durationMs);
      const start = clamp(startMs, 0, durationMs - len);
      setViewStartMs(Math.round(start));
      setViewEndMs(Math.round(start + len));
    },
    [durationMs],
  );

  // centerMs の位置を基準に factor 倍の表示幅にする (1未満で拡大)
  const zoomAt = useCallback(
    (centerMs: number, factor: number) => {
      const len = clamp(viewLenMs * factor, MIN_VIEW_MS, durationMs);
      const ratio = (centerMs - viewStartMs) / viewLenMs;
      setView(centerMs - ratio * len, len);
    },
    [viewLenMs, viewStartMs, durationMs, setView],
  );

  // ホイール縦でカーソル位置を中心にズーム、横でパン。
  // preventDefault が必要なため React の onWheel ではなく native listener を使う
  useEffect(() => {
    const el = wrapperRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      if (Math.abs(e.deltaY) >= Math.abs(e.deltaX)) {
        zoomAt(xToMs(e.clientX - rect.left), Math.exp(e.deltaY * 0.002));
      } else {
        setView(viewStartMs + e.deltaX * (viewLenMs / width), viewLenMs);
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoomAt, xToMs, setView, viewStartMs, viewLenMs, width]);

  // 波形と選択範囲・再生位置の描画
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width === 0 || peaks.length === 0) return;
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
    const bucketMs = durationMs / peaks.length;

    // 表示範囲のピークを1pxごとに切り出して描く (区間内の最大値)
    for (let px = 0; px < width; px++) {
      const t0 = viewStartMs + (px / width) * viewLenMs;
      const t1 = t0 + viewLenMs / width;
      const i0 = clamp(Math.floor(t0 / bucketMs), 0, peaks.length - 1);
      const i1 = clamp(Math.ceil(t1 / bucketMs), i0 + 1, peaks.length);
      let peak = 0;
      for (let i = i0; i < i1; i++) {
        if (peaks[i] > peak) peak = peaks[i];
      }
      const inside = px >= startX && px <= endX;
      const barHeight = Math.max(1, peak * (HEIGHT - 8));
      ctx.fillStyle = inside
        ? "#0a84ff"
        : dark
          ? "rgba(255,255,255,0.18)"
          : "rgba(0,0,0,0.15)";
      ctx.fillRect(px, (HEIGHT - barHeight) / 2, 1, barHeight);
    }

    // 選択範囲のハンドル (表示範囲内にあるときだけ)。
    // ドラッグできるのは上端のノブだけなので、ノブを目立たせて描く
    ctx.fillStyle = "#0a84ff";
    for (const x of [startX, endX]) {
      if (x < -GRAB_PX || x > width + GRAB_PX) continue;
      ctx.fillRect(x - 1, 0, 2, HEIGHT);
      ctx.beginPath();
      ctx.arc(x, KNOB_Y, KNOB_R, 0, Math.PI * 2);
      ctx.fill();
      // ノブの縁取りで背景から浮かせる
      ctx.strokeStyle = dark ? "#1c1c1e" : "#ffffff";
      ctx.lineWidth = 1.5;
      ctx.stroke();
    }

    // 再生位置
    const playX = msToX(playheadMs);
    if (playX >= 0 && playX <= width) {
      ctx.fillStyle = dark ? "#ffffff" : "#1c1c1e";
      ctx.fillRect(playX - 0.5, 0, 1, HEIGHT);
    }
  }, [
    peaks,
    width,
    trimStartMs,
    trimEndMs,
    playheadMs,
    viewStartMs,
    viewLenMs,
    durationMs,
    msToX,
  ]);

  // 再生中は再生位置を追従させ、選択範囲の終端で止める。
  // ズーム中に再生位置が画面外へ出たら表示範囲を送る
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
        if (ms > viewEndMs) {
          setView(ms - viewLenMs * 0.1, viewLenMs);
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, trimEndMs, viewEndMs, viewLenMs, setView]);

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

  // ノブ (上端の丸) の当たり判定。バーの線上は判定に含めず、
  // 境界のすぐ近くでも再生位置を置けるようにする
  const hitKnob = useCallback(
    (x: number, y: number): DragTarget => {
      for (const [target, ms] of [
        ["start", trimStartMs],
        ["end", trimEndMs],
      ] as const) {
        const knobX = msToX(ms);
        if (Math.hypot(x - knobX, y - KNOB_Y) <= GRAB_PX) {
          return target;
        }
      }
      return null;
    },
    [trimStartMs, trimEndMs, msToX],
  );

  const onPointerDown = (e: React.PointerEvent) => {
    if (disabled) return;
    const rect = wrapperRef.current?.getBoundingClientRect();
    if (!rect) return;
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;

    const knob = hitKnob(x, y);
    if (knob) {
      dragRef.current = knob;
      (e.target as Element).setPointerCapture(e.pointerId);
    } else {
      // ノブ以外のクリックは再生位置のジャンプ
      dragRef.current = null;
      seekTo(xToMs(x));
    }
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const rect = wrapperRef.current?.getBoundingClientRect();
    if (!rect) return;
    const x = e.clientX - rect.left;
    if (!dragRef.current) {
      // ノブの上ではカーソルを変えて、つかめる場所を示す
      const wrapper = wrapperRef.current;
      if (wrapper) {
        wrapper.style.cursor = hitKnob(x, e.clientY - rect.top)
          ? "ew-resize"
          : "pointer";
      }
      return;
    }
    const ms = xToMs(x);
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

  // ミニマップ: 全体の中の表示範囲を示し、ドラッグでパンできる
  const onMinimapPointer = (e: React.PointerEvent, isDown: boolean) => {
    if (isDown) {
      minimapDragRef.current = true;
      (e.target as Element).setPointerCapture(e.pointerId);
    } else if (!minimapDragRef.current) {
      return;
    }
    const rect = (e.currentTarget as Element).getBoundingClientRect();
    const centerMs = ((e.clientX - rect.left) / rect.width) * durationMs;
    setView(centerMs - viewLenMs / 2, viewLenMs);
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
      <div
        className="waveform-minimap"
        onPointerDown={(e) => onMinimapPointer(e, true)}
        onPointerMove={(e) => onMinimapPointer(e, false)}
        onPointerUp={() => {
          minimapDragRef.current = false;
        }}
      >
        <div
          className="waveform-minimap-view"
          style={{
            left: `${(viewStartMs / durationMs) * 100}%`,
            width: `${(viewLenMs / durationMs) * 100}%`,
          }}
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
        <span className="waveform-zoom">
          <button
            className="mark"
            onClick={() => zoomAt(playheadMs, 0.5)}
            title="再生位置を中心に拡大します (ホイールでも操作できます)"
          >
            ＋
          </button>
          <button
            className="mark"
            onClick={() =>
              zoomAt(viewStartMs + viewLenMs / 2, 2.0)
            }
            title="縮小します"
          >
            −
          </button>
          {zoomed && (
            <button
              className="mark"
              onClick={() => setView(0, durationMs)}
              title="全体を表示します"
            >
              全体
            </button>
          )}
        </span>
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
