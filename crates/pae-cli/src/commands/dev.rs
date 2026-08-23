//! 開発・技術検証用の低レベルコマンド群。
//! パイプラインの各段階を単体で実行して動作確認やパラメータ調整に使う

use std::path::PathBuf;

use clap::{Args, Subcommand};
use pae_core::config::AppConfig;
use pae_core::media::extract::{extract_analysis_wav, read_wav_samples};
use pae_core::media::ffmpeg::Ffmpeg;
use pae_core::media::probe::probe;
use pae_core::media::process::{
    apply_loudnorm, cut_video, measure_loudness, mix_bgm, BgmOpts, LoudnormTarget, VideoEncodeOpts,
};
use pae_core::progress::CancelToken;
use pae_core::timeline::{timeline_to_keep_ranges, validate_timeline};
use pae_core::types::{EditTimeline, VadParams};
use pae_core::vad::{SileroVad, Vad};

#[derive(Args)]
pub struct DevArgs {
    #[command(subcommand)]
    command: DevCommand,
}

#[derive(Subcommand)]
enum DevCommand {
    /// timeline.json に従って動画をカットする (BGM・loudnorm なし)
    Cut {
        input: PathBuf,
        #[arg(short, long)]
        timeline: PathBuf,
        #[arg(short, long, default_value = "cut.mp4")]
        output: PathBuf,
    },
    /// 出力動画の映像と音声の長さを検証する
    VerifySync {
        input: PathBuf,
        /// 期待値と比較する timeline.json
        #[arg(short, long)]
        timeline: Option<PathBuf>,
    },
    /// 解析用の 16kHz mono WAV を抽出する
    ExtractAudio {
        input: PathBuf,
        #[arg(short, long, default_value = "analysis.wav")]
        output: PathBuf,
    },
    /// VAD を実行して発話区間を JSON で表示する
    Vad {
        /// 入力 (動画・音声どちらでも可)
        input: PathBuf,
        #[arg(long, default_value_t = 0.4)]
        threshold: f32,
        #[arg(long, default_value_t = 100)]
        min_speech: u64,
        #[arg(long, default_value_t = 250)]
        min_silence: u64,
    },
    /// BGM をループ・フェード付きでミックスする
    MixBgm {
        input: PathBuf,
        #[arg(long)]
        bgm: PathBuf,
        #[arg(short, long, default_value = "mixed.mp4")]
        output: PathBuf,
        #[arg(long, default_value_t = 0.15)]
        volume: f32,
        #[arg(long, default_value_t = 2.0)]
        fade_in: f32,
        #[arg(long, default_value_t = 4.0)]
        fade_out: f32,
    },
    /// loudnorm 2パスで音量を調整する
    Loudnorm {
        input: PathBuf,
        #[arg(short, long, default_value = "normalized.mp4")]
        output: PathBuf,
        #[arg(long, default_value_t = -16.0)]
        lufs: f64,
    },
}

pub fn execute(args: DevArgs) -> anyhow::Result<()> {
    let config = AppConfig::load()?;
    let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref())?;
    let cancel = CancelToken::new();
    let mut print_progress = |fraction: f32| {
        eprint!("\r{:>5.1}%", fraction * 100.0);
    };

    match args.command {
        DevCommand::Cut {
            input,
            timeline,
            output,
        } => {
            let text = std::fs::read_to_string(&timeline)?;
            let timeline: EditTimeline = serde_json::from_str(&text)?;
            validate_timeline(&timeline)?;
            let ranges = timeline_to_keep_ranges(&timeline);
            let info = probe(&ffmpeg, &input)?;
            println!("keep ranges: {} 個", ranges.len());
            cut_video(
                &ffmpeg,
                &input,
                &ranges,
                &output,
                timeline.stats.output_duration_ms,
                0,
                &VideoEncodeOpts::auto(info.height),
                &mut print_progress,
                &cancel,
            )?;
            eprintln!();
            println!("出力: {}", output.display());
        }
        DevCommand::VerifySync { input, timeline } => {
            verify_sync(&ffmpeg, &input, timeline.as_deref())?;
        }
        DevCommand::ExtractAudio { input, output } => {
            let info = probe(&ffmpeg, &input)?;
            extract_analysis_wav(
                &ffmpeg,
                &input,
                &output,
                info.duration_ms,
                &mut print_progress,
                &cancel,
            )?;
            eprintln!();
            println!("出力: {}", output.display());
        }
        DevCommand::Vad {
            input,
            threshold,
            min_speech,
            min_silence,
        } => {
            let params = VadParams {
                threshold,
                min_speech_ms: min_speech,
                min_silence_ms: min_silence,
                ..VadParams::default()
            };
            let temp_dir = tempfile::Builder::new().prefix("pae-").tempdir()?;
            let wav_path = if input.extension().is_some_and(|e| e == "wav") {
                input.clone()
            } else {
                let info = probe(&ffmpeg, &input)?;
                let wav = temp_dir.path().join("analysis.wav");
                extract_analysis_wav(
                    &ffmpeg,
                    &input,
                    &wav,
                    info.duration_ms,
                    &mut |_| {},
                    &cancel,
                )?;
                wav
            };
            let (samples, sample_rate) = read_wav_samples(&wav_path)?;
            let segments =
                SileroVad.detect(&samples, sample_rate, &params, &mut print_progress, &cancel)?;
            eprintln!();
            println!("{}", serde_json::to_string_pretty(&segments)?);
            eprintln!("発話区間: {} 個", segments.len());
        }
        DevCommand::MixBgm {
            input,
            bgm,
            output,
            volume,
            fade_in,
            fade_out,
        } => {
            let info = probe(&ffmpeg, &input)?;
            let opts = BgmOpts {
                volume,
                fade_in_s: fade_in,
                fade_out_s: fade_out,
                ..BgmOpts::default()
            };
            mix_bgm(
                &ffmpeg,
                &input,
                &bgm,
                &output,
                info.duration_ms,
                &opts,
                &mut print_progress,
                &cancel,
            )?;
            eprintln!();
            println!("出力: {}", output.display());
        }
        DevCommand::Loudnorm {
            input,
            output,
            lufs,
        } => {
            let info = probe(&ffmpeg, &input)?;
            let target = LoudnormTarget {
                i: lufs,
                ..LoudnormTarget::default()
            };
            let measured = measure_loudness(&ffmpeg, &input, &target, &cancel)?;
            println!(
                "測定結果: I={} TP={} LRA={}",
                measured.input_i, measured.input_tp, measured.input_lra
            );
            apply_loudnorm(
                &ffmpeg,
                &input,
                &output,
                &target,
                &measured,
                info.duration_ms,
                &mut print_progress,
                &cancel,
            )?;
            eprintln!();
            println!("出力: {}", output.display());
        }
    }
    Ok(())
}

/// 映像と音声の長さのずれを検証する。timeline があれば期待値とも比較する
fn verify_sync(
    ffmpeg: &Ffmpeg,
    input: &std::path::Path,
    timeline: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let json = ffmpeg.probe([
        "-v".as_ref(),
        "error".as_ref(),
        "-print_format".as_ref(),
        "json".as_ref(),
        "-show_streams".as_ref(),
        input.as_os_str(),
    ])?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("streams が取得できません"))?;

    let mut video_ms: Option<f64> = None;
    let mut audio_ms: Option<f64> = None;
    for stream in streams {
        let duration_ms = stream["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|s| s * 1000.0);
        match stream["codec_type"].as_str() {
            Some("video") => video_ms = duration_ms,
            Some("audio") => audio_ms = duration_ms,
            _ => {}
        }
    }

    let video_ms = video_ms.ok_or_else(|| anyhow::anyhow!("映像ストリームがありません"))?;
    let audio_ms = audio_ms.ok_or_else(|| anyhow::anyhow!("音声ストリームがありません"))?;
    let diff = (video_ms - audio_ms).abs();

    println!("映像: {video_ms:.0}ms");
    println!("音声: {audio_ms:.0}ms");
    println!(
        "差:   {diff:.0}ms {}",
        if diff < 50.0 {
            "OK"
        } else {
            "NG (50ms 以上)"
        }
    );

    if let Some(timeline_path) = timeline {
        let text = std::fs::read_to_string(timeline_path)?;
        let timeline: EditTimeline = serde_json::from_str(&text)?;
        let expected = timeline.stats.output_duration_ms as f64;
        let diff_expected = (video_ms - expected).abs();
        println!("期待値: {expected:.0}ms (timeline)");
        println!(
            "期待値との差: {diff_expected:.0}ms {}",
            // 1フレーム (30fpsで33ms) 程度の誤差は許容
            if diff_expected < 50.0 {
                "OK"
            } else {
                "NG (50ms 以上)"
            }
        );
    }
    Ok(())
}
