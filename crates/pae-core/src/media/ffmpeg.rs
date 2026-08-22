use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;

/// ffmpeg / ffprobe の実行を担う。
/// バイナリの場所は 設定での上書き > 環境変数 PAE_FFMPEG_DIR > PATH の順で解決する。
/// 将来 Tauri の sidecar に切り替えるときはこの解決部分だけを変更すればよい
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl Ffmpeg {
    pub fn locate(override_dir: Option<&Path>) -> Result<Self> {
        let dir = override_dir
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var_os("PAE_FFMPEG_DIR").map(PathBuf::from));

        let (ffmpeg, ffprobe) = match dir {
            Some(dir) => (dir.join("ffmpeg"), dir.join("ffprobe")),
            None => (PathBuf::from("ffmpeg"), PathBuf::from("ffprobe")),
        };

        let candidate = Self { ffmpeg, ffprobe };
        candidate.verify()?;
        Ok(candidate)
    }

    fn verify(&self) -> Result<()> {
        for bin in [&self.ffmpeg, &self.ffprobe] {
            Command::new(bin)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|_| PaeError::FfmpegNotFound(bin.display().to_string()))?;
        }
        Ok(())
    }

    /// ffprobe を実行して stdout を返す
    pub fn probe<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(&self.ffprobe)
            .args(args)
            .output()
            .map_err(|e| PaeError::FfmpegNotFound(format!("{}: {e}", self.ffprobe.display())))?;
        if !output.status.success() {
            return Err(PaeError::ExternalProcess {
                tool: "ffprobe".into(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// ffmpeg を実行する。
    ///
    /// - `expected_output_ms` を渡すと `-progress pipe:1` の out_time_us から
    ///   進捗率を計算して `on_progress` に通知する
    /// - キャンセルされたら子プロセスを kill して `PaeError::Cancelled` を返す
    /// - 戻り値は stderr 全文（ログ保存やエラー調査に使う）
    pub fn run(
        &self,
        args: &[String],
        expected_output_ms: Option<u64>,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<String> {
        cancel.check()?;

        let mut full_args: Vec<String> = vec!["-hide_banner".into(), "-y".into()];
        let want_progress = expected_output_ms.is_some();
        if want_progress {
            full_args.extend(["-progress".into(), "pipe:1".into(), "-nostats".into()]);
        }
        full_args.extend_from_slice(args);

        tracing::debug!(args = ?full_args, "ffmpeg 実行");

        let mut child = Command::new(&self.ffmpeg)
            .args(&full_args)
            .stdin(Stdio::null())
            .stdout(if want_progress {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| PaeError::FfmpegNotFound(format!("{}: {e}", self.ffmpeg.display())))?;

        // stderr は別スレッドで吸い出す。パイプが詰まって ffmpeg が
        // 停止するのを防ぐため、進捗パースと並行して読み続ける必要がある
        let mut stderr_pipe = child.stderr.take().expect("stderr は piped 指定済み");
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = stderr_pipe.read_to_string(&mut buf);
            buf
        });

        if let (Some(total_ms), Some(stdout)) = (expected_output_ms, child.stdout.take()) {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    break;
                }
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                if let Some(us) = line.strip_prefix("out_time_us=") {
                    if let Ok(us) = us.trim().parse::<u64>() {
                        let fraction = (us as f64 / 1000.0 / total_ms as f64).min(1.0);
                        on_progress(fraction as f32);
                    }
                }
            }
        } else {
            // 進捗パースなしの実行。キャンセルに反応できるようポーリングで待つ
            loop {
                match child.try_wait()? {
                    Some(_) => break,
                    None => {
                        if cancel.is_cancelled() {
                            let _ = child.kill();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }

        let status = child.wait()?;
        let stderr = stderr_thread.join().unwrap_or_default();

        cancel.check()?;
        if !status.success() {
            return Err(PaeError::ExternalProcess {
                tool: "ffmpeg".into(),
                code: status.code(),
                stderr: tail(&stderr, 4000),
            });
        }
        Ok(stderr)
    }
}

/// エラーメッセージ用に stderr の末尾だけを切り出す（先頭は定型ログが多いため）
fn tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let start = s.len() - max_bytes;
    // UTF-8 の文字境界に合わせる
    let start = (start..s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(start);
    format!("...(省略)...\n{}", &s[start..])
}
