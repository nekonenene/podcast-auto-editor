//! タイムラインに基づく動画の再構成、BGM ミックス、ラウドネス調整。
//! ffmpeg のフィルタ式を生成する部分は純粋関数にしてテスト可能にしている

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;

use super::ffmpeg::Ffmpeg;
use super::probe::probe_stream_durations;

/// カットの継ぎ目に掛けるフェードアウトの長さ。
/// 部屋鳴りの波形が不連続につながるとプツッというノイズになるため、
/// 継ぎ目の手前で音量を絞ってから切る。
/// 継ぎ目は必ず無音区間の中にできるので、この長さなら会話は削られない。
///
/// 継ぎ目の後ろにフェードインは掛けない。
/// 聴き比べると、話し始めが鈍って聞き取りにくくなるため
const CUT_FADE_MS: u64 = 20;

/// フェードを掛けるあいだ、音声フレームを何サンプル単位に組み直すか。
/// aselect も afade もフレーム単位でしか働かないため、
/// 入力そのままのフレーム (AAC なら約 21ms) ではフェードが継ぎ目に届かない。
/// 48kHz で約 5.3ms まで細かくすると、フェードが狙った位置へ収まる
const CUT_FADE_FRAME_SAMPLES: u32 = 256;

/// フェードを操作する afade へ付ける名前。asendcmd から指名するために要る
const FADE_FILTER_NAME: &str = "afade@paecut";

/// keep_ranges (ミリ秒) から select / aselect の判定式を作る
fn build_select_expr(keep_ranges_ms: &[(u64, u64)]) -> String {
    keep_ranges_ms
        .iter()
        .map(|(start, end)| {
            format!(
                "between(t,{:.3},{:.3})",
                *start as f64 / 1000.0,
                *end as f64 / 1000.0
            )
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 映像チェーンのフィルタスクリプトを生成する。
///
/// カット後に setpts でタイムスタンプをゼロから振り直すのが音ズレ防止の要点。
/// `tail_ms` を指定すると末尾に最終フレームの静止を足す。
/// 会話が終わったあと BGM だけが数秒残るエンディングを作るために使う
pub fn build_cut_video_filter(keep_ranges_ms: &[(u64, u64)], tail_ms: u64) -> String {
    let expr = build_select_expr(keep_ranges_ms);
    let tail = if tail_ms > 0 {
        format!(
            ",tpad=stop_duration={:.3}:stop_mode=clone",
            tail_ms as f64 / 1000.0
        )
    } else {
        String::new()
    };
    format!("[0:v]select='{expr}',setpts=N/FRAME_RATE/TB{tail}[v]\n")
}

/// 継ぎ目のフェードアウトを afade へ指示する asendcmd のコマンド列を作る。
///
/// 残す区間の先頭ごとに「この区間はいつ絞り始めるか」を予約しておく。
/// フェードアウトが終わったあと afade の音量は 0 のままになるが、
/// 次の区間の先頭で次の予約を入れ直すため、そこで音量はもとに戻る。
/// フェードインを挟まずに戻せるのはこの性質のおかげである。
///
/// afade は1回分のフェードしか持てないため、こうして1個を使い回す。
/// 継ぎ目の数だけ afade を並べる手もあるが、フィルタが増えるほど1フレームあたりの
/// 処理が重くなり、継ぎ目が数百ある収録では現実的な速さでなくなる
pub fn build_cut_fade_commands(keep_ranges_ms: &[(u64, u64)]) -> String {
    if keep_ranges_ms.len() < 2 {
        return String::new();
    }
    let mut commands = String::new();
    for (i, (start, end)) in keep_ranges_ms.iter().enumerate() {
        // 区間がフェードより短ければ、その区間の長さに収める
        let fade = CUT_FADE_MS.min(end.saturating_sub(*start));
        let fade_at = if i + 1 < keep_ranges_ms.len() {
            end - fade
        } else {
            // 最後の区間の終わりは継ぎ目ではない。
            // 区間の外を指しておけば、予約は入るが実際には掛からない
            end + fade
        };
        commands.push_str(&fade_command(*start, fade_at, fade));
    }
    commands
}

/// asendcmd のコマンド1行。
/// `at_ms` の時点で、`fade_at_ms` から始まるフェードアウトを afade へ予約する
fn fade_command(at_ms: u64, fade_at_ms: u64, fade_ms: u64) -> String {
    let at = at_ms as f64 / 1000.0;
    let fade_at = fade_at_ms as f64 / 1000.0;
    let d = fade_ms as f64 / 1000.0;
    let f = FADE_FILTER_NAME;
    format!("{at:.3} {f} type out, {f} start_time {fade_at:.3}, {f} duration {d:.3};\n")
}

/// ffmpeg のフィルタ記述へ埋め込めるようにパスを整える。
/// Windows のドライブレターのコロンは、そのままだと引数の区切りと解釈されてしまう
fn escape_filter_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "/")
        .replace(':', "\\:")
}

/// 音声チェーンのフィルタスクリプトを生成する。
///
/// asetpts でタイムスタンプをゼロから振り直し、
/// aresample=async=1 で微小なギャップを吸収する。
/// `tail_ms` を指定すると末尾に無音のパディングを足す。
/// `fade_commands` にコマンドファイルのパスを渡すと、カットの継ぎ目にフェードを掛ける
pub fn build_cut_audio_filter(
    keep_ranges_ms: &[(u64, u64)],
    tail_ms: u64,
    fade_commands: Option<&Path>,
) -> String {
    let expr = build_select_expr(keep_ranges_ms);
    let tail = if tail_ms > 0 {
        format!(",apad=pad_dur={:.3}", tail_ms as f64 / 1000.0)
    } else {
        String::new()
    };
    // afade へ渡す値は、最初のコマンドが届くまでの初期値でしかない。
    // 残す区間の先頭ごとに asendcmd が設定を入れ直す
    let fade = match fade_commands {
        Some(path) => format!(
            "asetnsamples=n={}:p=0,asendcmd=filename='{}',{}=t=in:st=0:d={:.3},",
            CUT_FADE_FRAME_SAMPLES,
            escape_filter_path(path),
            FADE_FILTER_NAME,
            CUT_FADE_MS as f64 / 1000.0
        ),
        None => String::new(),
    };
    format!("[0:a]{fade}aselect='{expr}',asetpts=N/SR/TB,aresample=async=1{tail}[a]\n")
}

/// 継ぎ目のフェード用コマンドを一時ファイルへ書き出す。
/// 継ぎ目がなければ書き出さず None を返し、フェードの仕組みごと省く
fn write_fade_commands(dir: &Path, keep_ranges_ms: &[(u64, u64)]) -> Result<Option<PathBuf>> {
    let commands = build_cut_fade_commands(keep_ranges_ms);
    if commands.is_empty() {
        return Ok(None);
    }
    let path = dir.join("fade-commands.txt");
    std::fs::write(&path, commands)?;
    Ok(Some(path))
}

/// 出力動画のエンコード設定
#[derive(Debug, Clone)]
pub struct VideoEncodeOpts {
    pub encoder: String,
    pub bitrate: String,
}

impl VideoEncodeOpts {
    /// プラットフォームと解像度から適切なエンコーダとビットレートを選ぶ。
    /// ハードウェアエンコーダは x264 より同ビットレートでの品質が落ちるため、少し多めに盛る
    pub fn auto(height: Option<u32>) -> Self {
        let encoder = if cfg!(target_os = "macos") {
            "h264_videotoolbox"
        } else if cfg!(target_os = "windows") {
            // LGPL ビルドの ffmpeg には libx264 が入らないため、
            // Windows 標準の Media Foundation エンコーダを使う
            "h264_mf"
        } else {
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

/// フィルタスクリプトをファイルへ渡して ffmpeg を1回実行する。
///
/// "-/opt file" は値をファイルから読む ffmpeg の構文。
/// フィルタ式は長尺動画で数百節になり引数の長さ制限を超えうるためファイル渡しにする
/// (旧 -filter_complex_script は ffmpeg 9 で廃止)
#[allow(clippy::too_many_arguments)]
fn run_with_filter_script(
    ffmpeg: &Ffmpeg,
    input: &Path,
    script: &str,
    output_args: &[String],
    output: &Path,
    expected_output_ms: Option<u64>,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    let script_file = tempfile::Builder::new()
        .prefix("pae-filter-")
        .suffix(".txt")
        .tempfile()?;
    std::fs::write(script_file.path(), script)?;

    let mut args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-/filter_complex".into(),
        script_file.path().display().to_string(),
    ];
    args.extend_from_slice(output_args);
    args.push(output.display().to_string());
    ffmpeg.run(&args, expected_output_ms, on_progress, cancel)
}

/// タイムラインの keep_ranges に従って入力をカット・再結合する。
///
/// 映像ありの入力では、映像と音声を**別々の ffmpeg プロセス**で処理してから多重化する。
/// ひとつの filter_complex に映像出力と音声出力を同居させると、
/// ffmpeg 9 は長尺の入力で音声ブランチを途中で打ち切ることがあるため
/// (警告も出さず終了コードも 0 になる。詳細は docs/tech-notes.md)。
/// 2つのプロセスは同時に走らせる。映像のエンコードが律速なので、
/// 音声の処理時間はその裏に隠れて全体はむしろ速くなる。
///
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

    let log = if has_video {
        cut_video_and_audio(
            ffmpeg,
            input,
            keep_ranges_ms,
            output,
            output_duration_ms,
            tail_ms,
            encode,
            on_progress,
            cancel,
        )?
    } else {
        // 音声のみ: 中間ファイルは可逆の FLAC にして多段エンコードの劣化を避ける。
        // フェード用のコマンドファイルは ffmpeg が読み終えるまで消えないよう、
        // 一時ディレクトリごとこのスコープで抱えておく
        let fade_dir = tempfile::Builder::new().prefix("pae-fade-").tempdir()?;
        let fade_commands = write_fade_commands(fade_dir.path(), keep_ranges_ms)?;
        run_with_filter_script(
            ffmpeg,
            input,
            &build_cut_audio_filter(keep_ranges_ms, tail_ms, fade_commands.as_deref()),
            &["-map", "[a]", "-c:a", "flac", "-ar", "48000"].map(String::from),
            output,
            Some(output_duration_ms),
            on_progress,
            cancel,
        )?
    };

    verify_output_duration(ffmpeg, output, output_duration_ms, "カット")?;
    Ok(log)
}

/// 映像パスと音声パスを並行して走らせ、最後に多重化する
#[allow(clippy::too_many_arguments)]
fn cut_video_and_audio(
    ffmpeg: &Ffmpeg,
    input: &Path,
    keep_ranges_ms: &[(u64, u64)],
    output: &Path,
    output_duration_ms: u64,
    tail_ms: u64,
    encode: &VideoEncodeOpts,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    let parts_dir = tempfile::Builder::new().prefix("pae-cut-").tempdir()?;
    let video_part = parts_dir.path().join("video.mp4");
    let audio_part = parts_dir.path().join("audio.m4a");
    let fade_commands = write_fade_commands(parts_dir.path(), keep_ranges_ms)?;

    // 音声側は独立したキャンセル用トークンで動かす。
    // 映像側が失敗したときに、無駄になった音声処理をすぐ止めるため
    let audio_cancel = CancelToken::new();
    let audio_thread = {
        let ffmpeg = ffmpeg.clone();
        let cancel = audio_cancel.clone();
        let input = input.to_path_buf();
        let output = audio_part.clone();
        let script = build_cut_audio_filter(keep_ranges_ms, tail_ms, fade_commands.as_deref());
        std::thread::spawn(move || {
            run_with_filter_script(
                &ffmpeg,
                &input,
                &script,
                &["-map", "[a]", "-c:a", "aac", "-b:a", "192k", "-ar", "48000"].map(String::from),
                &output,
                None,
                &mut |_| {},
                &cancel,
            )
        })
    };

    // 進捗は映像パスのものを使う。律速なので体感と最も合う
    let video_result = run_with_filter_script(
        ffmpeg,
        input,
        &build_cut_video_filter(keep_ranges_ms, tail_ms),
        &[
            "-map",
            "[v]",
            "-c:v",
            &encode.encoder,
            "-b:v",
            &encode.bitrate,
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "cfr",
            "-an",
            // 中間ファイルなので faststart は付けない。
            // moov を先頭へ移すためだけに数GBを書き直すことになり、多重化でどうせ付け直す
        ]
        .map(String::from),
        &video_part,
        Some(output_duration_ms),
        &mut |fraction| on_progress(fraction * 0.95),
        cancel,
    );
    if video_result.is_err() {
        audio_cancel.cancel();
    }

    // 映像側が失敗しても必ずスレッドを回収する。
    // 先に ? で戻ると音声の子プロセスが取り残される
    let audio_result = audio_thread.join().unwrap_or_else(|_| {
        Err(PaeError::ExternalProcess {
            tool: "ffmpeg (音声パス)".into(),
            code: None,
            stderr: "音声パスを実行するスレッドが異常終了しました".into(),
        })
    });
    let video_log = video_result?;
    let audio_log = audio_result?;

    // 多重化は再エンコードなしなので数秒で終わる
    let mux_args: Vec<String> = vec![
        "-i".into(),
        video_part.display().to_string(),
        "-i".into(),
        audio_part.display().to_string(),
        "-map".into(),
        "0:v".into(),
        "-map".into(),
        "1:a".into(),
        "-c".into(),
        "copy".into(),
        "-movflags".into(),
        "+faststart".into(),
        output.display().to_string(),
    ];
    let mux_log = ffmpeg.run(
        &mux_args,
        Some(output_duration_ms),
        &mut |fraction| on_progress(0.95 + fraction * 0.05),
        cancel,
    )?;

    Ok(format!("{video_log}{audio_log}{mux_log}"))
}

/// 出力長が期待どおりかを判定する。
///
/// 許容誤差を出力長の 2% (最低2秒) と広めに取っているのは、
/// select がフレーム単位、aselect が音声フレーム単位で切るため、
/// keep_ranges の数だけ丸め誤差が積み上がるのを許すため。
/// 捉えたいのは数十%規模の欠落なので、この広さでも用は足りる
fn duration_within_tolerance(actual_ms: u64, expected_ms: u64) -> bool {
    let tolerance_ms = (expected_ms / 50).max(2_000);
    actual_ms.abs_diff(expected_ms) <= tolerance_ms
}

/// 生成したファイルが期待どおりの長さかを確かめる。
///
/// ffmpeg は長尺の入力で音声を途中までしか書かないことがあり、
/// そのとき警告を出さず終了コードも 0 のままになる。
/// 外部プロセスの成否だけでは検知できないので、ここで長さを突き合わせる
fn verify_output_duration(
    ffmpeg: &Ffmpeg,
    output: &Path,
    expected_ms: u64,
    stage: &str,
) -> Result<()> {
    let durations = probe_stream_durations(ffmpeg, output)?;
    for (stream, actual_ms) in [("映像", durations.video_ms), ("音声", durations.audio_ms)] {
        let Some(actual_ms) = actual_ms else { continue };
        if !duration_within_tolerance(actual_ms, expected_ms) {
            return Err(PaeError::OutputTruncated {
                stage: stage.into(),
                stream: stream.into(),
                actual_ms,
                expected_ms,
            });
        }
    }
    Ok(())
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
    let log = ffmpeg.run(&args, Some(main_duration_ms), on_progress, cancel)?;
    verify_output_duration(ffmpeg, output, main_duration_ms, "BGM ミックス")?;
    Ok(log)
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
    let log = ffmpeg.run(&args, Some(duration_ms), on_progress, cancel)?;
    verify_output_duration(ffmpeg, output, duration_ms, "ラウドネス調整")?;
    Ok(log)
}

/// 完成した動画・音声から Podcast 用 MP3 を書き出す。
/// bitrate_kbps が 0 のときは VBR 高音質 (-q:a 2, 平均 ~190kbps) で書き出す
pub fn encode_mp3(
    ffmpeg: &Ffmpeg,
    input: &Path,
    output: &Path,
    duration_ms: u64,
    bitrate_kbps: u32,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<String> {
    let quality_args: [String; 2] = if bitrate_kbps == 0 {
        ["-q:a".into(), "2".into()]
    } else {
        ["-b:a".into(), format!("{bitrate_kbps}k")]
    };
    let mut args: Vec<String> = vec![
        "-i".into(),
        input.display().to_string(),
        "-vn".into(),
        "-c:a".into(),
        "libmp3lame".into(),
    ];
    args.extend(quality_args);
    args.push(output.display().to_string());
    let log = ffmpeg.run(&args, Some(duration_ms), on_progress, cancel)?;
    verify_output_duration(ffmpeg, output, duration_ms, "MP3 書き出し")?;
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_video_filter_snapshot() {
        let script = build_cut_video_filter(&[(0, 2200), (5000, 6000)], 0);
        insta::assert_snapshot!(script, @r"
        [0:v]select='between(t,0.000,2.200)+between(t,5.000,6.000)',setpts=N/FRAME_RATE/TB[v]
        ");
    }

    #[test]
    fn cut_audio_filter_snapshot() {
        let script = build_cut_audio_filter(&[(0, 2200), (5000, 6000)], 0, None);
        insta::assert_snapshot!(script, @r"
        [0:a]aselect='between(t,0.000,2.200)+between(t,5.000,6.000)',asetpts=N/SR/TB,aresample=async=1[a]
        ");
    }

    #[test]
    fn cut_video_filter_with_ending_tail() {
        let script = build_cut_video_filter(&[(0, 2200)], 5000);
        insta::assert_snapshot!(script, @r"
        [0:v]select='between(t,0.000,2.200)',setpts=N/FRAME_RATE/TB,tpad=stop_duration=5.000:stop_mode=clone[v]
        ");
    }

    #[test]
    fn cut_audio_filter_with_ending_tail() {
        let script = build_cut_audio_filter(&[(0, 2200)], 3000, None);
        insta::assert_snapshot!(script, @r"
        [0:a]aselect='between(t,0.000,2.200)',asetpts=N/SR/TB,aresample=async=1,apad=pad_dur=3.000[a]
        ");
    }

    /// 72分の出力での判定。フレーム境界の丸めは許し、実際に遭遇した欠落は捉えること
    #[test]
    fn tolerates_frame_rounding_but_catches_truncation() {
        assert!(duration_within_tolerance(4_332_747, 4_332_732));
        assert!(duration_within_tolerance(4_347_000, 4_332_732));
        // 音声が約 1/3 になった実例
        assert!(!duration_within_tolerance(1_449_400, 4_332_732));
    }

    /// 短い出力では 2% が小さくなりすぎるため、最低2秒を下限にしている
    #[test]
    fn short_output_uses_minimum_tolerance() {
        assert!(duration_within_tolerance(9_900, 8_000));
        assert!(!duration_within_tolerance(11_000, 8_000));
    }

    #[test]
    fn cut_audio_filter_with_fade_snapshot() {
        let script = build_cut_audio_filter(
            &[(0, 2200), (5000, 6000)],
            0,
            Some(Path::new("/tmp/pae/fade-commands.txt")),
        );
        insta::assert_snapshot!(script, @r"
        [0:a]asetnsamples=n=256:p=0,asendcmd=filename='/tmp/pae/fade-commands.txt',afade@paecut=t=in:st=0:d=0.020,aselect='between(t,0.000,2.200)+between(t,5.000,6.000)',asetpts=N/SR/TB,aresample=async=1[a]
        ");
    }

    /// 区間の先頭ごとに、その区間の終わりのフェードアウトを予約する。
    /// 最後の区間の終わりは継ぎ目ではないので、区間の外を指して掛からないようにする
    #[test]
    fn cut_fade_commands_snapshot() {
        let commands = build_cut_fade_commands(&[(0, 2200), (5000, 6000), (9000, 9500)]);
        insta::assert_snapshot!(commands, @r"
        0.000 afade@paecut type out, afade@paecut start_time 2.180, afade@paecut duration 0.020;
        5.000 afade@paecut type out, afade@paecut start_time 5.980, afade@paecut duration 0.020;
        9.000 afade@paecut type out, afade@paecut start_time 9.520, afade@paecut duration 0.020;
        ");
    }

    /// 区間がフェードより短いときは、その区間の長さまでフェードを縮める
    #[test]
    fn cut_fade_commands_clamp_short_range() {
        let commands = build_cut_fade_commands(&[(0, 2200), (5000, 5008), (9000, 9500)]);
        assert!(commands.contains("5.000 afade@paecut type out"));
        assert!(commands.contains("start_time 5.000, afade@paecut duration 0.008"));
    }

    /// 継ぎ目がひとつも無ければフェードの仕組みごと省く
    #[test]
    fn cut_fade_commands_empty_for_single_range() {
        assert!(build_cut_fade_commands(&[(0, 2200)]).is_empty());
    }

    #[test]
    fn filter_path_escapes_windows_drive_letter() {
        assert_eq!(
            escape_filter_path(Path::new(r"C:\tmp\fade.txt")),
            r"C\:/tmp/fade.txt"
        );
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
