use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::output::TranscriptFormat;
use pae_core::pipeline::{run_job, JobSpec};
use pae_core::types::Preset;

use crate::progress::CliProgress;

/// run / render で共通の出力オプション
#[derive(Args)]
pub struct CommonOpts {
    /// BGM ファイル。指定すると設定に保存され次回以降のデフォルトになる
    #[arg(long)]
    pub bgm: Option<PathBuf>,

    /// BGM を付けない (設定済みのデフォルト BGM も無視する)
    #[arg(long)]
    pub no_bgm: bool,

    /// BGM の音量 (会話に対する倍率, 例: 0.15)
    #[arg(long)]
    pub bgm_volume: Option<f32>,

    /// BGM のフェードイン時間 (秒)
    #[arg(long)]
    pub fade_in: Option<f32>,

    /// BGM のフェードアウト時間 (秒)
    #[arg(long)]
    pub fade_out: Option<f32>,

    /// 会話終了後に BGM だけを残す余韻の長さ (秒, 0 で無効)
    #[arg(long)]
    pub ending_tail: Option<f32>,

    /// 声の帯域で BGM を下げる量 (dB, 負の値。0 で無効)
    #[arg(long)]
    pub bgm_duck: Option<f32>,

    /// Podcast MP3 のビットレート (kbps)。0 で VBR 高音質
    #[arg(long)]
    pub mp3_bitrate: Option<u32>,

    /// 出力範囲の開始位置 (秒)。収録冒頭の無駄話をカットする
    #[arg(long)]
    pub trim_start: Option<f64>,

    /// 出力範囲の終了位置 (秒)。収録末尾の無駄話をカットする
    #[arg(long)]
    pub trim_end: Option<f64>,

    /// ラウドネスターゲット (LUFS, 例: -16)
    #[arg(long)]
    pub lufs: Option<f64>,

    /// 文字起こしモデル名
    #[arg(long)]
    pub model: Option<String>,

    /// 文字起こしをスキップする
    #[arg(long)]
    pub skip_transcribe: bool,

    /// 文字起こしの出力フォーマット (カンマ区切り: txt,json,srt,md)
    #[arg(long)]
    pub formats: Option<String>,

    /// 文字起こしへ話者ラベルを付ける
    #[arg(long)]
    pub diarize: bool,

    /// 収録に参加している人数。話者分離はこの数へ分ける
    #[arg(long)]
    pub speakers: Option<u32>,
}

#[derive(Args)]
pub struct RunArgs {
    /// 入力動画ファイル
    pub input: PathBuf,

    /// 出力先ディレクトリ
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// 無音短縮プリセット (natural / standard / aggressive)
    #[arg(long)]
    pub preset: Option<String>,

    #[command(flatten)]
    pub common: CommonOpts,
}

/// CLI 引数 > 設定ファイル > デフォルト値 の優先順で JobSpec を組み立てる
pub fn build_spec(
    input: PathBuf,
    output: Option<PathBuf>,
    preset: Option<&str>,
    opts: &CommonOpts,
    config: &AppConfig,
) -> anyhow::Result<JobSpec> {
    let output_dir = output
        .or_else(|| config.output_dir.clone())
        .unwrap_or_else(|| PathBuf::from("output"));

    let mut spec = JobSpec::from_config(input, output_dir, config);

    if let Some(name) = preset {
        spec.preset =
            Preset::by_name(name).ok_or_else(|| anyhow::anyhow!("未知のプリセットです: {name}"))?;
    }
    if opts.no_bgm {
        spec.bgm = None;
    } else if let Some(bgm) = &opts.bgm {
        spec.bgm = Some(bgm.clone());
    }
    if let Some(v) = opts.bgm_volume {
        spec.bgm_opts.volume = v;
    }
    if let Some(v) = opts.fade_in {
        spec.bgm_opts.fade_in_s = v;
    }
    if let Some(v) = opts.fade_out {
        spec.bgm_opts.fade_out_s = v;
    }
    if let Some(v) = opts.ending_tail {
        spec.bgm_opts.ending_tail_s = v;
    }
    if let Some(v) = opts.bgm_duck {
        spec.bgm_opts.voice_duck_db = v;
    }
    if let Some(v) = opts.mp3_bitrate {
        spec.mp3_bitrate_kbps = v;
    }
    // トリム範囲。終了だけの指定は「そこまで」ではなく明示指定を要求するほうが
    // 事故が少ないため、両方揃っていなくても片側は 0 / 入力末尾で補完する
    if opts.trim_start.is_some() || opts.trim_end.is_some() {
        let start_ms = (opts.trim_start.unwrap_or(0.0).max(0.0) * 1000.0) as u64;
        let end_ms = opts
            .trim_end
            .map(|s| (s * 1000.0) as u64)
            .unwrap_or(u64::MAX);
        spec.trim_range_ms = Some((start_ms, end_ms));
    }
    if let Some(v) = opts.lufs {
        spec.target_lufs = v;
    }
    if let Some(model) = &opts.model {
        spec.model = model.clone();
    }
    if opts.skip_transcribe {
        spec.transcribe = false;
    }
    if opts.diarize {
        spec.diarize = true;
    }
    if let Some(count) = opts.speakers {
        if count == 0 {
            anyhow::bail!("話者の人数は1以上を指定してください");
        }
        spec.speaker_count = count;
    }
    if let Some(formats) = &opts.formats {
        let formats = formats
            .split(',')
            .map(|s| {
                TranscriptFormat::parse(s.trim())
                    .ok_or_else(|| anyhow::anyhow!("未知のフォーマットです: {s}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        spec.outputs.set_transcript_formats(&formats);
    }
    Ok(spec)
}

/// 一度指定した BGM を次回以降のデフォルトとして設定へ保存する
pub fn save_bgm_defaults(config: &mut AppConfig, opts: &CommonOpts) -> anyhow::Result<()> {
    let mut changed = false;
    if let Some(bgm) = &opts.bgm {
        config.default_bgm = Some(bgm.clone());
        changed = true;
    }
    if let Some(v) = opts.bgm_volume {
        config.bgm.volume = v;
        changed = true;
    }
    if changed {
        config.save()?;
    }
    Ok(())
}

pub fn execute(args: RunArgs) -> anyhow::Result<()> {
    let mut config = AppConfig::load()?;
    let spec = build_spec(
        args.input,
        args.output,
        args.preset.as_deref(),
        &args.common,
        &config,
    )?;

    let cancel = super::install_cancel_handler()?;
    let progress = CliProgress::new();
    let report = run_job(&spec, &progress, &cancel)?;
    progress.finish();

    save_bgm_defaults(&mut config, &args.common)?;
    super::print_report(&report);
    Ok(())
}
