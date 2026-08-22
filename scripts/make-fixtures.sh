#!/bin/bash
# テスト用のメディアファイルを fixtures/ に生成する
# 巨大な動画を Git で管理する代わりに、既知のパターンを持つ短いメディアを
# ffmpeg (と macOS の say) で再現可能に作る
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p fixtures
cd fixtures

echo "=== トーンパターン音声 (11秒) ==="
# 2s 440Hz → 4s 無音 → 2s 880Hz → 1s 無音 → 2s 440Hz
# 無音短縮の検証で「どの区間が残ったか」を周波数で判別できる
ffmpeg -y -v error \
  -f lavfi -i "sine=440:d=2" \
  -f lavfi -i "anullsrc=r=44100:cl=mono:d=4" \
  -f lavfi -i "sine=880:d=2" \
  -f lavfi -i "anullsrc=r=44100:cl=mono:d=1" \
  -f lavfi -i "sine=440:d=2" \
  -filter_complex "[0][1][2][3][4]concat=n=5:v=0:a=1[a]" -map "[a]" \
  tone.wav

echo "=== トーンパターン動画 (11秒, タイムスタンプ焼き込み) ==="
# 映像にソース時刻を焼き込むことで、カット後にどの区間が残ったか目視できる
ffmpeg -y -v error \
  -f lavfi -i "testsrc=size=1280x720:rate=30:duration=11" \
  -i tone.wav \
  -c:v libx264 -preset fast -pix_fmt yuv420p -c:a aac -shortest \
  tone_pattern.mp4

echo "=== 日本語音声パターン ==="
# VAD・文字起こしの検証用。純粋なトーンは VAD が発話と判定しないため
# macOS の say で日本語合成音声を作る
if command -v say >/dev/null; then
  say -v Kyoko "こんにちは、今日はポッドキャストの自動編集について話します。" -o speech1.aiff
  say -v Kyoko "そうですね、それは確かに便利だと思います。" -o speech2.aiff
  say -v Kyoko "うん。" -o aizuchi.aiff
  say -v Kyoko "それでは、また次回お会いしましょう。ありがとうございました。" -o speech3.aiff

  # 2s 無音 → 発話1 → 4s 無音 → 発話2 → 0.8s 無音 → 相槌 → 5s 無音 → 発話3 → 2s 無音
  ffmpeg -y -v error \
    -f lavfi -i "anullsrc=r=16000:cl=mono:d=2" \
    -i speech1.aiff \
    -f lavfi -i "anullsrc=r=16000:cl=mono:d=4" \
    -i speech2.aiff \
    -f lavfi -i "anullsrc=r=16000:cl=mono:d=0.8" \
    -i aizuchi.aiff \
    -f lavfi -i "anullsrc=r=16000:cl=mono:d=5" \
    -i speech3.aiff \
    -f lavfi -i "anullsrc=r=16000:cl=mono:d=2" \
    -filter_complex "[0][1][2][3][4][5][6][7][8]concat=n=9:v=0:a=1,aresample=16000[a]" \
    -map "[a]" -ac 1 \
    speech_pattern.wav

  DUR=$(ffprobe -v error -show_entries format=duration -of csv=p=0 speech_pattern.wav)
  ffmpeg -y -v error \
    -f lavfi -i "testsrc=size=1280x720:rate=30:duration=${DUR}" \
    -i speech_pattern.wav \
    -c:v libx264 -preset fast -pix_fmt yuv420p -c:a aac -shortest \
    speech_pattern.mp4
  rm -f speech1.aiff speech2.aiff aizuchi.aiff speech3.aiff
else
  echo "say コマンドがないため日本語音声パターンをスキップしました"
fi

echo "=== テスト用 BGM (8秒ループ素材) ==="
ffmpeg -y -v error \
  -f lavfi -i "sine=frequency=220:duration=8" \
  -f lavfi -i "sine=frequency=330:duration=8" \
  -filter_complex "[0][1]amix=inputs=2,tremolo=f=0.5:d=0.5,volume=0.6[a]" \
  -map "[a]" -c:a libmp3lame -q:a 4 \
  bgm.mp3

echo "完了: fixtures/ にテストメディアを生成しました"
ls -lh *.wav *.mp4 *.mp3 2>/dev/null
