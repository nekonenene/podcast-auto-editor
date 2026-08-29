import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AppConfig,
  FfmpegStatus,
  JobResult,
  MediaInfo,
  ModelInfo,
  OutputSelection,
  ProgressEvent,
  Stage,
  WaveformData,
} from "./types";
import { Waveform } from "./Waveform";
import {
  DEFAULT_OUTPUTS,
  MEDIA_OUTPUT_KEYS,
  MP3_BITRATES,
  OUTPUT_LABELS,
  STAGE_LABELS,
  STAGE_ORDER,
  TRANSCRIPT_OUTPUT_KEYS,
} from "./types";
import "./App.css";

const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv"];
const AUDIO_EXTENSIONS = ["mp3", "wav", "m4a", "aac", "flac"];

type UiPhase = "idle" | "running" | "done" | "error";

// 設定画面で変更でき、その場で保存される項目
type SettingsForm = {
  outputs: OutputSelection;
  voiceDuck: boolean;
  mp3Bitrate: number;
  ffmpegDir: string | null;
};

function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}分${String(s).padStart(2, "0")}秒`;
}

// 完成メッセージの「80分00秒 → 72分10秒（9.7% 短縮）」を組み立てる。
// 元動画の長さでなく編集した範囲を基準にしないと、範囲選択で切った分まで短縮に見えてしまう。
// BGM の余韻も無音短縮の成果ではないため、短縮率から外して末尾へ添える
function formatShrinkSummary(result: JobResult): string {
  const editedMs = result.outputDurationMs - result.tailMs;
  const rate = 100 * (1 - editedMs / result.editedRangeMs);
  const summary = `${formatDuration(result.editedRangeMs)} → ${formatDuration(
    editedMs,
  )}（${rate.toFixed(1)}% 短縮）`;
  if (result.tailMs === 0) {
    return summary;
  }
  return `${summary} ＋ BGM余韻 ${Math.round(result.tailMs / 1000)}秒`;
}

// パス操作は Windows の "\" と macOS/Linux の "/" の両方に対応する

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function parentDir(path: string): string {
  const index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return index > 0 ? path.slice(0, index) : path;
}

function joinPath(dir: string, name: string): string {
  const separator = dir.includes("\\") ? "\\" : "/";
  return `${dir}${separator}${name}`;
}

function extensionOf(path: string): string {
  return path.split(".").pop()?.toLowerCase() ?? "";
}

function App() {
  const [video, setVideo] = useState<string | null>(null);
  const [videoInfo, setVideoInfo] = useState<MediaInfo | null>(null);
  const [waveform, setWaveform] = useState<WaveformData | null>(null);
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const [trimStartMs, setTrimStartMs] = useState(0);
  const [trimEndMs, setTrimEndMs] = useState(0);
  const [bgm, setBgm] = useState<string | null>(null);
  const [preset, setPreset] = useState("natural");
  const [bgmVolume, setBgmVolume] = useState(0.15);
  const [transcribe, setTranscribe] = useState(true);
  const [model, setModel] = useState("large-v3-turbo");
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [diarize, setDiarize] = useState(false);
  const [speakerCount, setSpeakerCount] = useState(2);
  const [diarizeModel, setDiarizeModel] = useState<ModelInfo | null>(null);
  const [outputDir, setOutputDir] = useState<string | null>(null);
  const [fadeInS, setFadeInS] = useState(2.0);
  const [fadeOutS, setFadeOutS] = useState(4.0);
  const [endingTailS, setEndingTailS] = useState(5.0);
  const [voiceDuck, setVoiceDuck] = useState(true);
  const [outputs, setOutputs] = useState<OutputSelection>(DEFAULT_OUTPUTS);
  const [mp3Bitrate, setMp3Bitrate] = useState(128);
  const [ffmpegDir, setFfmpegDir] = useState<string | null>(null);
  const [ffmpegStatus, setFfmpegStatus] = useState<FfmpegStatus | null>(null);
  const [showSettings, setShowSettings] = useState(false);

  const [previewing, setPreviewing] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(false);
  const previewAudioRef = useRef<HTMLAudioElement | null>(null);
  const previewUrlRef = useRef<string | null>(null);

  const [phase, setPhase] = useState<UiPhase>("idle");
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<JobResult | null>(null);

  // 指定されたフォルダで ffmpeg が使えるかを確かめ、結果を設定画面へ出す
  const checkFfmpeg = useCallback((dir: string | null) => {
    setFfmpegStatus(null);
    invoke<FfmpegStatus>("check_ffmpeg", { dir })
      .then(setFfmpegStatus)
      .catch((e) =>
        setFfmpegStatus({
          found: false,
          summary: "確かめられませんでした",
          detail: String(e),
        }),
      );
  }, []);

  // 起動時に保存済み設定とモデル一覧を読み込む
  useEffect(() => {
    invoke<AppConfig>("get_config")
      .then((config) => {
        setBgm(config.default_bgm);
        setPreset(config.preset);
        setBgmVolume(config.bgm.volume);
        setFadeInS(config.bgm.fade_in_s);
        setFadeOutS(config.bgm.fade_out_s);
        setEndingTailS(config.bgm.ending_tail_s ?? 5.0);
        setVoiceDuck((config.bgm.voice_duck_db ?? -4.0) < 0);
        setOutputs(config.outputs ?? DEFAULT_OUTPUTS);
        setMp3Bitrate(config.mp3_bitrate_kbps ?? 128);
        setTranscribe(config.transcribe);
        setModel(config.model);
        setDiarize(config.diarize ?? false);
        setSpeakerCount(config.speaker_count ?? 2);
        setOutputDir(config.output_dir);
        setFfmpegDir(config.ffmpeg_dir);
        checkFfmpeg(config.ffmpeg_dir);
      })
      .catch((e) => setError(String(e)));
    invoke<ModelInfo[]>("list_models")
      .then(setModels)
      .catch(() => setModels([]));
    invoke<ModelInfo>("diarize_model_info")
      .then(setDiarizeModel)
      .catch(() => setDiarizeModel(null));
  }, []);

  const chooseVideo = useCallback(async (path: string) => {
    setError(null);
    setResult(null);
    setPhase("idle");
    setVideo(path);
    setVideoInfo(null);
    setWaveform(null);
    setPreviewSrc(null);
    // 出力先が未設定なら、動画と同じ場所の podcast-output をデフォルトにする
    setOutputDir((current) => current ?? joinPath(parentDir(path), "podcast-output"));
    try {
      const info = await invoke<MediaInfo>("probe_media", { path });
      setVideoInfo(info);
      setTrimStartMs(0);
      setTrimEndMs(info.duration_ms);

      // 波形とプレビュー再生の準備。失敗しても編集自体はできるので警告に留める
      try {
        await invoke("allow_media_preview", { path });
        setPreviewSrc(convertFileSrc(path));
        setWaveform(await invoke<WaveformData>("waveform", { path }));
      } catch (e) {
        console.warn("波形の準備に失敗:", e);
      }
    } catch (e) {
      setError(String(e));
      setVideo(null);
    }
  }, []);

  // ウィンドウへのドラッグ&ドロップ。動画は常に入力として扱い、
  // 音声ファイルは「入力が未設定なら入力、設定済みなら BGM」と解釈する。
  // Tauri 外 (通常ブラウザでの表示確認時) では webview API が無いためスキップする
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type !== "drop") return;
      for (const path of event.payload.paths) {
        const ext = extensionOf(path);
        if (VIDEO_EXTENSIONS.includes(ext)) {
          void chooseVideo(path);
        } else if (AUDIO_EXTENSIONS.includes(ext)) {
          if (video === null) {
            void chooseVideo(path);
          } else {
            setBgm(path);
          }
        }
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [chooseVideo, video]);

  const selectVideo = async () => {
    const path = await open({
      multiple: false,
      filters: [
        {
          name: "動画・音声",
          extensions: [...VIDEO_EXTENSIONS, ...AUDIO_EXTENSIONS],
        },
      ],
    });
    if (typeof path === "string") await chooseVideo(path);
  };

  const selectBgm = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "音声", extensions: AUDIO_EXTENSIONS }],
    });
    if (typeof path === "string") setBgm(path);
  };

  const selectOutputDir = async () => {
    const path = await open({ directory: true });
    if (typeof path === "string") setOutputDir(path);
  };

  const stopPreview = useCallback(() => {
    previewAudioRef.current?.pause();
    previewAudioRef.current = null;
    if (previewUrlRef.current) {
      URL.revokeObjectURL(previewUrlRef.current);
      previewUrlRef.current = null;
    }
    setPreviewing(false);
  }, []);

  // 現在の設定で試聴用ミックスを生成してループ再生する
  const playPreview = useCallback(async () => {
    if (!video || !bgm) return;
    setPreviewLoading(true);
    try {
      const data = await invoke<ArrayBuffer>("bgm_preview", {
        request: { input: video, bgm, bgmVolume },
      });
      stopPreview();
      const url = URL.createObjectURL(
        new Blob([data], { type: "audio/mpeg" }),
      );
      const audio = new Audio(url);
      audio.loop = true;
      await audio.play();
      previewAudioRef.current = audio;
      previewUrlRef.current = url;
      setPreviewing(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setPreviewLoading(false);
    }
  }, [video, bgm, bgmVolume, stopPreview]);

  // 試聴中に音量を変えたら、少し待ってから作り直して反映する
  useEffect(() => {
    if (!previewing) return;
    const timer = setTimeout(() => {
      void playPreview();
    }, 400);
    return () => clearTimeout(timer);
    // playPreview は設定値に依存しているため、設定変更のたびに発火する
  }, [bgmVolume]); // eslint-disable-line react-hooks/exhaustive-deps

  // 設定画面での変更は即座に保存する。
  // 変えたい項目だけを渡し、残りは現在の値をそのまま使う
  const saveSettings = useCallback(
    (patch: Partial<SettingsForm>) => {
      const next = { outputs, voiceDuck, mp3Bitrate, ffmpegDir, ...patch };
      setOutputs(next.outputs);
      setVoiceDuck(next.voiceDuck);
      setMp3Bitrate(next.mp3Bitrate);
      setFfmpegDir(next.ffmpegDir);
      invoke("save_settings", {
        update: {
          outputs: next.outputs,
          voiceDuckDb: next.voiceDuck ? -4.0 : 0.0,
          mp3BitrateKbps: next.mp3Bitrate,
          ffmpegDir: next.ffmpegDir,
        },
      }).catch((e) => setError(String(e)));
    },
    [outputs, voiceDuck, mp3Bitrate, ffmpegDir],
  );

  const selectFfmpegDir = async () => {
    const dir = await open({ directory: true });
    if (typeof dir !== "string") return;
    saveSettings({ ffmpegDir: dir });
    checkFfmpeg(dir);
  };

  const clearFfmpegDir = () => {
    saveSettings({ ffmpegDir: null });
    checkFfmpeg(null);
  };

  const start = async () => {
    if (!video || !outputDir) return;
    stopPreview();
    setPhase("running");
    setError(null);
    setResult(null);
    setProgress(null);

    const onProgress = new Channel<ProgressEvent>();
    onProgress.onmessage = (event) => setProgress(event);

    try {
      const jobResult = await invoke<JobResult>("start_job", {
        request: {
          input: video,
          outputDir,
          bgm,
          bgmVolume,
          fadeInS,
          fadeOutS,
          endingTailS,
          preset,
          transcribe,
          model,
          diarize,
          speakerCount,
          // 全範囲のときは指定なし (トリムなし) として送る
          trimStartMs: trimStartMs > 0 ? trimStartMs : null,
          trimEndMs:
            videoInfo && trimEndMs < videoInfo.duration_ms ? trimEndMs : null,
        },
        onProgress,
      });
      setResult(jobResult);
      setPhase("done");
    } catch (e) {
      setError(String(e));
      setPhase(String(e).includes("キャンセル") ? "idle" : "error");
    }
  };

  const cancel = () => {
    void invoke("cancel_job");
  };

  const reveal = (path: string) => {
    void invoke("reveal_path", { path });
  };

  // この実行で通るステージだけをチェックリストに出す
  const activeStages = useMemo(() => {
    return STAGE_ORDER.filter((stage) => {
      if (stage === "mix_bgm") return bgm !== null;
      if (stage === "diarize") return transcribe && diarize;
      if (stage === "transcribe" || stage === "write_outputs") return transcribe;
      return true;
    });
  }, [bgm, transcribe, diarize]);

  const currentStageIndex = progress
    ? activeStages.indexOf(progress.stage)
    : -1;

  const selectedModel = models.find((m) => m.name === model);
  const running = phase === "running";
  const anyOutputSelected = Object.values(outputs).some(Boolean);

  // ffmpeg が見つかっていないときだけ、選ぶことをうながす言葉にする
  const ffmpegPickerLabel = ffmpegDir
    ? "別の場所を選ぶ"
    : ffmpegStatus?.found === false
      ? "フォルダを選択"
      : "自分で指定する";

  if (showSettings) {
    return (
      <main className="container">
        <div className="topbar">
          <button className="back" onClick={() => setShowSettings(false)}>
            ← 戻る
          </button>
          <h1>設定</h1>
        </div>
        <section className="form">
          <div className="field">
            <label>出力するファイル</label>
            {MEDIA_OUTPUT_KEYS.map((key) => (
              <label className="row" key={key}>
                <input
                  type="checkbox"
                  checked={outputs[key]}
                  onChange={(e) =>
                    saveSettings({
                      outputs: { ...outputs, [key]: e.target.checked },
                    })
                  }
                />
                {OUTPUT_LABELS[key]}
              </label>
            ))}
            <fieldset className="subgroup">
              <legend>文字起こしから作られるファイル</legend>
              <div className="subgroup-body">
                <p className="hint">
                  メイン画面の「文字起こし」が ON のときに出力されます
                </p>
                {TRANSCRIPT_OUTPUT_KEYS.map((key) => (
                  <label className="row" key={key}>
                    <input
                      type="checkbox"
                      checked={outputs[key]}
                      onChange={(e) =>
                        saveSettings({
                          outputs: { ...outputs, [key]: e.target.checked },
                        })
                      }
                    />
                    {OUTPUT_LABELS[key]}
                  </label>
                ))}
              </div>
            </fieldset>
            {!anyOutputSelected && (
              <p className="hint warning">少なくとも1つ選択してください</p>
            )}
          </div>
          <div className="field">
            <label>MP3 の音質</label>
            <select
              value={mp3Bitrate}
              onChange={(e) =>
                saveSettings({ mp3Bitrate: Number(e.target.value) })
              }
            >
              {MP3_BITRATES.map((kbps) => (
                <option key={kbps} value={kbps}>
                  {kbps === 0
                    ? "VBR — 高音質 (内容に応じた可変ビットレート)"
                    : `${kbps} kbps`}
                  {kbps === 64 ? " — 会話向け (小さいファイル)" : ""}
                  {kbps === 128 ? " — 標準" : ""}
                  {kbps === 320 ? " — 最高音質" : ""}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label>音声処理</label>
            <label className="row">
              <input
                type="checkbox"
                checked={voiceDuck}
                onChange={(e) => saveSettings({ voiceDuck: e.target.checked })}
              />
              声の帯域で BGM を下げる (聞き取りやすくなります)
            </label>
          </div>
          <div className="field">
            <label>ffmpeg の場所</label>
            <div className="ffmpeg-status">
              <p className={ffmpegStatus?.found === false ? "hint warning" : "hint"}>
                {/* 自分で指定したときは、そのフォルダを見せれば説明は要らない */}
                {ffmpegDir ??
                  (ffmpegStatus
                    ? `自動検索（${ffmpegStatus.summary}）`
                    : "確認しています…")}
              </p>
              <button className="link" onClick={selectFfmpegDir}>
                {ffmpegPickerLabel}
              </button>
            </div>
            {ffmpegStatus?.detail && (
              <p className="hint warning status-detail">{ffmpegStatus.detail}</p>
            )}
            {ffmpegDir && (
              <button className="link" onClick={clearFfmpegDir}>
                指定をやめて自動で探す
              </button>
            )}
          </div>
        </section>
      </main>
    );
  }

  return (
    <main className="container">
      <div className="topbar">
        <h1>Podcast Auto Editor</h1>
        <button
          className="gear"
          onClick={() => setShowSettings(true)}
          disabled={running}
          title="設定"
        >
          ⚙
        </button>
      </div>

      <section className="form" aria-disabled={running}>
        <div className="field">
          <label>動画・音声</label>
          <button className="picker" onClick={selectVideo} disabled={running}>
            {video ? fileName(video) : "クリックして選択 (ドラッグ&ドロップ可)"}
          </button>
          {videoInfo && videoInfo.has_video && (
            <p className="hint">
              {formatDuration(videoInfo.duration_ms)} ・ {videoInfo.width}x
              {videoInfo.height} ・ {videoInfo.video_codec}/
              {videoInfo.audio_codec}
            </p>
          )}
          {videoInfo && !videoInfo.has_video && (
            <p className="hint">
              {formatDuration(videoInfo.duration_ms)} ・ 音声のみ (
              {videoInfo.audio_codec}) ・ MP4 の代わりに MP3 が出力されます
            </p>
          )}
          {waveform && previewSrc && videoInfo && (
            <Waveform
              src={previewSrc}
              peaks={waveform.peaks}
              durationMs={videoInfo.duration_ms}
              trimStartMs={trimStartMs}
              trimEndMs={trimEndMs}
              onTrimChange={(start, end) => {
                setTrimStartMs(start);
                setTrimEndMs(end);
              }}
              disabled={running}
            />
          )}
          {video && videoInfo && !waveform && (
            <p className="hint">波形を解析中...</p>
          )}
        </div>

        <div className="field">
          <label>BGM</label>
          <div className="row">
            <button className="picker" onClick={selectBgm} disabled={running}>
              {bgm ? fileName(bgm) : "BGM を選択 (なしでも可)"}
            </button>
            {bgm && (
              <button
                className="clear"
                onClick={() => setBgm(null)}
                disabled={running}
                title="BGM を外す"
              >
                ×
              </button>
            )}
          </div>
        </div>

        {bgm && (
          <>
            <div className="field">
              <label>BGM 音量: {(bgmVolume * 100).toFixed(0)}%</label>
              <div className="row">
                <input
                  type="range"
                  min="0.03"
                  max="0.5"
                  step="0.01"
                  value={bgmVolume}
                  onChange={(e) => setBgmVolume(Number(e.target.value))}
                  disabled={running}
                />
                <button
                  className="preview"
                  onClick={() => (previewing ? stopPreview() : void playPreview())}
                  disabled={running || !video || previewLoading}
                  title="動画の一部と BGM を現在の設定でミックスして試聴します"
                >
                  {previewLoading ? "生成中..." : previewing ? "■ 停止" : "▶ 試聴"}
                </button>
              </div>
              {previewing && (
                <p className="hint">
                  試聴中 (ループ再生)。スライダーを動かすと反映されます
                </p>
              )}
            </div>
            <div className="field">
              <label>
                エンディングの余韻: {endingTailS.toFixed(0)}秒
                {endingTailS === 0 && " (なし)"}
              </label>
              <input
                type="range"
                min="0"
                max="15"
                step="1"
                value={endingTailS}
                onChange={(e) => setEndingTailS(Number(e.target.value))}
                disabled={running}
              />
              <p className="hint">会話終了後に BGM だけを残してフェードアウトします</p>
            </div>
          </>
        )}

        <div className="field">
          <label>無音処理</label>
          <select
            value={preset}
            onChange={(e) => setPreset(e.target.value)}
            disabled={running}
          >
            <option value="natural">Natural — 1.5秒以上の間を0.9秒に</option>
            <option value="standard">Standard — 1.0秒以上の間を0.6秒に</option>
            <option value="aggressive">Aggressive — 0.7秒以上の間を0.4秒に</option>
          </select>
        </div>

        <div className="field">
          <label className="row">
            <input
              type="checkbox"
              checked={transcribe}
              onChange={(e) => setTranscribe(e.target.checked)}
              disabled={running}
            />
            文字起こし
          </label>
          {transcribe && (
            <>
              <select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                disabled={running}
              >
                {models.map((m) => (
                  <option key={m.name} value={m.name}>
                    {m.name} — {m.description}
                  </option>
                ))}
              </select>
              {selectedModel && !selectedModel.downloaded && (
                <p className="hint">
                  初回に約{selectedModel.approxSizeMb}MB
                  のモデルを自動ダウンロードします
                </p>
              )}
              <label className="row">
                <input
                  type="checkbox"
                  checked={diarize}
                  onChange={(e) => setDiarize(e.target.checked)}
                  disabled={running}
                />
                話者ラベルを付ける
              </label>
              {diarize && (
                <>
                  <label className="row">
                    話者の人数
                    <input
                      type="number"
                      min={2}
                      max={6}
                      value={speakerCount}
                      onChange={(e) =>
                        setSpeakerCount(
                          Math.min(6, Math.max(2, Number(e.target.value) || 2)),
                        )
                      }
                      disabled={running}
                    />
                  </label>
                  <p className="hint">
                    最初に喋った人が「話者1」になります。
                    決めきれなかった発言は「話者不明」になります
                  </p>
                  {diarizeModel && !diarizeModel.downloaded && (
                    <p className="hint">
                      初回に約{diarizeModel.approxSizeMb}MB
                      のモデルを自動ダウンロードします
                    </p>
                  )}
                </>
              )}
            </>
          )}
        </div>

        <div className="field">
          <label>出力先</label>
          <button className="picker" onClick={selectOutputDir} disabled={running}>
            {outputDir ?? "フォルダを選択"}
          </button>
        </div>
      </section>

      {error && <div className="error">{error}</div>}

      {!running && (
        <button
          className="start"
          onClick={start}
          disabled={!video || !outputDir || !videoInfo || !anyOutputSelected}
        >
          編集開始
        </button>
      )}
      {!anyOutputSelected && (
        <p className="hint warning">
          出力ファイルが選択されていません。右上の設定から選んでください
        </p>
      )}

      {running && (
        <section className="progress">
          <ul>
            {activeStages.map((stage, index) => {
              const state =
                index < currentStageIndex
                  ? "done"
                  : index === currentStageIndex
                    ? "active"
                    : "pending";
              return (
                <li key={stage} className={state}>
                  <span className="stage-name">
                    {state === "done" ? "✓" : state === "active" ? "▶" : "・"}{" "}
                    {STAGE_LABELS[stage as Stage]}
                  </span>
                  {state === "active" && progress?.fraction != null && (
                    <span className="percent">
                      {Math.round(progress.fraction * 100)}%
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
          {progress?.message && <p className="hint">{progress.message}</p>}
          <button className="cancel" onClick={cancel}>
            キャンセル
          </button>
        </section>
      )}

      {phase === "done" && result && (
        <section className="result">
          <h2>完成しました 🎉</h2>
          <p>
            {formatShrinkSummary(result)}・処理 {Math.round(result.totalSeconds)}
            秒
          </p>
          <ul className="outputs">
            {result.outputs.map((path) => (
              <li key={path}>
                <span className="output-name">{fileName(path)}</span>
                <button onClick={() => reveal(path)}>表示</button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </main>
  );
}

export default App;
