//! 話者埋め込みの計算。ONNX モデルへ通す部分なので I/O を持つ

use std::path::Path;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::{PaeError, Result};

/// 音声の一区間を、話者の声質を表すベクトルへ変換する
pub struct SpeakerEmbedder {
    session: Session,
}

impl SpeakerEmbedder {
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.exists() {
            return Err(PaeError::Diarize(format!(
                "モデルファイルがありません: {}",
                model_path.display()
            )));
        }
        let session = Session::builder()
            .and_then(|b| b.with_optimization_level(GraphOptimizationLevel::Level3))
            .and_then(|b| b.with_intra_threads(1))
            .and_then(|b| b.commit_from_file(model_path))
            .map_err(|e| PaeError::Diarize(format!("モデルの読み込みに失敗: {e}")))?;
        Ok(Self { session })
    }

    /// 16kHz mono の i16 サンプル列から埋め込みを求める
    pub fn embed(&mut self, samples: &[i16]) -> Result<Vec<f32>> {
        // 波形は 16bit 整数の範囲のまま渡す。
        // モデルに埋め込まれた情報が normalize_samples = 0 を指しており、
        // 学習時も -1.0〜1.0 へ正規化せずに特徴量を作っているため
        let waveform: Vec<f32> = samples.iter().map(|&s| s as f32).collect();

        // knf-rs の compute_fbank はフレーム方向の平均を引くところまで済ませてくれる。
        // WeSpeaker の ResNet34 はこの平均を引いた特徴量を前提に学習されている
        let features = knf_rs::compute_fbank(&waveform)
            .map_err(|e| PaeError::Diarize(format!("特徴量の計算に失敗: {e}")))?;
        let features = features.insert_axis(ndarray::Axis(0));

        let outputs = Tensor::from_array(features)
            .map_err(|e| PaeError::Diarize(format!("入力テンソルの作成に失敗: {e}")))
            .and_then(|input| {
                self.session
                    .run(ort::inputs!["feats" => input])
                    .map_err(|e| PaeError::Diarize(format!("推論に失敗: {e}")))
            })?;

        let embedding = outputs
            .get("embs")
            .ok_or_else(|| PaeError::Diarize("出力 embs が見つかりません".into()))?
            .try_extract_tensor::<f32>()
            .map_err(|e| PaeError::Diarize(format!("出力の取り出しに失敗: {e}")))?;
        Ok(embedding.1.to_vec())
    }
}
