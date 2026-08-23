# Windows での開発環境セットアップ

Windows 上で開発モード（`cargo` / `npm`）でアプリを動かすための手順です。  
自分用の exe を作る手順も後半にありますが、ffmpeg を同梱しないため、  
他のマシンへそのまま配布することはまだできません。

## コマンドはどこで実行するか

このドキュメントのコマンドは、特に断りがない限りすべて  
**PowerShell**（Windows Terminal で開くものでよい）から実行してください。  
winget も cargo も npm も PowerShell からそのまま使えます。

ただしビルド系のコマンドは、**Developer PowerShell for VS 2022** から実行してください。  
Visual Studio と一緒にスタートメニューへ入るショートカットです。  
clang は MSVC と Windows SDK のヘッダーの場所を自力では見つけられず、  
通常の PowerShell でビルドすると `stdio.h` が見つからないというエラーで失敗します。  
Developer PowerShell は環境変数 `INCLUDE` を設定してくれるため、これが解決します。

いま開いている PowerShell をそのまま切り替えることもできます。

```powershell
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
Import-Module "$vs\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments "-arch=x64 -host_arch=x64"
```

winget でのインストール作業は、通常の PowerShell のままで構いません。

**WSL は使わないでください。**  
このアプリは Windows ネイティブのデスクトップアプリのため、  
WSL 内でビルドすると Linux 向けバイナリになってしまい、  
GUI の表示に使う WebView2 や、動画エンコードに使う Media Foundation が利用できません。

bash スクリプト（`scripts/make-fixtures.sh`）や `make` を使いたい場合だけ、  
Git for Windows 付属の **Git Bash** を使います。詳しくは後述の対応表を参照してください。

## 必要なもの

| ツール | 入手方法 | 補足 |
| --- | --- | --- |
| Visual Studio Build Tools | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` | Rust (MSVC) と whisper.cpp のビルドに必要。`--override` を省くと外枠だけが入り、C++ コンパイラが入らない |
| Rust (stable) | `winget install Rustlang.Rustup` もしくは https://rustup.rs | MSVC ツールチェーン（デフォルト）を使う |
| CMake | `winget install Kitware.CMake` | whisper.cpp のビルドに必要 |
| LLVM (libclang) | `winget install LLVM.LLVM` | whisper-rs の bindgen が `libclang.dll` を必要とする。あわせて後述の `LIBCLANG_PATH` を設定する |
| Node.js | `winget install OpenJS.NodeJS.LTS` | デスクトップ GUI の開発に必要 |
| FFmpeg | `winget install Gyan.FFmpeg` | full ビルドのため libx264 入りで、統合テストもそのまま動く |
| WebView2 Runtime | 通常はプリインストール済み | Windows 11 と最近の Windows 10 には最初から入っている |

インストール後に新しいターミナルを開き、`ffmpeg -version` と `cargo --version` が通ることを確認してください。

### 環境変数の設定

LLVM を入れたら、環境変数 `LIBCLANG_PATH` を設定してください。  
whisper-rs は C のヘッダーから Rust の定義を作るのに bindgen を使い、  
bindgen は `libclang.dll` の場所をこの変数から探すためです。

```powershell
[Environment]::SetEnvironmentVariable('LIBCLANG_PATH', 'C:\Program Files\LLVM\bin', 'User')
```

未設定のままビルドすると `Unable to find libclang` というエラーで止まります。  
Visual Studio にも `VC\Tools\Llvm` というフォルダがありますが、  
入っているのは clang-format と clang-tidy だけで `libclang.dll` は含まれないため、LLVM を別に入れる必要があります。

あわせて `BINDGEN_EXTRA_CLANG_ARGS` も設定してください。  
bindgen は `clang.exe` を起動せず `libclang.dll` を読み込んで使うため、  
clang が自分の組み込みヘッダーの置き場所を見失い、`stdbool.h` が見つからないというエラーになります。  
その場所を直接教えるための設定です。

```powershell
[Environment]::SetEnvironmentVariable('BINDGEN_EXTRA_CLANG_ARGS', '"-IC:/Program Files/LLVM/lib/clang/22/include"', 'User')
```

パスの `22` は LLVM のメジャーバージョンなので、LLVM を更新したら書き換えてください。  
値を二重引用符で囲み、区切りをスラッシュにしているのには理由があります。  
bindgen はこの変数をシェルと同じ規則で単語に分割するため、  
囲まないと `Program Files` の空白でパスが切れ、バックスラッシュもエスケープとして消えてしまいます。

これらの設定を忘れると、bindgen は失敗してもビルドを止めず、  
クレートに同梱された Linux 用の定義へ黙って差し替えます。  
その結果 `_IO_FILE` や `_G_fpos_t` のサイズが合わないという、  
原因のわかりにくいコンパイルエラーになります。

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
    ```powershell
    winget install Nvidia.CUDA
    ```
2. make でビルド・起動する
    ```bash
    make up # 依存を解決
    make run-dev-cuda # CUDAを使いビルド・起動
    ```

make コマンドが無い環境では、以下で代替できます。  
`--features cuda` がポイントです。

```bash
cd crates/pae-app && npm run tauri -- dev --features cuda
```

make コマンドは `winget install ezwinports.make` でインストールできます。

### おまけ: CLI で使う場合

デスクトップアプリを起動せず、コマンドラインだけで処理することもできます。  
文字起こしを GPU でおこなうには、ここでも `--features cuda` を付けます。

```bash
cargo run -p pae-cli --features cuda -- run input.mp4 -o output
```

無音検出とタイムライン生成だけを確認したい場合は `analyze` を使います。  
こちらは文字起こしをしないため、`--features cuda` を付ける意味はありません。

```bash
cargo run -p pae-cli -- analyze input.mp4
```

## 配布用の exe を作る

開発モードではなく、ダブルクリックで起動できる実行ファイルを作れます。

```bash
make build-exe-cuda # CUDA を有効にする場合
make build-exe      # CUDA を使わない場合
```

必要な環境変数と Developer PowerShell の条件は、開発モードと同じです。  
初回は Tauri が WiX と NSIS を自動でダウンロードするため、時間がかかります。

生成物は次の場所に出ます。

| 生成物 | 場所 |
| --- | --- |
| 実行ファイル | `target/release/pae-app.exe` |
| MSI インストーラー | `target/release/bundle/msi/` |
| NSIS インストーラー | `target/release/bundle/nsis/` |

### 他のマシンへ渡す場合の注意

作った exe は ffmpeg を同梱していません。  
起動時に PATH や既知のディレクトリから探す作りのため、  
自分のマシンでは動きますが、ffmpeg が入っていないマシンでは動きません。

`--features cuda` を付けた exe は、さらに CUDA のランタイム DLL を必要とします。  
CUDA Toolkit が入っていないマシンでは起動しないため、  
渡す相手を選ばないようにするなら CUDA 無しでビルドしてください。

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
| `make build-exe` | `cd crates/pae-app && npm run tauri build` |
| `make build-exe-cuda` | `cd crates/pae-app && npm run tauri -- build --features cuda` |
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
