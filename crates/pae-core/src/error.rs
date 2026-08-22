use std::path::PathBuf;

/// アプリ全体で使うエラー型
/// 外部プロセスや外部ライブラリの失敗を握りつぶさず、原因を保持したまま上へ伝える
#[derive(Debug, thiserror::Error)]
pub enum PaeError {
    #[error("入力ファイルが見つかりません: {0}")]
    InputNotFound(PathBuf),

    #[error("ffmpeg / ffprobe が見つかりません。パス: {0}")]
    FfmpegNotFound(String),

    #[error("{tool} の実行に失敗しました (exit code: {code:?})\n{stderr}")]
    ExternalProcess {
        tool: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("メディア情報の解析に失敗しました: {0}")]
    ProbeParse(String),

    #[error("VAD の実行に失敗しました: {0}")]
    Vad(String),

    #[error("文字起こしに失敗しました: {0}")]
    Transcribe(String),

    #[error("モデルのダウンロードに失敗しました: {0}")]
    ModelDownload(String),

    #[error("タイムラインが不正です: {0}")]
    InvalidTimeline(String),

    #[error("処理がキャンセルされました")]
    Cancelled,

    #[error("設定の読み書きに失敗しました: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, PaeError>;
