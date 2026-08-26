//! 話者埋め込みモデルの定義。
//!
//! WeSpeaker の ResNet34 を VoxCeleb で学習させたものを ONNX へ変換したモデルを使う。
//! 配布元は sherpa-onnx のリリースページで、利用条件への同意なしにダウンロードできる。
//! Hugging Face 上の pyannote 版は同意が必要なため避けている。
//!
//! 同じ配布元にある CAM++ は、フレーム方向の平均を引いた特徴量を前提としておらず、
//! 手元の検証では同じ話者どうしのほうが別の話者より似ていない結果になった。
//! ResNet34 は平均を引く前提で学習されており、そのまま正しく働く

use crate::models::ModelSpec;

pub const EMBEDDING_MODEL: ModelSpec = ModelSpec {
    name: "wespeaker-resnet34",
    file_name: "wespeaker_en_voxceleb_resnet34_LM.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
    approx_size_mb: 26,
    description: "話者分離用の話者埋め込みモデル (WeSpeaker ResNet34)",
};
