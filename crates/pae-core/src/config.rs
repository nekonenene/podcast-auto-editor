//! アプリ設定の保存と読み込み。macOS では ~/Library/Application Support 配下に置く

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PaeError, Result};
use crate::media::process::BgmOpts;

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
