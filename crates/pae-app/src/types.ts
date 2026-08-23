// Rust 側 (src-tauri/src/commands.rs) の DTO と対応させた型定義

export interface BgmOpts {
  volume: number;
  fade_in_s: number;
  fade_out_s: number;
  ending_tail_s: number;
  voice_duck_db: number;
}

export interface AppConfig {
  default_bgm: string | null;
  bgm: BgmOpts;
  preset: string;
  output_dir: string | null;
  model: string;
  transcribe: boolean;
  target_lufs: number;
  ffmpeg_dir: string | null;
}

export interface MediaInfo {
  path: string;
  duration_ms: number;
  has_video: boolean;
  video_codec: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  audio_codec: string | null;
  sample_rate: number | null;
  channels: number | null;
}

export type Stage =
  | "probe"
  | "extract_audio"
  | "vad"
  | "timeline"
  | "render_video"
  | "mix_bgm"
  | "loudnorm"
  | "render_mp3"
  | "transcribe"
  | "write_outputs";

export interface ProgressEvent {
  stage: Stage;
  stageLabel: string;
  fraction: number | null;
  message: string | null;
}

export interface StageSeconds {
  stageLabel: string;
  seconds: number;
}

export interface JobResult {
  outputs: string[];
  sourceDurationMs: number;
  outputDurationMs: number;
  timings: StageSeconds[];
  totalSeconds: number;
  realTimeFactor: number;
}

export interface ModelInfo {
  name: string;
  approxSizeMb: number;
  description: string;
  downloaded: boolean;
}

// 進捗チェックリストの表示順。Rust 側の Stage::label と同じ文言にしている
export const STAGE_LABELS: Record<Stage, string> = {
  probe: "入力情報の取得",
  extract_audio: "音声抽出",
  vad: "無音検出",
  timeline: "編集タイムライン生成",
  render_video: "動画編集",
  mix_bgm: "BGM追加",
  loudnorm: "音量調整",
  render_mp3: "MP3出力",
  transcribe: "文字起こし",
  write_outputs: "ファイル出力",
};

export const STAGE_ORDER: Stage[] = [
  "probe",
  "extract_audio",
  "vad",
  "timeline",
  "render_video",
  "mix_bgm",
  "loudnorm",
  "render_mp3",
  "transcribe",
  "write_outputs",
];
