# AGENTS.md

このファイルはコーディングエージェントから常に参照される。追記は簡潔におこなう。
参照頻度が低い情報は `docs/` に分離し、必要な場合のみ参照する。

## 基本姿勢

ユーザーへの説明は平易な日本語でおこなう。
コミットメッセージは変更理由が追いやすいよう丁寧に書く。prefix は不要。

## アプリ概要

Google Meet 等の録画から、長すぎる無音を自然に短縮し、BGM追加とローカル文字起こしをおこなうデスクトップアプリ。
macOS / Apple Silicon を最優先とする。

## 重要な方針

- Podcast の動画・音声を外部サーバーへ送信しない（モデルの初回ダウンロードのみネットワーク使用）
- 有料 API を利用しない
- 「無音削除」ではなく「長すぎる無音の自然な短縮」を優先する
- GUI とメディア処理ロジックを分離する（コアは `pae-core`、GUI は将来の `pae-app`）
- 動画全体をメモリへ読み込まない
- 編集内容は中間の EditTimeline データとして表現する（時間は u64 ミリ秒）
- 外部プロセスのエラーを握りつぶさない

## 技術構成

- コア/CLI: Rust (Cargo workspace)。GUI は Tauri 2 を予定（Phase 3、未実装）
- VAD: Silero VAD (`voice_activity_detector` crate)
- 文字起こし: whisper.cpp (`whisper-rs`, Metal)。デフォルトモデル large-v3-turbo q5_0
- メディア処理: ffmpeg 外部プロセス（開発時は PATH の ffmpeg、配布時は sidecar 予定）

## コマンド

```bash
# unit test + integration test (ffmpeg 必須)
cargo test

# モデルダウンロードが必要なテスト
cargo test -p pae-core -- --ignored

# テスト用メディア生成
./scripts/make-fixtures.sh

# CLI 実行例
cargo run -p pae-cli -- run input.mp4 -o output --bgm bgm.mp3
cargo run -p pae-cli -- analyze input.mp4       # timeline.json 生成のみ
cargo run -p pae-cli -- dev --help              # 検証用低レベルコマンド

# format / lint
cargo fmt
cargo clippy --all-targets
```

変更後は `cargo fmt && cargo clippy --all-targets && cargo test` を必ず実行する。

## アーキテクチャ

- UI から FFmpeg や文字起こし処理を直接呼ばない。`pae_core::pipeline::run_job` を入口にする
- MediaProcessor (media/) / Vad (vad.rs) / Transcriber (transcribe/) / TimelineGenerator (timeline.rs) の責務を混在させない
- timeline.rs は純粋関数のみ。I/O を持ち込まない
- ffmpeg のフィルタ式生成は純粋関数にしてスナップショットテストする
- 将来の差し替えを考慮するが、過度な抽象化は避ける（trait は Vad / Transcriber / ProgressSink のみ）

## コーディングルール

- セクション区切りコメントやナンバリングコメントは禁止
- コメントは自明でない意図や仕組みだけを平易な言葉で書く
- 1行コメントの末尾に句点は不要
- 巨大な関数・コンポーネントを作らない
- TypeScript の `any`、Rust の不要な unsafe は原則使用しない（unsafe には理由コメント必須）

## テスト

- timeline.rs の判定ロジックは境界値ユニットテストで重点的に検証する
- FFmpeg を含む処理は生成した数秒のメディアで統合テストする（`crates/pae-core/tests/`）
- 巨大な動画ファイルをリポジトリへコミットしない（`fixtures/` は gitignore 済み）

技術調査の詳細・既知の落とし穴は `docs/tech-notes.md` を参照する。
