# 技術調査メモと既知の落とし穴

2026-08 の技術調査結果の要約と、実装中に判明した注意点。

## 文字起こしエンジンの選定

whisper.cpp (whisper-rs) + large-v3-turbo q5_0 をデフォルトにした。

| 候補 | 判断 | 理由 |
|---|---|---|
| whisper.cpp + large-v3-turbo | **採用** | Metal 対応で 90分→10分前後。Rust に静的リンクでき配布が最も楽。日本語の自由会話に頑健 |
| Kotoba-Whisper v2.0 (GGML) | モデル選択肢として対応 | TV音声由来の ReazonSpeech 学習でドメイン依存が強い。2026年の第三者ベンチマーク（Neosophie）では複数話者の自由会話で CER 0.495 と large-v3-turbo (0.184) に大差で劣る。公式 GGML 変換があるため whisper.cpp でそのまま動く |
| faster-whisper | 不採用 | Apple Silicon で Metal 非対応 (CPU のみ)、Python 必須 |
| mlx-whisper | 不採用 | whisper.cpp の約2倍速だが Python 同梱が必要で配布が複雑化 |

- モデルは初回利用時に Hugging Face からダウンロードし、
  `~/Library/Application Support/net.hatone.PodcastAutoEditor/models/` にキャッシュする。
  Windows は `%LOCALAPPDATA%` 配下 (data_local_dir)。574MB のモデルが
  ドメイン環境でログオン同期される Roaming (data_dir) を避けるため。
- モデル追加は `crates/pae-core/src/transcribe/model.rs` の `MODELS` に1エントリ足すだけ。

### GPU 推論

- macOS: Metal を常時有効（target 依存の feature 指定なのでビルド環境の追加要件なし）
- Windows / Linux: `cuda` feature の opt-in（要 CUDA Toolkit + NVIDIA GPU）。
  CUDA Toolkit が無い環境でもビルドできるよう、target 依存での無条件付与にはしていない。
  feature は pae-cli / pae-app → pae-core → whisper-rs へと伝播する。
  whisper.cpp は GPU ビルドならデフォルトで GPU を使うためコード変更は不要。
  feature なしでは CPU 推論になり、Apple Silicon の Metal より大幅に遅い

### whisper-rs 0.16 の既知バグ

`FullParams::set_abort_callback_safe` はクロージャを二重 Box するのに trampoline が
具象型として直接デリファレンスするため未定義動作になる（whisper_full が error -6 を返す）。
本プロジェクトでは生ポインタ API + AtomicBool で回避している
（`transcribe/mod.rs` の abort_trampoline）。whisper-rs 更新時は修正状況を確認すること。

## VAD の選定

Silero VAD (`voice_activity_detector` crate) を採用。

- MIT ライセンス、約2MB の ONNX モデルが crate に同梱（ネットワーク不要）
- 16kHz で 512 サンプル (32ms) 単位の確率を返す
- TEN VAD は精度で勝るが「Agora と競合する形での利用禁止」条項付きで不採用
- WebRTC VAD は精度不足（Silero 比エラー約4倍）

日本語の相槌（「うん」等、100〜200ms）を拾うため recall 重視のパラメータにしている:
threshold 0.4 / min_speech 100ms / min_silence 250ms / padding 前150ms・後250ms。
`min_speech_ms` を 250ms などに上げると相槌が消えるので注意。

## 無音カットの方式

`select`/`aselect` + `setpts`/`asetpts` + `aresample=async=1` による1パス全再エンコードを採用。

- smart cut (区間 stream copy + 境界のみ再エンコード) は高速だがグリッチ・同期ズレの
  報告が多く不採用。将来の高速化候補として温存
- カット後の PTS 振り直し（setpts=N/FRAME_RATE/TB / asetpts=N/SR/TB）が音ズレ防止の要
- フィルタ式はファイル渡しにする。**ffmpeg 9 で `-filter_complex_script` は廃止**され、
  汎用の `-/filter_complex <file>` 構文に変わった
- エンコーダ: macOS は h264_videotoolbox（Media Engine、x264 比約4倍速）。
  品質は同ビットレートで x264 に劣るためビットレートを多めにしている (720p: 4M, 1080p: 6M)
- Windows は h264_mf（Media Foundation）。LGPL ビルドの ffmpeg に libx264 が入らないため。
  ビットレート表は videotoolbox 用に盛った値をそのまま流用している。
  CI の Windows ランナーにはハードウェアエンコーダが無いため、統合テストは libx264 に固定している

## FFmpeg の配布とライセンス（Phase 3 で対応）

- 開発中は PATH の ffmpeg（Homebrew 等）を使用。`PAE_FFMPEG_DIR` で上書き可能
- 配布時は **LGPL 構成の自前ビルド**を Tauri sidecar として同梱する
  - libx264 は GPL のため使わない。macOS=h264_videotoolbox / Windows=h264_mf で LGPL を維持
  - macOS arm64 の LGPL 静的バイナリを配る定番サイトは存在しないため自前ビルドが必要
    （martin-riedl.de のビルドスクリプトがベースに使える。Windows は BtbN の LGPL 版が使える）
- LGPL の義務: 対応ソースの提供、About 画面での表記
  「This software uses code of FFmpeg licensed under the LGPLv2.1」、LGPL 全文の同梱

## loudnorm

2パス (測定→linear=true で適用) で -16 LUFS (Apple Podcasts 標準) に合わせる。

- 測定 JSON は stderr の末尾に出る。他ログと混ざるため最後の `{...}` を抽出してパースする
- loudnorm は内部で 192kHz にアップサンプルするため出力に `-ar 48000` を明示する

## ライセンスまとめ（配布時に表記が必要）

| 依存 | ライセンス |
|---|---|
| FFmpeg (LGPL ビルド) | LGPL 2.1+ (ソース提供義務あり) |
| whisper.cpp / whisper-rs | MIT |
| Whisper モデル (OpenAI) | MIT |
| Kotoba-Whisper モデル | Apache 2.0 |
| Silero VAD (モデル含む) | MIT |
| voice_activity_detector / ort | MIT / Apache 2.0 |

## 聴感評価からの品質改善 (実装済み)

- **声の帯域の EQ 分離**: BGM 側だけ `equalizer=f=2500:width_type=o:w=2` で
  声の中心帯域を下げる (`voice_duck_db`, デフォルト -4dB, 0 で無効)。
  声側は加工しない
- **エンディングの BGM 余韻**: カット段階で映像の最終フレーム静止 (tpad) と
  音声の無音パディング (apad) を末尾に足し、BGM のフェードアウトを
  その余韻に重ねる (`ending_tail_s`, デフォルト5秒, BGM なしなら付かない)

## ベンチマーク実測 (M系 Apple Silicon, 2026-08)

実録画 (720p30) をフルパイプライン処理 (BGM + loudnorm + 文字起こし込み):

- 161秒入力 + large-v3-turbo: 合計 35.1s (**RTF 0.22**)。60分動画なら約13分の計算
- 内訳の傾向: 動画編集と loudnorm が各 ≈ 実時間の 6%、文字起こし (turbo) ≈ 6%、VAD ≈ 0.6%
- ボトルネックは h264_videotoolbox の再エンコードと loudnorm の2パス
