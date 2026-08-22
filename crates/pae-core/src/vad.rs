//! Voice Activity Detection。Silero VAD (ONNX) で発話区間を検出する

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;
use crate::types::{SpeechSegment, VadParams};

/// VAD の抽象。将来別モデルへ差し替えられるよう trait にしている
pub trait Vad {
    /// 16kHz mono の i16 サンプル列から発話区間を検出する
    fn detect(
        &mut self,
        samples: &[i16],
        sample_rate: u32,
        params: &VadParams,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<Vec<SpeechSegment>>;
}

/// Silero VAD 実装。モデル (約2MB) は voice_activity_detector crate に同梱されており、
/// ネットワーク接続なしで動作する
pub struct SileroVad;

/// Silero VAD は 16kHz では 512 サンプル (32ms) 単位で推論する
const CHUNK_SIZE: usize = 512;

impl Vad for SileroVad {
    fn detect(
        &mut self,
        samples: &[i16],
        sample_rate: u32,
        params: &VadParams,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<Vec<SpeechSegment>> {
        if sample_rate != 16_000 {
            return Err(PaeError::Vad(format!(
                "サンプルレートは 16kHz である必要があります (入力: {sample_rate}Hz)"
            )));
        }

        let mut vad = voice_activity_detector::VoiceActivityDetector::builder()
            .sample_rate(sample_rate as i64)
            .chunk_size(CHUNK_SIZE)
            .build()
            .map_err(|e| PaeError::Vad(format!("Silero VAD の初期化に失敗: {e}")))?;

        let total_chunks = samples.len().div_ceil(CHUNK_SIZE);
        let mut probabilities = Vec::with_capacity(total_chunks);

        for (i, chunk) in samples.chunks(CHUNK_SIZE).enumerate() {
            // 進捗通知とキャンセル確認はチャンク数百個ごとで十分
            if i % 256 == 0 {
                cancel.check()?;
                on_progress(i as f32 / total_chunks as f32);
            }
            // 末尾の欠けたチャンクはゼロ埋めして推論する
            let prob = if chunk.len() == CHUNK_SIZE {
                vad.predict(chunk.iter().copied())
            } else {
                let mut padded = chunk.to_vec();
                padded.resize(CHUNK_SIZE, 0);
                vad.predict(padded)
            };
            probabilities.push(prob);
        }
        on_progress(1.0);

        Ok(probabilities_to_segments(
            &probabilities,
            chunk_duration_ms(sample_rate),
            samples.len() as u64 * 1000 / sample_rate as u64,
            params,
        ))
    }
}

fn chunk_duration_ms(sample_rate: u32) -> u64 {
    CHUNK_SIZE as u64 * 1000 / sample_rate as u64
}

/// チャンクごとの発話確率から発話区間を組み立てる
///
/// 1. しきい値にヒステリシスを持たせてチャンクを発話/無音に分類
///    （発話開始は threshold、終了は threshold - 0.15。境界でのばたつきを防ぐ）
/// 2. min_silence_ms 未満の無音を挟む発話同士をつなげる
/// 3. min_speech_ms 未満の短すぎる発話候補をノイズとして除去
///
/// パディング付与は TimelineGenerator 側の責務なのでここでは行わない
fn probabilities_to_segments(
    probabilities: &[f32],
    chunk_ms: u64,
    total_ms: u64,
    params: &VadParams,
) -> Vec<SpeechSegment> {
    let exit_threshold = (params.threshold - 0.15).max(0.01);

    let mut raw: Vec<SpeechSegment> = Vec::new();
    let mut current_start: Option<u64> = None;

    for (i, &prob) in probabilities.iter().enumerate() {
        let t = i as u64 * chunk_ms;
        match current_start {
            None if prob >= params.threshold => current_start = Some(t),
            Some(start) if prob < exit_threshold => {
                raw.push(SpeechSegment {
                    start_ms: start,
                    end_ms: t,
                });
                current_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = current_start {
        raw.push(SpeechSegment {
            start_ms: start,
            end_ms: total_ms,
        });
    }

    // 短い無音を挟む発話をつなげる（息継ぎ程度の間で区間が分断されるのを防ぐ）
    let mut merged: Vec<SpeechSegment> = Vec::with_capacity(raw.len());
    for seg in raw {
        match merged.last_mut() {
            Some(last) if seg.start_ms - last.end_ms < params.min_silence_ms => {
                last.end_ms = seg.end_ms;
            }
            _ => merged.push(seg),
        }
    }

    // 短すぎる発話候補を除去。ただし相槌を消さないよう min_speech_ms は小さく保つこと
    merged
        .into_iter()
        .filter(|s| s.duration_ms() >= params.min_speech_ms)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> VadParams {
        VadParams::default() // threshold 0.4, min_speech 100ms, min_silence 250ms
    }

    /// 32ms チャンク × n 個の確率列を作るヘルパ
    fn probs(pattern: &[(usize, f32)]) -> Vec<f32> {
        pattern
            .iter()
            .flat_map(|&(count, p)| std::iter::repeat_n(p, count))
            .collect()
    }

    #[test]
    fn detects_simple_speech_run() {
        // 10チャンク無音 → 20チャンク発話 → 10チャンク無音
        let p = probs(&[(10, 0.1), (20, 0.9), (10, 0.1)]);
        let segs = probabilities_to_segments(&p, 32, 40 * 32, &params());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_ms, 320);
        assert_eq!(segs[0].end_ms, 960);
    }

    #[test]
    fn merges_speech_across_short_silence() {
        // 発話 → 192ms (6チャンク) の無音 → 発話。min_silence 250ms 未満なので結合される
        let p = probs(&[(10, 0.9), (6, 0.1), (10, 0.9), (5, 0.1)]);
        let segs = probabilities_to_segments(&p, 32, 31 * 32, &params());
        assert_eq!(segs.len(), 1);
    }

    #[test]
    fn keeps_short_aizuchi_above_min_speech() {
        // 4チャンク = 128ms の相槌は min_speech 100ms 以上なので残る
        let p = probs(&[(20, 0.1), (4, 0.9), (20, 0.1)]);
        let segs = probabilities_to_segments(&p, 32, 44 * 32, &params());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].duration_ms(), 128);
    }

    #[test]
    fn drops_too_short_noise() {
        // 2チャンク = 64ms は min_speech 100ms 未満なので除去
        let p = probs(&[(20, 0.1), (2, 0.9), (20, 0.1)]);
        let segs = probabilities_to_segments(&p, 32, 42 * 32, &params());
        assert!(segs.is_empty());
    }

    #[test]
    fn hysteresis_keeps_wavering_speech_together() {
        // 発話中に確率が 0.3 まで下がっても exit しきい値 (0.25) 以上なら継続
        let p = probs(&[(10, 0.9), (5, 0.3), (10, 0.9), (10, 0.1)]);
        let segs = probabilities_to_segments(&p, 32, 35 * 32, &params());
        assert_eq!(segs.len(), 1);
    }

    #[test]
    fn speech_until_end_of_audio() {
        let p = probs(&[(10, 0.1), (10, 0.9)]);
        let segs = probabilities_to_segments(&p, 32, 20 * 32, &params());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].end_ms, 640);
    }
}
