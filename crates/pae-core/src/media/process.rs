//! タイムラインに基づく動画の再構成、BGM ミックス、ラウドネス調整。
//! ffmpeg のフィルタ式を生成する部分は純粋関数にしてテスト可能にしている

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;

use super::ffmpeg::Ffmpeg;

/// keep_ranges (ミリ秒) から select/aselect 用のフィルタスクリプトを生成する。
///
/// カット後に setpts / asetpts でタイムスタンプをゼロから振り直すこと、
/// aresample=async=1 で微小なギャップを吸収することが音ズレ防止の要点
pub fn build_cut_filter_script(keep_ranges_ms: &[(u64, u64)]) -> String {
    let expr: Vec<String> = keep_ranges_ms
        .iter()
        .map(|(start, end)| {
            format!(
                "between(t,{:.3},{:.3})",
                *start as f64 / 1000.0,
                *end as f64 / 1000.0
            )
        })
        .collect();
    let expr = expr.join("+");
    format!(
        "[0:v]select='{expr}',setpts=N/FRAME_RATE/TB[v];\n\
         [0:a]aselect='{expr}',asetpts=N/SR/TB,aresample=async=1[a]\n"
    )
}

/// 出力動画のエンコード設定
#[derive(Debug, Clone)]
pub struct VideoEncodeOpts {
    pub encoder: String,
    pub bitrate: String,
}

impl VideoEncodeOpts {
    /// プラットフォームと解像度から適切なエンコーダとビットレートを選ぶ。
    /// videotoolbox は x264 より同ビットレートでの品質が落ちるため、少し多めに盛る
    pub fn auto(height: Option<u32>) -> Self {
        let encoder = if cfg!(target_os = "macos") {
            "h264_videotoolbox"
        } else {
            // Windows では h264_mf を想定。開発用フォールバックとして libx264
            "libx264"
        };
        let bitrate = match height {
            Some(h) if h >= 1080 => "6M",
            Some(h) if h >= 720 => "4M",
            _ => "2500k",
        };
        Self {
            encoder: encoder.into(),
            bitrate: bitrate.into(),
        }
    }
}

/// タイムラインの keep_ranges に従って動画をカット・再結合する。
/// 全再エンコードだが、デコードは1回で済み A/V 同期が最も安定する方式
#[allow(clippy::too_many_arguments)]
pub fn cut_video(
    ffmpeg: &Ffmpeg,
    input: &Path,
    keep_ranges_ms: &[(u64, u64)],
    output: &Path,
    output_duration_ms: u64,
    encode: &VideoEncodeOpts,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    if keep_ranges_ms.is_empty() {
        return Err(PaeError::InvalidTimeline(
            "出力に残す区間がありません".into(),
        ));
    }

    let script = build_cut_filter_script(keep_ranges_ms);
    let script_file = tempfile::Builder::new()
        .prefix("pae-filter-")
        .suffix(".txt")
        .tempfile()?;
    std::fs::write(script_file.path(), &script)?;

    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        // "-/opt file" は値をファイルから読む ffmpeg の構文。
        // フィルタ式は長尺動画で数百節になり引数の長さ制限を超えうるためファイル渡しにする
        // (旧 -filter_complex_script は ffmpeg 9 で廃止)
        "-/filter_complex".into(),
        script_file.path().display().to_string(),
        "-map".into(),
        "[v]".into(),
        "-map".into(),
        "[a]".into(),
        "-c:v".into(),
        encode.encoder.clone(),
        "-b:v".into(),
        encode.bitrate.clone(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-fps_mode".into(),
        "cfr".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-ar".into(),
        "48000".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ];
    ffmpeg.run(&args, Some(output_duration_ms), on_progress, cancel)
}

/// BGM のミックス設定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgmOpts {
    /// BGM の音量（会話音声に対する倍率）。0.15 = 約 -16.5dB
    pub volume: f32,
    pub fade_in_s: f32,
    pub fade_out_s: f32,
}

impl Default for BgmOpts {
    fn default() -> Self {
        Self {
            volume: 0.15,
            fade_in_s: 2.0,
            fade_out_s: 4.0,
        }
    }
}

/// BGM ミックス用のフィルタ式を生成する。
///
/// - `-stream_loop -1` により BGM は入力段で無限ループしている前提
/// - amix の duration=first で本編の長さに合わせて終了する
/// - normalize=0 にしないと amix が会話音声の音量を半分に下げてしまう
pub fn build_bgm_filter(main_duration_ms: u64, opts: &BgmOpts) -> String {
    let duration_s = main_duration_ms as f64 / 1000.0;
    let fade_out_start = (duration_s - opts.fade_out_s as f64).max(0.0);
    format!(
        "[1:a]volume={volume},afade=t=in:st=0:d={fade_in},afade=t=out:st={fade_out_start:.3}:d={fade_out}[bgm];\
         [0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]",
        volume = opts.volume,
        fade_in = opts.fade_in_s,
        fade_out = opts.fade_out_s,
    )
}

/// 編集済み動画に BGM をループ・フェード付きでミックスする。映像は無劣化コピー
#[allow(clippy::too_many_arguments)]
pub fn mix_bgm(
    ffmpeg: &Ffmpeg,
    input: &Path,
    bgm: &Path,
    output: &Path,
    main_duration_ms: u64,
    opts: &BgmOpts,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    if !bgm.exists() {
        return Err(PaeError::InputNotFound(bgm.to_path_buf()));
    }
    let filter = build_bgm_filter(main_duration_ms, opts);
    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-stream_loop".into(),
        "-1".into(),
        "-i".into(),
        bgm.display().to_string(),
        "-filter_complex".into(),
        filter,
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "[a]".into(),
        "-c:v".into(),
        "copy".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-ar".into(),
        "48000".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ];
    ffmpeg.run(&args, Some(main_duration_ms), on_progress, cancel)
}

/// loudnorm 1パス目の測定結果
#[derive(Debug, Clone, Deserialize)]
pub struct LoudnessMeasurement {
    pub input_i: String,
    pub input_tp: String,
    pub input_lra: String,
    pub input_thresh: String,
    pub target_offset: String,
}

/// loudnorm 2パス処理のターゲット
#[derive(Debug, Clone, Copy)]
pub struct LoudnormTarget {
    /// integrated loudness (LUFS)。Apple Podcasts 標準は -16
    pub i: f64,
    pub tp: f64,
    pub lra: f64,
}

impl Default for LoudnormTarget {
    fn default() -> Self {
        Self {
            i: -16.0,
            tp: -1.5,
            lra: 11.0,
        }
    }
}

/// loudnorm 1パス目: 音声全体を測定する
pub fn measure_loudness(
    ffmpeg: &Ffmpeg,
    input: &Path,
    target: &LoudnormTarget,
    cancel: &CancelToken,
) -> Result<LoudnessMeasurement> {
    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-vn".into(),
        "-af".into(),
        format!(
            "loudnorm=I={}:TP={}:LRA={}:print_format=json",
            target.i, target.tp, target.lra
        ),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let stderr = ffmpeg.run(&args, None, &mut |_| {}, cancel)?;
    parse_loudnorm_json(&stderr)
}

/// loudnorm は測定結果の JSON を stderr の末尾に出力する。
/// 他のログと混ざるため、最後の '{' から始まる JSON ブロックを取り出す
fn parse_loudnorm_json(stderr: &str) -> Result<LoudnessMeasurement> {
    let start = stderr
        .rfind('{')
        .ok_or_else(|| PaeError::ProbeParse("loudnorm の測定 JSON が見つかりません".into()))?;
    let end = stderr[start..]
        .find('}')
        .map(|i| start + i + 1)
        .ok_or_else(|| PaeError::ProbeParse("loudnorm の測定 JSON が閉じていません".into()))?;
    serde_json::from_str(&stderr[start..end])
        .map_err(|e| PaeError::ProbeParse(format!("loudnorm JSON の解析に失敗: {e}")))
}

/// loudnorm 2パス目: 測定値を使って線形ゲインで正規化する。
/// linear=true により声質を変えるダイナミック処理を避ける。映像は無劣化コピー
#[allow(clippy::too_many_arguments)]
pub fn apply_loudnorm(
    ffmpeg: &Ffmpeg,
    input: &Path,
    output: &Path,
    target: &LoudnormTarget,
    measured: &LoudnessMeasurement,
    duration_ms: u64,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    let filter = format!(
        "loudnorm=I={}:TP={}:LRA={}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
        target.i,
        target.tp,
        target.lra,
        measured.input_i,
        measured.input_tp,
        measured.input_lra,
        measured.input_thresh,
        measured.target_offset,
    );
    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-af".into(),
        filter,
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "0:a".into(),
        "-c:v".into(),
        "copy".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        // loudnorm は内部で 192kHz にアップサンプルするため明示的に戻す
        "-ar".into(),
        "48000".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ];
    ffmpeg.run(&args, Some(duration_ms), on_progress, cancel)
}

/// 完成した動画から Podcast 用 MP3 を書き出す
pub fn encode_mp3(
    ffmpeg: &Ffmpeg,
    input: &Path,
    output: &Path,
    duration_ms: u64,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    let args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-vn".into(),
        "-c:a".into(),
        "libmp3lame".into(),
        "-q:a".into(),
        "2".into(),
        output.display().to_string(),
    ];
    ffmpeg.run(&args, Some(duration_ms), on_progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_filter_script_snapshot() {
        let script = build_cut_filter_script(&[(0, 2200), (5000, 6000)]);
        insta::assert_snapshot!(script, @r"
        [0:v]select='between(t,0.000,2.200)+between(t,5.000,6.000)',setpts=N/FRAME_RATE/TB[v];
        [0:a]aselect='between(t,0.000,2.200)+between(t,5.000,6.000)',asetpts=N/SR/TB,aresample=async=1[a]
        ");
    }

    #[test]
    fn bgm_filter_snapshot() {
        let filter = build_bgm_filter(60_000, &BgmOpts::default());
        insta::assert_snapshot!(filter, @"[1:a]volume=0.15,afade=t=in:st=0:d=2,afade=t=out:st=56.000:d=4[bgm];[0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]");
    }

    #[test]
    fn bgm_fade_out_clamps_to_zero() {
        let opts = BgmOpts {
            fade_out_s: 10.0,
            ..BgmOpts::default()
        };
        let filter = build_bgm_filter(5_000, &opts);
        assert!(filter.contains("afade=t=out:st=0.000:d=10"));
    }

    #[test]
    fn parses_loudnorm_json_from_noisy_stderr() {
        let stderr = "frame=100 ...\n[Parsed_loudnorm_0 @ 0x123]\n{\n\
            \"input_i\" : \"-23.11\",\n\
            \"input_tp\" : \"-4.20\",\n\
            \"input_lra\" : \"6.80\",\n\
            \"input_thresh\" : \"-33.51\",\n\
            \"output_i\" : \"-16.00\",\n\
            \"output_tp\" : \"-1.50\",\n\
            \"output_lra\" : \"5.90\",\n\
            \"output_thresh\" : \"-26.50\",\n\
            \"normalization_type\" : \"linear\",\n\
            \"target_offset\" : \"0.30\"\n}\n";
        let m = parse_loudnorm_json(stderr).unwrap();
        assert_eq!(m.input_i, "-23.11");
        assert_eq!(m.target_offset, "0.30");
    }
}
