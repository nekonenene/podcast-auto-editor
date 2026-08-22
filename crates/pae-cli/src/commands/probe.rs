use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::media::ffmpeg::Ffmpeg;

#[derive(Args)]
pub struct ProbeArgs {
    /// 入力ファイル
    pub input: PathBuf,
}

pub fn execute(args: ProbeArgs) -> anyhow::Result<()> {
    let config = AppConfig::load()?;
    let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref())?;
    let info = pae_core::media::probe::probe(&ffmpeg, &args.input)?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}
