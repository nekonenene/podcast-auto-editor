//! GUI の波形表示用に、音声サンプルからピーク列を計算する

/// サンプル列を bucket_count 個の区間に分け、各区間のピーク (0.0〜1.0) を返す。
/// 全体の最大値で正規化するため、静かな録音でも波形の形が見える
pub fn compute_waveform(samples: &[i16], bucket_count: usize) -> Vec<f32> {
    if samples.is_empty() || bucket_count == 0 {
        return Vec::new();
    }
    let bucket_size = samples.len().div_ceil(bucket_count);
    let mut peaks: Vec<f32> = samples
        .chunks(bucket_size)
        .map(|chunk| {
            let peak = chunk.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            peak as f32 / i16::MAX as f32
        })
        .collect();

    let max = peaks.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        for p in &mut peaks {
            *p /= max;
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert!(compute_waveform(&[], 100).is_empty());
        assert!(compute_waveform(&[100], 0).is_empty());
    }

    #[test]
    fn peaks_are_normalized() {
        // 前半は小さい音、後半は大きい音
        let mut samples = vec![100i16; 1000];
        samples.extend(vec![10_000i16; 1000]);
        let peaks = compute_waveform(&samples, 2);
        assert_eq!(peaks.len(), 2);
        assert_eq!(peaks[1], 1.0);
        assert!((peaks[0] - 0.01).abs() < 0.001);
    }

    #[test]
    fn bucket_count_is_respected() {
        let samples = vec![1000i16; 16000];
        let peaks = compute_waveform(&samples, 100);
        assert_eq!(peaks.len(), 100);
    }
}
