import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  AppConfig,
  JobResult,
  MediaInfo,
  ModelInfo,
  OutputSelection,
  ProgressEvent,
  Stage,
} from "./types";
import {
  DEFAULT_OUTPUTS,
  MEDIA_OUTPUT_KEYS,
  OUTPUT_LABELS,
  STAGE_LABELS,
  STAGE_ORDER,
  TRANSCRIPT_OUTPUT_KEYS,
} from "./types";
import "./App.css";

const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv"];
const AUDIO_EXTENSIONS = ["mp3", "wav", "m4a", "aac", "flac"];

type UiPhase = "idle" | "running" | "done" | "error";

function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}分${String(s).padStart(2, "0")}秒`;
}

function fileName(path: string): string {
  return path.split("/").pop() ?? path;
}

function parentDir(path: string): string {
  const index = path.lastIndexOf("/");
  return index > 0 ? path.slice(0, index) : path;
}

function extensionOf(path: string): string {
  return path.split(".").pop()?.toLowerCase() ?? "";
}

function App() {
  const [video, setVideo] = useState<string | null>(null);
  const [videoInfo, setVideoInfo] = useState<MediaInfo | null>(null);
  const [bgm, setBgm] = useState<string | null>(null);
  const [preset, setPreset] = useState("natural");
  const [bgmVolume, setBgmVolume] = useState(0.15);
  const [transcribe, setTranscribe] = useState(true);
  const [model, setModel] = useState("large-v3-turbo");
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [outputDir, setOutputDir] = useState<string | null>(null);
  const [fadeInS, setFadeInS] = useState(2.0);
  const [fadeOutS, setFadeOutS] = useState(4.0);
  const [endingTailS, setEndingTailS] = useState(5.0);
  const [voiceDuck, setVoiceDuck] = useState(true);
  const [outputs, setOutputs] = useState<OutputSelection>(DEFAULT_OUTPUTS);
  const [showSettings, setShowSettings] = useState(false);

  const [previewing, setPreviewing] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(false);
  const previewAudioRef = useRef<HTMLAudioElement | null>(null);
  const previewUrlRef = useRef<string | null>(null);

  const [phase, setPhase] = useState<UiPhase>("idle");
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<JobResult | null>(null);

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
        setTranscribe(config.transcribe);
        setModel(config.model);
        setOutputDir(config.output_dir);
      })
      .catch((e) => setError(String(e)));
    invoke<ModelInfo[]>("list_models")
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  const chooseVideo = useCallback(async (path: string) => {
    setError(null);
    setResult(null);
    setPhase("idle");
    setVideo(path);
    setVideoInfo(null);
    // 出力先が未設定なら、動画と同じ場所の podcast-output をデフォルトにする
    setOutputDir((current) => current ?? `${parentDir(path)}/podcast-output`);
    try {
      setVideoInfo(await invoke<MediaInfo>("probe_media", { path }));
    } catch (e) {
      setError(String(e));
      setVideo(null);
    }
  }, []);

  // ウィンドウへのドラッグ&ドロップ。動画は入力、音声ファイルは BGM として扱う。
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
          setBgm(path);
        }
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [chooseVideo]);

  const selectVideo = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "動画", extensions: VIDEO_EXTENSIONS }],
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

  // 設定画面での変更は即座に保存する
  const saveSettings = useCallback(
    (nextOutputs: OutputSelection, nextVoiceDuck: boolean) => {
      setOutputs(nextOutputs);
      setVoiceDuck(nextVoiceDuck);
      invoke("save_settings", {
        update: {
          outputs: nextOutputs,
          voiceDuckDb: nextVoiceDuck ? -4.0 : 0.0,
        },
      }).catch((e) => setError(String(e)));
    },
    [],
  );

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
      if (stage === "transcribe" || stage === "write_outputs") return transcribe;
      return true;
    });
  }, [bgm, transcribe]);

  const currentStageIndex = progress
    ? activeStages.indexOf(progress.stage)
    : -1;

  const selectedModel = models.find((m) => m.name === model);
  const running = phase === "running";
  const anyOutputSelected = Object.values(outputs).some(Boolean);

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
                    saveSettings(
                      { ...outputs, [key]: e.target.checked },
                      voiceDuck,
                    )
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
                        saveSettings(
                          { ...outputs, [key]: e.target.checked },
                          voiceDuck,
                        )
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
            <label>音声処理</label>
            <label className="row">
              <input
                type="checkbox"
                checked={voiceDuck}
                onChange={(e) => saveSettings(outputs, e.target.checked)}
              />
              声の帯域で BGM を下げる (聞き取りやすくなります)
            </label>
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
          <label>動画</label>
          <button className="picker" onClick={selectVideo} disabled={running}>
            {video ? fileName(video) : "クリックして選択 (ドラッグ&ドロップ可)"}
          </button>
          {videoInfo && (
            <p className="hint">
              {formatDuration(videoInfo.duration_ms)} ・ {videoInfo.width}x
              {videoInfo.height} ・ {videoInfo.video_codec}/
              {videoInfo.audio_codec}
            </p>
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
            <option value="natural">Natural — 自然な間を多めに残す</option>
            <option value="standard">Standard — 通常の Podcast 編集</option>
            <option value="aggressive">Aggressive — テンポ重視</option>
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
            {formatDuration(result.sourceDurationMs)} →{" "}
            {formatDuration(result.outputDurationMs)}（
            {(
              100 *
              (1 - result.outputDurationMs / result.sourceDurationMs)
            ).toFixed(1)}
            % 短縮）・処理 {Math.round(result.totalSeconds)}秒
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
