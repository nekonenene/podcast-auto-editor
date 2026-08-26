//! 文字起こしモデル (GGML) の一覧。
//! ダウンロードとキャッシュの仕組みは話者分離と共通なので crate::models にある

pub use crate::models::{ModelManager, ModelSpec};

/// 対応モデル一覧。whisper.cpp (GGML) 形式であれば追加はここに1行足すだけでよい
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        name: "large-v3-turbo",
        file_name: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        approx_size_mb: 574,
        description: "推奨。日本語の自由会話に強い (量子化版)",
    },
    ModelSpec {
        name: "kotoba-v2.0",
        file_name: "ggml-kotoba-whisper-v2.0-q5_0.bin",
        url: "https://huggingface.co/kotoba-tech/kotoba-whisper-v2.0-ggml/resolve/main/ggml-kotoba-whisper-v2.0-q5_0.bin",
        approx_size_mb: 538,
        description: "日本語特化 (明瞭な音声向け・高速)",
    },
    ModelSpec {
        name: "tiny",
        file_name: "ggml-tiny.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        approx_size_mb: 78,
        description: "テスト用。精度は低い",
    },
];

pub fn find_model(name: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.name == name)
}
