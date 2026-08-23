// Rust 側 (src-tauri/src/commands.rs) の DTO と対応させた型定義

export interface BgmOpts {
  volume: number;
  fade_in_s: number;
  fade_out_s: number;
  ending_tail_s: number;
  voice_duck_db: number;
}

export interface OutputSelection {
  edited_mp4: boolean;
  podcast_mp3: boolean;
  timeline_json: boolean;
  transcript_txt: boolean;
  transcript_json: boolean;
  transcript_srt: boolean;
  transcript_md: boolean;
}

export const DEFAULT_OUTPUTS: OutputSelection = {
  edited_mp4: true,
  podcast_mp3: true,
  timeline_json: true,
  transcript_txt: true,
  transcript_json: true,
  transcript_srt: true,
  transcript_md: true,
};

// 設定画面のグループ分け。文字起こしから作られるファイルは
// メイン画面の「文字起こし」が ON のときだけ出力される
export const MEDIA_OUTPUT_KEYS = [
  "edited_mp4",
  "podcast_mp3",
  "timeline_json",
] as const satisfies readonly (keyof OutputSelection)[];

export const TRANSCRIPT_OUTPUT_KEYS = [
  "transcript_txt",
  "transcript_json",
  "transcript_srt",
  "transcript_md",
] as const satisfies readonly (keyof OutputSelection)[];

export const OUTPUT_LABELS: Record<keyof OutputSelection, string> = {
  edited_mp4: "編集済み動画 (MP4)",
  podcast_mp3: "Podcast 用音声 (MP3)",
  timeline_json: "編集タイムライン (JSON)",
  transcript_txt: "文字起こし (TXT)",
  transcript_json: "文字起こし (JSON)",
  transcript_srt: "字幕 (SRT)",
  transcript_md: "文字起こし (Markdown)",
};

export interface AppConfig {
  default_bgm: string | null;
  bgm: BgmOpts;
  preset: string;
  output_dir: string | null;
  model: string;
  transcribe: boolean;
  target_lufs: number;
  ffmpeg_dir: string | null;
  outputs: OutputSelection;
  mp3_bitrate_kbps: number;
}

// 0 は VBR 高音質 (可変ビットレート) を表す
export const MP3_BITRATES = [64, 96, 128, 192, 256, 320, 0] as const;

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
  render_video: "カット編集",
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
