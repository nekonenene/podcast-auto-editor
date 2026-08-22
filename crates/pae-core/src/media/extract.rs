use std::path::Path;

use crate::error::Result;
use crate::progress::CancelToken;

use super::ffmpeg::Ffmpeg;

/// VAD・文字起こしで使う 16kHz サンプルレート。Silero VAD と Whisper の両方がこの値を要求する
pub const ANALYSIS_SAMPLE_RATE: u32 = 16_000;

/// 動画から解析用の音声 (16kHz mono WAV) を抽出する
pub fn extract_analysis_wav(
    ffmpeg: &Ffmpeg,
    input: &Path,
    output_wav: &Path,
    duration_ms: u64,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<()> {
    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        ANALYSIS_SAMPLE_RATE.to_string(),
        "-c:a".into(),
        "pcm_s16le".into(),
        output_wav.display().to_string(),
    ];
    ffmpeg.run(&args, Some(duration_ms), on_progress, cancel)?;
    Ok(())
}

/// WAV ファイルを i16 サンプル列として読み込む。
/// 60分の 16kHz mono でも約115MB なのでメモリに載せて問題ない
pub fn read_wav_samples(path: &Path) -> Result<(Vec<i16>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| crate::error::PaeError::ProbeParse(format!("WAV 読み込み失敗: {e}")))?;
    let spec = reader.spec();
    let samples: std::result::Result<Vec<i16>, _> = reader.samples::<i16>().collect();
    let samples = samples.map_err(|e| {
        crate::error::PaeError::ProbeParse(format!("WAV サンプル読み込み失敗: {e}"))
    })?;
    Ok((samples, spec.sample_rate))
}
