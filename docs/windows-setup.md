# Windows での開発環境セットアップ

Windows 上で開発モード（`cargo` / `npm`）でアプリを動かすための手順です。
インストーラーでの配布はまだ対応していません。

## コマンドはどこで実行するか

このドキュメントのコマンドは、特に断りがない限りすべて
**PowerShell**（Windows Terminal で開くものでよい）から実行してください。
winget も cargo も npm も PowerShell からそのまま使えます。

**WSL は使わないでください。**
このアプリは Windows ネイティブのデスクトップアプリのため、
WSL 内でビルドすると Linux 向けバイナリになってしまい、
GUI の表示に使う WebView2 や、動画エンコードに使う Media Foundation が利用できません。

bash スクリプト（`scripts/make-fixtures.sh`）や `make` を使いたい場合だけ、
Git for Windows 付属の **Git Bash** を使います。詳しくは後述の対応表を参照してください。

## 必要なもの

| ツール | 入手方法 | 補足 |
| --- | --- | --- |
| Visual Studio Build Tools | `winget install Microsoft.VisualStudio.2022.BuildTools` | 「C++ によるデスクトップ開発」ワークロードを選ぶ。Rust (MSVC) と whisper.cpp のビルドに必要 |
| Rust (stable) | https://rustup.rs/ | MSVC ツールチェーン（デフォルト）を使う |
| CMake | `winget install Kitware.CMake` | whisper.cpp のビルドに必要 |
| Node.js | `winget install OpenJS.NodeJS.LTS` | デスクトップ GUI の開発に必要 |
| FFmpeg | `winget install Gyan.FFmpeg` | full ビルドのため libx264 入りで、統合テストもそのまま動く |
| WebView2 Runtime | 通常はプリインストール済み | Windows 11 と最近の Windows 10 には最初から入っている |

インストール後に新しいターミナルを開き、`ffmpeg -version` と `cargo --version` が
通ることを確認してください。

winget 以外で FFmpeg を入れた場合、PATH に無くても以下の場所は自動で探します。

- `%LOCALAPPDATA%\Microsoft\WinGet\Links`
- `C:\ProgramData\chocolatey\bin`
- `C:\ffmpeg\bin`
- `C:\Program Files\ffmpeg\bin`

それ以外の場所に置いた場合は、環境変数 `PAE_FFMPEG_DIR` でディレクトリを指定できます。

## CUDA による文字起こしの高速化（任意）

Windows では macOS の Metal に相当する GPU 支援が既定では使われず、
文字起こしが CPU 推論になり大幅に遅くなります。
NVIDIA GPU があるなら CUDA 対応でビルドすると高速になります。

1. [CUDA Toolkit](https://developer.nvidia.com/cuda-downloads) をインストールする
2. `cuda` feature を付けてビルド・起動する

```bash
make run-dev-cuda
```

make が無い環境では次のコマンドが同等です。

```bash
cd crates/pae-app && npm run tauri -- dev --features cuda
```

CLI の場合は `cargo run -p pae-cli --features cuda -- run input.mp4 -o output` のように
`--features cuda` を付けます。

## make が無い環境でのコマンド対応表

Makefile のターゲットは以下の生コマンドに対応します。
Git for Windows 付属の Git Bash を使う場合は `make` を入れれば Makefile もそのまま動きます。

PowerShell で実行する場合の注意:
Windows 標準の PowerShell 5.1 は `&&` でのコマンド連結に対応していません。
`cd crates/pae-app` と `npm install` を分けて実行するか、
`&&` が使える PowerShell 7 (`winget install Microsoft.PowerShell`) の利用をおすすめします。

| make ターゲット | 対応するコマンド |
| --- | --- |
| `make up` | `cd crates/pae-app && npm install` |
| `make run-dev` | `cd crates/pae-app && npm run tauri dev` |
| `make run-dev-cuda` | `cd crates/pae-app && npm run tauri -- dev --features cuda` |
| `make build` | `cargo build` |
| `make test` | `cargo test` |
| `make test-all` | `cargo test` のあと `cargo test -p pae-core -- --ignored` |
| `make fmt` | `cargo fmt` |
| `make lint` | `cargo clippy --all-targets` と `cd crates/pae-app && npx tsc --noEmit` |
| `make check` | fmt → lint → test を順に実行 |

`scripts/make-fixtures.sh` は bash スクリプトのため Git Bash で実行してください。
macOS の `say` コマンドを使う日本語音声の fixture だけは自動でスキップされますが、
統合テスト (`cargo test`) は fixtures/ を使わず ffmpeg で自前生成するため、
fixture 無しでもテストは通ります。

## Windows 固有の動作

- 出力動画のエンコーダは Media Foundation の `h264_mf` を使います
- 文字起こしモデル（500MB 超）は `%LOCALAPPDATA%\hatone\PodcastAutoEditor\data\models` に保存されます
- 設定ファイルは `%APPDATA%\hatone\PodcastAutoEditor\config\config.toml` に保存されます

## 動作確認チェックリスト

初めて Windows 環境を作ったときは、以下を一通り確認してください。

- [ ] `cargo test` が通る（ffmpeg のフィルタスクリプトを一時ファイル経由で読めるかの確認を兼ねる）
- [ ] GUI に動画をドラッグ＆ドロップすると、出力先のデフォルトが `<動画の場所>\podcast-output` になる
- [ ] 出力一覧の「表示」ボタンでエクスプローラーがファイルを選択した状態で開く
- [ ] 出力動画が h264_mf でエンコードされている（`ffprobe` の encoder 表示で確認）
- [ ] CUDA ビルドの場合、文字起こし中にタスクマネージャーで GPU が使われている
- [ ] モデルが `%LOCALAPPDATA%` 配下にダウンロードされる
- [ ] 日本語やスペースを含むパスの動画でも一通り動く
