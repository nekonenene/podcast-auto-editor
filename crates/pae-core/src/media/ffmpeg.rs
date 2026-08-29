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
        let specified = override_dir
            .map(|dir| (dir.to_path_buf(), "設定"))
            .or_else(|| {
                std::env::var_os("PAE_FFMPEG_DIR")
                    .map(|dir| (PathBuf::from(dir), "環境変数 PAE_FFMPEG_DIR"))
            });

        // 場所を明示されたときは、そこが駄目でも他を探さない。
        // 黙って別の ffmpeg を使うと、指定した意味がなくなってしまうため
        if let Some((dir, source)) = specified {
            let candidate = Self::in_dir(&dir);
            return match candidate.verify() {
                Ok(()) => Ok(candidate),
                Err(reason) => Err(PaeError::FfmpegNotFound(format!(
                    "{source} で指定された {} を使おうとしましたが、実行できませんでした。\n  {reason}",
                    dir.display()
                ))),
            };
        }

        // PATH → よくあるインストール先の順で探す。
        // macOS の GUI アプリは Finder から起動されるとシェルの PATH を継承しないため、
        // Homebrew などの標準的な場所へのフォールバックが必要
        let mut candidates = vec![(
            "PATH".to_string(),
            Self {
                ffmpeg: PathBuf::from("ffmpeg"),
                ffprobe: PathBuf::from("ffprobe"),
            },
        )];
        let mut fallback_dirs: Vec<PathBuf> = Vec::new();
        if cfg!(target_os = "macos") {
            fallback_dirs.push(PathBuf::from("/opt/homebrew/bin"));
            fallback_dirs.push(PathBuf::from("/usr/local/bin"));
        }
        if cfg!(windows) {
            // winget はシンボリックリンクを Links に集約する
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                fallback_dirs.push(PathBuf::from(local).join(r"Microsoft\WinGet\Links"));
            }
            fallback_dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
            fallback_dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
            fallback_dirs.push(PathBuf::from(r"C:\Program Files\ffmpeg\bin"));
        }
        for dir in fallback_dirs {
            candidates.push((dir.display().to_string(), Self::in_dir(&dir)));
        }

        // 探した場所をすべて記録しておく。1か所だけ報告すると、
        // 「最後に試した場所」を「探しに行った唯一の場所」と読み違えてしまうため
        let mut reasons = Vec::new();
        for (label, candidate) in candidates {
            match candidate.verify() {
                Ok(()) => return Ok(candidate),
                Err(reason) => reasons.push(format!("  - {label}: {reason}")),
            }
        }
        let searched = reasons.join("\n");
        Err(PaeError::FfmpegNotFound(format!(
            "次の場所を探しましたが、どれも実行できませんでした。\n{searched}\n\
             ffmpeg のあるフォルダは環境変数 PAE_FFMPEG_DIR で指定できます"
        )))
    }

    /// 指定したディレクトリ直下の ffmpeg / ffprobe を指す。
    /// 拡張子は付けない。Windows では起動時に .exe が自動で補われる
    fn in_dir(dir: &Path) -> Self {
        Self {
            ffmpeg: dir.join("ffmpeg"),
            ffprobe: dir.join("ffprobe"),
        }
    }

    /// 実際に `-version` を実行して、使える組み合わせかどうかを確かめる。
    /// 駄目だった理由は、そのままユーザーへ見せられる文にして返す
    fn verify(&self) -> std::result::Result<(), String> {
        for bin in [&self.ffmpeg, &self.ffprobe] {
            match no_window_command(bin)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    return Err(format!(
                        "{} は起動できましたが、終了コード {} で失敗しました",
                        file_name(bin),
                        status
                            .code()
                            .map_or_else(|| "不明".to_string(), |c| c.to_string())
                    ));
                }
                Err(e) => return Err(format!("{}: {e}", file_name(bin))),
            }
        }
        Ok(())
    }

    /// ffprobe を実行して stdout を返す
    pub fn probe<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = no_window_command(&self.ffprobe)
            .args(args)
            .output()
            .map_err(|e| {
                PaeError::FfmpegNotFound(format!(
                    "{} の起動に失敗しました: {e}",
                    self.ffprobe.display()
                ))
            })?;
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

        let mut child = no_window_command(&self.ffmpeg)
            .args(&full_args)
            .stdin(Stdio::null())
            .stdout(if want_progress {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                PaeError::FfmpegNotFound(format!(
                    "{} の起動に失敗しました: {e}",
                    self.ffmpeg.display()
                ))
            })?;

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

/// エラー文では場所を別に示すため、ここではファイル名だけを取り出す
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// 外部プロセス起動用の Command を作る。
/// Windows では CREATE_NO_WINDOW を付けないと、GUI アプリから ffmpeg のような
/// コンソールアプリを起動するたびに黒いウィンドウが一瞬表示されてしまう
fn no_window_command(bin: &Path) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 場所を明示されたときは、そこだけを報告する
    #[test]
    fn locate_reports_only_the_specified_dir() {
        let dir = PathBuf::from("/no/such/dir");
        let message = Ffmpeg::locate(Some(&dir)).unwrap_err().to_string();
        assert!(message.contains("設定"), "{message}");
        assert!(message.contains("no/such/dir"), "{message}");
        assert!(!message.contains("PATH"), "{message}");
    }
}
