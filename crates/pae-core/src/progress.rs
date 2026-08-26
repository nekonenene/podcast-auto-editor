use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{PaeError, Result};

/// パイプラインの処理段階。CLI と GUI の両方で進捗表示に使う
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Probe,
    ExtractAudio,
    Vad,
    Timeline,
    RenderVideo,
    MixBgm,
    Loudnorm,
    RenderMp3,
    Transcribe,
    Diarize,
    WriteOutputs,
}

impl Stage {
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Probe => "入力情報の取得",
            Stage::ExtractAudio => "音声抽出",
            Stage::Vad => "無音検出",
            Stage::Timeline => "編集タイムライン生成",
            Stage::RenderVideo => "カット編集",
            Stage::MixBgm => "BGM追加",
            Stage::Loudnorm => "音量調整",
            Stage::RenderMp3 => "MP3出力",
            Stage::Transcribe => "文字起こし",
            Stage::Diarize => "話者分離",
            Stage::WriteOutputs => "ファイル出力",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressReport {
    pub stage: Stage,
    /// ステージ内の進捗 0.0〜1.0。所要が読めない処理では None
    pub fraction: Option<f32>,
    pub message: Option<String>,
}

/// 進捗の通知先。CLI は indicatif、GUI は Tauri の Channel で実装する
pub trait ProgressSink: Send + Sync {
    fn report(&self, report: &ProgressReport);
}

/// 進捗表示が不要な場面（テスト等)で使う実装
pub struct NullSink;

impl ProgressSink for NullSink {
    fn report(&self, _report: &ProgressReport) {}
}

/// 処理のキャンセル用トークン。clone してスレッドや子プロセス監視へ配る
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 内部フラグへの参照を返す。FFI コールバック (whisper の abort callback 等) へ
    /// ポインタとして渡す用途に限定すること
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// キャンセル済みならエラーを返す。処理ループの節目で呼ぶ
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(PaeError::Cancelled)
        } else {
            Ok(())
        }
    }
}
