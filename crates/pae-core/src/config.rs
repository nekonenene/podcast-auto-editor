//! アプリ設定の保存と読み込み。macOS では ~/Library/Application Support 配下に置く

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PaeError, Result};
use crate::media::process::BgmOpts;
use crate::output::TranscriptFormat;

/// どの成果物ファイルを出力するかの選択
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSelection {
    pub edited_mp4: bool,
    pub podcast_mp3: bool,
    pub timeline_json: bool,
    pub transcript_txt: bool,
    pub transcript_json: bool,
    pub transcript_srt: bool,
    pub transcript_md: bool,
}

impl Default for OutputSelection {
    fn default() -> Self {
        Self {
            edited_mp4: true,
            podcast_mp3: true,
            timeline_json: true,
            transcript_txt: true,
            transcript_json: true,
            transcript_srt: true,
            transcript_md: true,
        }
    }
}

impl OutputSelection {
    /// 選択されている文字起こしフォーマットの一覧
    pub fn transcript_formats(&self) -> Vec<TranscriptFormat> {
        let mut formats = Vec::new();
        if self.transcript_txt {
            formats.push(TranscriptFormat::Txt);
        }
        if self.transcript_json {
            formats.push(TranscriptFormat::Json);
        }
        if self.transcript_srt {
            formats.push(TranscriptFormat::Srt);
        }
        if self.transcript_md {
            formats.push(TranscriptFormat::Markdown);
        }
        formats
    }

    /// 文字起こしフォーマットの一覧から選択状態を作る (CLI の --formats 用)
    pub fn set_transcript_formats(&mut self, formats: &[TranscriptFormat]) {
        self.transcript_txt = formats.contains(&TranscriptFormat::Txt);
        self.transcript_json = formats.contains(&TranscriptFormat::Json);
        self.transcript_srt = formats.contains(&TranscriptFormat::Srt);
        self.transcript_md = formats.contains(&TranscriptFormat::Markdown);
    }

    pub fn any_selected(&self) -> bool {
        self.edited_mp4
            || self.podcast_mp3
            || self.timeline_json
            || !self.transcript_formats().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// デフォルトの BGM ファイル。一度指定すると次回以降も使われる
    pub default_bgm: Option<PathBuf>,
    pub bgm: BgmOpts,
    /// 無音短縮プリセット名 (natural / standard / aggressive)
    pub preset: String,
    /// デフォルトの出力先ディレクトリ
    pub output_dir: Option<PathBuf>,
    /// 文字起こしモデル名
    pub model: String,
    pub transcribe: bool,
    /// ラウドネスターゲット (LUFS)
    pub target_lufs: f64,
    /// ffmpeg / ffprobe のあるディレクトリ (未指定なら PATH から探す)
    pub ffmpeg_dir: Option<PathBuf>,
    /// 出力するファイルの選択
    pub outputs: OutputSelection,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_bgm: None,
            bgm: BgmOpts::default(),
            preset: "natural".into(),
            output_dir: None,
            model: "large-v3-turbo".into(),
            transcribe: true,
            target_lufs: -16.0,
            ffmpeg_dir: None,
            outputs: OutputSelection::default(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("net", "hatone", "PodcastAutoEditor")
            .ok_or_else(|| PaeError::Config("ホームディレクトリが見つかりません".into()))?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// 設定ファイルを読み込む。存在しなければデフォルト値を返す
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| PaeError::Config(format!("設定の解析に失敗: {e}")))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| PaeError::Config(format!("設定のシリアライズに失敗: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = AppConfig {
            default_bgm: Some(PathBuf::from("/music/bgm.mp3")),
            ..AppConfig::default()
        };
        config.bgm.volume = 0.2;
        config.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.default_bgm, Some(PathBuf::from("/music/bgm.mp3")));
        assert_eq!(loaded.bgm.volume, 0.2);
    }

    #[test]
    fn missing_file_returns_default() {
        let loaded = AppConfig::load_from(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(loaded.preset, "natural");
    }
}
