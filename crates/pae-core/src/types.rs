use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// VAD が検出した発話区間。時刻はすべて入力メディア先頭からのミリ秒
///
/// 時間を f64 秒ではなく u64 ミリ秒で持つのは、境界値の比較（1500ms ちょうど等）と
/// セグメント連続性の検証を浮動小数の誤差から守るため
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeechSegment {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl SpeechSegment {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Speech,
    Silence,
}

/// セグメントをどう扱うか
/// 将来のフィラー削除や手動カットは Speech + Remove として表現できる
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentAction {
    Keep,
    Compress,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSegment {
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub kind: SegmentKind,
    pub action: SegmentAction,
    /// 出力に残す長さ。Keep なら全長、Compress なら短縮後の長さ、Remove なら 0
    pub keep_duration_ms: u64,
}

impl TimelineSegment {
    pub fn source_duration_ms(&self) -> u64 {
        self.source_end_ms - self.source_start_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineStats {
    pub source_duration_ms: u64,
    pub output_duration_ms: u64,
    pub silence_count: usize,
    pub compressed_count: usize,
}

/// 編集内容を表す中間データ
/// segments は入力の 0ms から末尾までを隙間なくカバーする不変条件を持つ
/// JSON として保存でき、手修正して再レンダリングにも使える
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditTimeline {
    /// timeline.json のスキーマバージョン。互換性の判断に使う
    pub version: u32,
    pub source_path: PathBuf,
    pub source_duration_ms: u64,
    pub preset_name: String,
    pub vad_params: VadParams,
    pub segments: Vec<TimelineSegment>,
    pub stats: TimelineStats,
}

pub const TIMELINE_VERSION: u32 = 1;

/// VAD のパラメータ。相槌や息継ぎを取りこぼさないよう recall 重視の初期値
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VadParams {
    /// 発話と判定する確率のしきい値。下げるほど小さな声を拾う
    pub threshold: f32,
    /// これより短い発話候補はノイズとして無視する
    /// 日本語の短い相槌（「うん」等）が 100〜200ms 程度なので大きくしすぎない
    pub min_speech_ms: u64,
    /// これより短い無音は発話の一部とみなして前後をつなげる
    pub min_silence_ms: u64,
    /// 発話区間の前に付ける余白。語頭の欠けを防ぐ
    pub pad_before_ms: u64,
    /// 発話区間の後に付ける余白。語尾や息継ぎの欠けを防ぐ
    pub pad_after_ms: u64,
}

impl Default for VadParams {
    fn default() -> Self {
        Self {
            threshold: 0.4,
            min_speech_ms: 100,
            min_silence_ms: 250,
            pad_before_ms: 150,
            pad_after_ms: 250,
        }
    }
}

/// 無音短縮プリセット。「無音を削除する」のではなく
/// 「長すぎる無音を自然な長さまで短縮する」ためのパラメータ
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    /// この長さ以上の無音を短縮対象にする
    pub compress_threshold_ms: u64,
    /// 短縮後に残す無音の長さ
    pub target_silence_ms: u64,
    /// 動画の冒頭・末尾の無音は会話の間ではないため、
    /// しきい値未満でも target_silence_ms まで短縮する
    pub trim_edges: bool,
}

impl Preset {
    pub fn natural() -> Self {
        Self {
            name: "natural".into(),
            compress_threshold_ms: 3000,
            target_silence_ms: 1200,
            trim_edges: true,
        }
    }

    pub fn standard() -> Self {
        Self {
            name: "standard".into(),
            compress_threshold_ms: 1500,
            target_silence_ms: 800,
            trim_edges: true,
        }
    }

    pub fn aggressive() -> Self {
        Self {
            name: "aggressive".into(),
            compress_threshold_ms: 700,
            target_silence_ms: 300,
            trim_edges: true,
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "natural" => Some(Self::natural()),
            "standard" => Some(Self::standard()),
            "aggressive" => Some(Self::aggressive()),
            _ => None,
        }
    }
}

/// 文字起こしの1セグメント。話者分離を将来追加できるよう speaker を予約している
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// ffprobe で取得する入力メディアの情報
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub duration_ms: u64,
    pub has_video: bool,
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// フレームレート (例: 30/1 なら 30.0)
    pub fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
}
