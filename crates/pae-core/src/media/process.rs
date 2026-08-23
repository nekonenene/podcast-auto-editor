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
/// aresample=async=1 で微小なギャップを吸収することが音ズレ防止の要点。
///
/// `tail_ms` を指定すると末尾に「余韻」を足す。映像は最終フレームの静止
/// (tpad clone)、音声は無音のパディングで、あとから BGM だけが数秒残って
/// フェードアウトする Podcast らしいエンディングを作るために使う。
///
/// `has_video` が false のとき (mp3 や wav などの音声入力) は音声チェーンだけを作る
pub fn build_cut_filter_script(
    keep_ranges_ms: &[(u64, u64)],
    tail_ms: u64,
    has_video: bool,
) -> String {
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
    let (video_tail, audio_tail) = if tail_ms > 0 {
        let tail_s = tail_ms as f64 / 1000.0;
        (
            format!(",tpad=stop_duration={tail_s:.3}:stop_mode=clone"),
            format!(",apad=pad_dur={tail_s:.3}"),
        )
    } else {
        (String::new(), String::new())
    };
    let audio_chain =
        format!("[0:a]aselect='{expr}',asetpts=N/SR/TB,aresample=async=1{audio_tail}[a]\n");
    if has_video {
        format!("[0:v]select='{expr}',setpts=N/FRAME_RATE/TB{video_tail}[v];\n{audio_chain}")
    } else {
        audio_chain
    }
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

/// タイムラインの keep_ranges に従って入力をカット・再結合する。
/// 全再エンコードだが、デコードは1回で済み A/V 同期が最も安定する方式。
/// 音声のみの入力では音声チェーンだけを処理する (出力は .flac を想定)
#[allow(clippy::too_many_arguments)]
pub fn cut_media(
    ffmpeg: &Ffmpeg,
    input: &Path,
    keep_ranges_ms: &[(u64, u64)],
    output: &Path,
    output_duration_ms: u64,
    tail_ms: u64,
    has_video: bool,
    encode: &VideoEncodeOpts,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    if keep_ranges_ms.is_empty() {
        return Err(PaeError::InvalidTimeline(
            "出力に残す区間がありません".into(),
        ));
    }

    let script = build_cut_filter_script(keep_ranges_ms, tail_ms, has_video);
    let script_file = tempfile::Builder::new()
        .prefix("pae-filter-")
        .suffix(".txt")
        .tempfile()?;
    std::fs::write(script_file.path(), &script)?;

    let mut args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        // "-/opt file" は値をファイルから読む ffmpeg の構文。
        // フィルタ式は長尺動画で数百節になり引数の長さ制限を超えうるためファイル渡しにする
        // (旧 -filter_complex_script は ffmpeg 9 で廃止)
        "-/filter_complex".into(),
        script_file.path().display().to_string(),
    ];
    if has_video {
        args.extend(
            [
                "-map",
                "[v]",
                "-map",
                "[a]",
                "-c:v",
                &encode.encoder,
                "-b:v",
                &encode.bitrate,
                "-pix_fmt",
                "yuv420p",
                "-fps_mode",
                "cfr",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-ar",
                "48000",
                "-movflags",
                "+faststart",
            ]
            .map(String::from),
        );
    } else {
        // 音声のみ: 中間ファイルは可逆の FLAC にして多段エンコードの劣化を避ける
        args.extend(["-map", "[a]", "-c:a", "flac", "-ar", "48000"].map(String::from));
    }
    args.push(output.display().to_string());
    ffmpeg.run(&args, Some(output_duration_ms), on_progress, cancel)
}

/// BGM のミックス設定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgmOpts {
    /// BGM の音量（会話音声に対する倍率）。0.15 = 約 -16.5dB
    pub volume: f32,
    #[serde(default = "default_fade_in")]
    pub fade_in_s: f32,
    #[serde(default = "default_fade_out")]
    pub fade_out_s: f32,
    /// 会話終了後に BGM だけを残す余韻の長さ (秒)。フェードアウトはこの中で行う
    #[serde(default = "default_ending_tail")]
    pub ending_tail_s: f32,
    /// 声の中心帯域 (2.5kHz 付近) で BGM を下げる量 (dB, 負の値)。
    /// 会話の聞き取りやすさを上げる。0 で無効
    #[serde(default = "default_voice_duck")]
    pub voice_duck_db: f32,
}

fn default_fade_in() -> f32 {
    2.0
}
fn default_fade_out() -> f32 {
    4.0
}
fn default_ending_tail() -> f32 {
    5.0
}
fn default_voice_duck() -> f32 {
    -4.0
}

impl Default for BgmOpts {
    fn default() -> Self {
        Self {
            volume: 0.15,
            fade_in_s: default_fade_in(),
            fade_out_s: default_fade_out(),
            ending_tail_s: default_ending_tail(),
            voice_duck_db: default_voice_duck(),
        }
    }
}

/// BGM ミックス用のフィルタ式を生成する。
///
/// - `-stream_loop -1` により BGM は入力段で無限ループしている前提
/// - amix の duration=first で本編 (余韻パディング込み) の長さに合わせて終了する
/// - normalize=0 にしないと amix が会話音声の音量を半分に下げてしまう
/// - voice_duck_db が負なら、声の中心帯域 (2.5kHz 付近・2オクターブ幅) の
///   BGM だけを下げて会話を聞き取りやすくする。声側は加工しない
pub fn build_bgm_filter(main_duration_ms: u64, opts: &BgmOpts) -> String {
    let duration_s = main_duration_ms as f64 / 1000.0;
    let fade_out_start = (duration_s - opts.fade_out_s as f64).max(0.0);
    format!(
        "[1:a]volume={volume}{duck},afade=t=in:st=0:d={fade_in},afade=t=out:st={fade_out_start:.3}:d={fade_out}[bgm];\
         [0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]",
        volume = opts.volume,
        duck = duck_filter(opts),
        fade_in = opts.fade_in_s,
        fade_out = opts.fade_out_s,
    )
}

fn duck_filter(opts: &BgmOpts) -> String {
    if opts.voice_duck_db < 0.0 {
        format!(
            ",equalizer=f=2500:width_type=o:w=2:g={:.1}",
            opts.voice_duck_db
        )
    } else {
        String::new()
    }
}

/// BGM 音量プレビュー用のフィルタ式。フェードは掛けず、
/// 音量と EQ のバランスだけを本番と同じ設定で確認する
pub fn build_bgm_preview_filter(opts: &BgmOpts) -> String {
    format!(
        "[1:a]volume={volume}{duck}[bgm];\
         [0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]",
        volume = opts.volume,
        duck = duck_filter(opts),
    )
}

/// 入力動画の一部分と BGM を現在の設定でミックスした試聴用の音声を生成する。
/// 音声だけの処理なので短時間で終わり、GUI の音量スライダー調整に使う
#[allow(clippy::too_many_arguments)]
pub fn render_bgm_preview(
    ffmpeg: &Ffmpeg,
    input: &Path,
    bgm: &Path,
    opts: &BgmOpts,
    start_ms: u64,
    duration_ms: u64,
    output: &Path,
    cancel: &CancelToken,
) -> Result<()> {
    if !bgm.exists() {
        return Err(PaeError::InputNotFound(bgm.to_path_buf()));
    }
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{:.3}", start_ms as f64 / 1000.0),
        "-t".into(),
        format!("{:.3}", duration_ms as f64 / 1000.0),
        "-i".into(),
        input.display().to_string(),
        "-stream_loop".into(),
        "-1".into(),
        "-i".into(),
        bgm.display().to_string(),
        "-filter_complex".into(),
        build_bgm_preview_filter(opts),
        "-map".into(),
        "[a]".into(),
        "-c:a".into(),
        "libmp3lame".into(),
        "-q:a".into(),
        "4".into(),
        output.display().to_string(),
    ];
    ffmpeg.run(&args, None, &mut |_| {}, cancel)?;
    Ok(())
}

/// 編集済み動画に BGM をループ・フェード付きでミックスする。映像は無劣化コピー
#[allow(clippy::too_many_arguments)]
pub fn mix_bgm(
    ffmpeg: &Ffmpeg,
    input: &Path,
    bgm: &Path,
    output: &Path,
    main_duration_ms: u64,
    has_video: bool,
    opts: &BgmOpts,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    if !bgm.exists() {
        return Err(PaeError::InputNotFound(bgm.to_path_buf()));
    }
    let filter = build_bgm_filter(main_duration_ms, opts);
    let mut args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-stream_loop".into(),
        "-1".into(),
        "-i".into(),
        bgm.display().to_string(),
        "-filter_complex".into(),
        filter,
    ];
    if has_video {
        args.extend(
            [
                "-map",
                "0:v",
                "-map",
                "[a]",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-ar",
                "48000",
                "-movflags",
                "+faststart",
            ]
            .map(String::from),
        );
    } else {
        args.extend(["-map", "[a]", "-c:a", "flac", "-ar", "48000"].map(String::from));
    }
    args.push(output.display().to_string());
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
    has_video: bool,
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
    let mut args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-af".into(),
        filter,
    ];
    if has_video {
        args.extend(
            [
                "-map",
                "0:v",
                "-map",
                "0:a",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                // loudnorm は内部で 192kHz にアップサンプルするため明示的に戻す
                "-ar",
                "48000",
                "-movflags",
                "+faststart",
            ]
            .map(String::from),
        );
    } else {
        args.extend(["-map", "0:a", "-c:a", "flac", "-ar", "48000"].map(String::from));
    }
    args.push(output.display().to_string());
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
        let script = build_cut_filter_script(&[(0, 2200), (5000, 6000)], 0, true);
        insta::assert_snapshot!(script, @r"
        [0:v]select='between(t,0.000,2.200)+between(t,5.000,6.000)',setpts=N/FRAME_RATE/TB[v];
        [0:a]aselect='between(t,0.000,2.200)+between(t,5.000,6.000)',asetpts=N/SR/TB,aresample=async=1[a]
        ");
    }

    #[test]
    fn cut_filter_script_with_ending_tail() {
        let script = build_cut_filter_script(&[(0, 2200)], 5000, true);
        insta::assert_snapshot!(script, @r"
        [0:v]select='between(t,0.000,2.200)',setpts=N/FRAME_RATE/TB,tpad=stop_duration=5.000:stop_mode=clone[v];
        [0:a]aselect='between(t,0.000,2.200)',asetpts=N/SR/TB,aresample=async=1,apad=pad_dur=5.000[a]
        ");
    }

    #[test]
    fn cut_filter_script_audio_only() {
        let script = build_cut_filter_script(&[(0, 2200), (5000, 6000)], 3000, false);
        insta::assert_snapshot!(script, @r"
        [0:a]aselect='between(t,0.000,2.200)+between(t,5.000,6.000)',asetpts=N/SR/TB,aresample=async=1,apad=pad_dur=3.000[a]
        ");
    }

    #[test]
    fn bgm_filter_snapshot() {
        let filter = build_bgm_filter(60_000, &BgmOpts::default());
        insta::assert_snapshot!(filter, @"[1:a]volume=0.15,equalizer=f=2500:width_type=o:w=2:g=-4.0,afade=t=in:st=0:d=2,afade=t=out:st=56.000:d=4[bgm];[0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]");
    }

    #[test]
    fn bgm_preview_filter_snapshot() {
        let filter = build_bgm_preview_filter(&BgmOpts::default());
        insta::assert_snapshot!(filter, @"[1:a]volume=0.15,equalizer=f=2500:width_type=o:w=2:g=-4.0[bgm];[0:a][bgm]amix=inputs=2:duration=first:normalize=0[a]");
    }

    #[test]
    fn bgm_filter_without_duck() {
        let opts = BgmOpts {
            voice_duck_db: 0.0,
            ..BgmOpts::default()
        };
        let filter = build_bgm_filter(60_000, &opts);
        assert!(!filter.contains("equalizer"));
    }

    /// 旧バージョンの config.toml (新フィールドなし) も読めること
    #[test]
    fn bgm_opts_deserializes_legacy_config() {
        let opts: BgmOpts =
            toml::from_str("volume = 0.1\nfade_in_s = 2.0\nfade_out_s = 4.0\n").unwrap();
        assert_eq!(opts.ending_tail_s, 5.0);
        assert_eq!(opts.voice_duck_db, -4.0);
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
