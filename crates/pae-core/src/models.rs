//! 推論モデルのダウンロードとキャッシュ管理。
//! 文字起こしと話者分離で共通して使う。モデルの取得だけがネットワークを使い、
//! 推論そのものは完全ローカルでおこなう

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;

/// 利用可能なモデルの定義
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// CLI や設定で指定する名前
    pub name: &'static str,
    pub file_name: &'static str,
    pub url: &'static str,
    /// おおよそのサイズ (MB)。ダウンロード前の表示用
    pub approx_size_mb: u64,
    pub description: &'static str,
}

/// モデルの保存先ディレクトリを管理する
pub struct ModelManager {
    models_dir: PathBuf,
}

impl ModelManager {
    /// OS の慣習に従った場所を使う。
    /// macOS: ~/Library/Application Support、Windows: %LOCALAPPDATA%。
    /// モデルは 500MB を超えるため、Windows ではログオン時に同期される
    /// Roaming (data_dir) を避けて Local に置く
    pub fn new() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("net", "hatone", "PodcastAutoEditor")
            .ok_or_else(|| PaeError::Config("ホームディレクトリが見つかりません".into()))?;
        let models_dir = dirs.data_local_dir().join("models");
        Ok(Self { models_dir })
    }

    pub fn with_dir(models_dir: PathBuf) -> Self {
        Self { models_dir }
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn model_path(&self, spec: &ModelSpec) -> PathBuf {
        self.models_dir.join(spec.file_name)
    }

    pub fn is_downloaded(&self, spec: &ModelSpec) -> bool {
        self.model_path(spec).exists()
    }

    /// モデルをダウンロードする。既に存在すればそのパスを返す。
    /// 一時ファイルへ書き込み、完了後にリネームするため中断しても壊れたモデルは残らない
    pub fn ensure_model(
        &self,
        spec: &ModelSpec,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<PathBuf> {
        let path = self.model_path(spec);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.models_dir)?;

        tracing::info!(
            model = spec.name,
            url = spec.url,
            "モデルをダウンロードします"
        );

        let response = ureq::get(spec.url)
            .call()
            .map_err(|e| PaeError::ModelDownload(format!("{}: {e}", spec.url)))?;

        let total_bytes: Option<u64> = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());

        let tmp_path = path.with_extension("download");
        let mut file = std::fs::File::create(&tmp_path)?;
        let mut reader = response.into_body().into_reader();

        let mut buf = [0u8; 64 * 1024];
        let mut written: u64 = 0;
        loop {
            if cancel.is_cancelled() {
                drop(file);
                let _ = std::fs::remove_file(&tmp_path);
                return Err(PaeError::Cancelled);
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| PaeError::ModelDownload(format!("読み込みエラー: {e}")))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            written += n as u64;
            if let Some(total) = total_bytes {
                on_progress(written as f32 / total as f32);
            }
        }
        file.flush()?;
        drop(file);

        if let Some(total) = total_bytes {
            if written != total {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(PaeError::ModelDownload(format!(
                    "ダウンロードが不完全です ({written}/{total} bytes)"
                )));
            }
        }

        std::fs::rename(&tmp_path, &path)?;
        tracing::info!(path = %path.display(), "モデルのダウンロードが完了しました");
        Ok(path)
    }
}
