//! ffmpeg で生成した短いテストメディアを使う統合テスト。
//! カット結果の長さ・A/V 同期・トーン位置を数値で検証する。
//! ffmpeg が PATH にあることが前提 (開発必須要件のため、なければ失敗させる)

use std::path::{Path, PathBuf};

use pae_core::media::extract::{extract_analysis_wav, read_wav_samples};
use pae_core::media::ffmpeg::Ffmpeg;
use pae_core::media::probe::probe;
use pae_core::media::process::{
    apply_loudnorm, cut_media, measure_loudness, mix_bgm, BgmOpts, LoudnormTarget, VideoEncodeOpts,
};
use pae_core::progress::CancelToken;
use pae_core::timeline::timeline_to_keep_ranges;
use pae_core::types::{
    EditTimeline, SegmentAction, SegmentKind, TimelineSegment, TimelineStats, VadParams,
    TIMELINE_VERSION,
};

fn ffmpeg() -> Ffmpeg {
    Ffmpeg::locate(None).expect("ffmpeg が見つかりません。開発には ffmpeg が必要です")
}

fn cancel() -> CancelToken {
    CancelToken::new()
}

/// テストでは OS ごとの自動選択を使わず libx264 に固定する。
/// CI の Windows ランナーにはハードウェアエンコーダが無く、
/// auto() が選ぶ h264_mf で失敗するため
fn test_encode_opts() -> VideoEncodeOpts {
    VideoEncodeOpts {
        encoder: "libx264".into(),
        bitrate: "2500k".into(),
    }
}

/// トーンパターン動画を生成する。
/// 音声: 2s 440Hz → 4s 無音 → 2s 880Hz → 1s 無音 → 2s 440Hz (計11秒)
fn generate_tone_video(ffmpeg: &Ffmpeg, dir: &Path) -> PathBuf {
    let out = dir.join("tone.mp4");
    let args: Vec<String> = [
        "-f",
        "lavfi",
        "-i",
        "sine=440:d=2",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=44100:cl=mono:d=4",
        "-f",
        "lavfi",
        "-i",
        "sine=880:d=2",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=44100:cl=mono:d=1",
        "-f",
        "lavfi",
        "-i",
        "sine=440:d=2",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=320x240:rate=30:duration=11",
        "-filter_complex",
        "[0][1][2][3][4]concat=n=5:v=0:a=1[a]",
        "-map",
        "5:v",
        "-map",
        "[a]",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([out.display().to_string()])
    .collect();
    ffmpeg
        .run(&args, None, &mut |_| {}, &cancel())
        .expect("テスト動画の生成に失敗");
    out
}

/// トーン区間 (2-6秒の無音を1秒へ短縮、8-9秒の無音は残す) の手書きタイムライン
fn tone_timeline(source: &Path) -> EditTimeline {
    let seg = |start: u64, end: u64, kind: SegmentKind, action: SegmentAction, keep: u64| {
        TimelineSegment {
            source_start_ms: start,
            source_end_ms: end,
            kind,
            action,
            keep_duration_ms: keep,
        }
    };
    use SegmentAction::*;
    use SegmentKind::*;
    let segments = vec![
        seg(0, 2000, Speech, Keep, 2000),
        seg(2000, 6000, Silence, Compress, 1000),
        seg(6000, 8000, Speech, Keep, 2000),
        seg(8000, 9000, Silence, Keep, 1000),
        seg(9000, 11000, Speech, Keep, 2000),
    ];
    EditTimeline {
        version: TIMELINE_VERSION,
        source_path: source.to_path_buf(),
        source_duration_ms: 11_000,
        preset_name: "test".into(),
        vad_params: VadParams::default(),
        segments,
        stats: TimelineStats {
            source_duration_ms: 11_000,
            output_duration_ms: 8_000,
            silence_count: 2,
            compressed_count: 1,
        },
    }
}

/// 音声を 50ms 窓の RMS で解析し、トーンの ON/OFF が切り替わる時刻 (ms) を返す
fn detect_transitions(ffmpeg: &Ffmpeg, media: &Path, duration_ms: u64) -> Vec<u64> {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("out.wav");
    extract_analysis_wav(ffmpeg, media, &wav, duration_ms, &mut |_| {}, &cancel()).unwrap();
    let (samples, sample_rate) = read_wav_samples(&wav).unwrap();

    let window = (sample_rate as usize / 1000) * 50;
    let mut transitions = Vec::new();
    let mut prev_on = false;
    for (i, chunk) in samples.chunks(window).enumerate() {
        let rms =
            (chunk.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / chunk.len() as f64).sqrt();
        let on = rms > 1000.0;
        if i > 0 && on != prev_on {
            transitions.push(i as u64 * 50);
        }
        prev_on = on;
    }
    transitions
}

#[test]
fn cut_preserves_av_sync_and_tone_positions() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = generate_tone_video(&ffmpeg, dir.path());
    let timeline = tone_timeline(&input);
    let keep_ranges = timeline_to_keep_ranges(&timeline);
    assert_eq!(keep_ranges, vec![(0, 3000), (6000, 11000)]);

    let output = dir.path().join("cut.mp4");
    cut_media(
        &ffmpeg,
        &input,
        &keep_ranges,
        &output,
        timeline.stats.output_duration_ms,
        0,
        true,
        &test_encode_opts(),
        &mut |_| {},
        &cancel(),
    )
    .unwrap();

    // 出力の長さ: 8秒 ± 100ms (エンコーダのフレーム境界誤差を許容)
    let info = probe(&ffmpeg, &output).unwrap();
    assert!(
        (info.duration_ms as i64 - 8000).abs() < 100,
        "出力の長さが期待とずれています: {}ms",
        info.duration_ms
    );

    // カット後のトーン配置: 0-2s ON, 2-3s OFF, 3-5s ON(880Hz), 5-6s OFF, 6-8s ON
    // → 遷移は 2.0 / 3.0 / 5.0 / 6.0 秒付近の4箇所
    let transitions = detect_transitions(&ffmpeg, &output, info.duration_ms);
    let expected = [2000u64, 3000, 5000, 6000];
    assert_eq!(
        transitions.len(),
        expected.len(),
        "トーン遷移の数が違います: {transitions:?}"
    );
    for (actual, expected) in transitions.iter().zip(expected.iter()) {
        assert!(
            (*actual as i64 - *expected as i64).abs() <= 100,
            "トーン遷移位置がずれています: {actual}ms (期待 {expected}ms)"
        );
    }
}

/// 音声のみの入力 (WAV) でもカットでき、トーン位置がずれないこと
#[test]
fn cut_audio_only_input() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();

    // 動画版と同じトーンパターンの WAV を作る
    let input = dir.path().join("tone.wav");
    let args: Vec<String> = [
        "-f",
        "lavfi",
        "-i",
        "sine=440:d=2",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=44100:cl=mono:d=4",
        "-f",
        "lavfi",
        "-i",
        "sine=880:d=2",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=r=44100:cl=mono:d=1",
        "-f",
        "lavfi",
        "-i",
        "sine=440:d=2",
        "-filter_complex",
        "[0][1][2][3][4]concat=n=5:v=0:a=1[a]",
        "-map",
        "[a]",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([input.display().to_string()])
    .collect();
    ffmpeg.run(&args, None, &mut |_| {}, &cancel()).unwrap();

    let timeline = tone_timeline(&input);
    let keep_ranges = timeline_to_keep_ranges(&timeline);
    let output = dir.path().join("cut.flac");
    cut_media(
        &ffmpeg,
        &input,
        &keep_ranges,
        &output,
        timeline.stats.output_duration_ms,
        0,
        false,
        &test_encode_opts(),
        &mut |_| {},
        &cancel(),
    )
    .unwrap();

    let info = probe(&ffmpeg, &output).unwrap();
    assert!(!info.has_video);
    assert!(
        (info.duration_ms as i64 - 8000).abs() < 100,
        "出力の長さが期待とずれています: {}ms",
        info.duration_ms
    );

    let transitions = detect_transitions(&ffmpeg, &output, info.duration_ms);
    let expected = [2000u64, 3000, 5000, 6000];
    assert_eq!(transitions.len(), expected.len(), "遷移: {transitions:?}");
    for (actual, expected) in transitions.iter().zip(expected.iter()) {
        assert!(
            (*actual as i64 - *expected as i64).abs() <= 100,
            "トーン遷移位置がずれています: {actual}ms (期待 {expected}ms)"
        );
    }
}

#[test]
fn bgm_mix_keeps_main_duration() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();
    let input = generate_tone_video(&ffmpeg, dir.path());

    // 3秒しかない BGM を 11秒の本編にループでミックスする
    let bgm = dir.path().join("bgm.mp3");
    let args: Vec<String> = ["-f", "lavfi", "-i", "sine=330:d=3", "-c:a", "libmp3lame"]
        .iter()
        .map(|s| s.to_string())
        .chain([bgm.display().to_string()])
        .collect();
    ffmpeg.run(&args, None, &mut |_| {}, &cancel()).unwrap();

    let output = dir.path().join("mixed.mp4");
    mix_bgm(
        &ffmpeg,
        &input,
        &bgm,
        &output,
        11_000,
        true,
        &BgmOpts::default(),
        &mut |_| {},
        &cancel(),
    )
    .unwrap();

    let info = probe(&ffmpeg, &output).unwrap();
    assert!(
        (info.duration_ms as i64 - 11_000).abs() < 150,
        "BGM ミックス後の長さが本編とずれています: {}ms",
        info.duration_ms
    );
}

#[test]
fn loudnorm_reaches_target() {
    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();

    // 小さめの音量のテスト音声を作る
    let input = dir.path().join("quiet.mp4");
    let args: Vec<String> = [
        "-f",
        "lavfi",
        "-i",
        "sine=440:d=5",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=320x240:rate=30:duration=5",
        "-filter_complex",
        "[0:a]volume=0.1[a]",
        "-map",
        "1:v",
        "-map",
        "[a]",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-shortest",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([input.display().to_string()])
    .collect();
    ffmpeg.run(&args, None, &mut |_| {}, &cancel()).unwrap();

    let target = LoudnormTarget::default();
    let measured = measure_loudness(&ffmpeg, &input, &target, &cancel()).unwrap();

    let output = dir.path().join("normalized.mp4");
    apply_loudnorm(
        &ffmpeg,
        &input,
        &output,
        &target,
        &measured,
        5_000,
        true,
        &mut |_| {},
        &cancel(),
    )
    .unwrap();

    // 出力を再測定してターゲット付近に収まっていることを確認
    let remeasured = measure_loudness(&ffmpeg, &output, &target, &cancel()).unwrap();
    let output_i: f64 = remeasured.input_i.parse().unwrap();
    assert!(
        (output_i - target.i).abs() < 1.0,
        "出力ラウドネスがターゲットから外れています: {output_i} LUFS (目標 {})",
        target.i
    );
}

/// 文字起こしの統合テスト。tiny モデルのダウンロードが必要なため #[ignore]。
/// 実行: cargo test -p pae-core -- --ignored
#[test]
#[ignore]
fn transcribe_produces_monotonic_segments() {
    use pae_core::transcribe::model::{find_model, ModelManager};
    use pae_core::transcribe::{Transcriber, WhisperTranscriber};

    let ffmpeg = ffmpeg();
    let dir = tempfile::tempdir().unwrap();

    let spec = find_model("tiny").unwrap();
    let manager = ModelManager::new().unwrap();
    let model_path = manager
        .ensure_model(spec, &mut |_| {}, &cancel())
        .expect("tiny モデルの取得に失敗");

    // 人の声がないと whisper は何も返さないことがあるため、テストは
    // 「クラッシュせず、返ったセグメントのタイムスタンプが単調増加」のみ検証する
    let input = generate_tone_video(&ffmpeg, dir.path());
    let wav = dir.path().join("t.wav");
    extract_analysis_wav(&ffmpeg, &input, &wav, 11_000, &mut |_| {}, &cancel()).unwrap();
    let (samples, _) = read_wav_samples(&wav).unwrap();

    let mut transcriber = WhisperTranscriber::load(&model_path).unwrap();
    let segments = transcriber
        .transcribe(&samples, "ja", &mut |_| {}, &cancel())
        .unwrap();

    let mut prev_end = 0u64;
    for seg in &segments {
        assert!(seg.start_ms <= seg.end_ms);
        assert!(seg.start_ms >= prev_end || seg.start_ms >= prev_end.saturating_sub(500));
        prev_end = seg.end_ms;
    }
}
