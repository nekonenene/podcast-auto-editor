# よく使うコマンド集。`make` または `make help` で一覧を表示する

.DEFAULT_GOAL := help

.PHONY: help
help: ## このヘルプを表示する
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

.PHONY: up
up: ## 開発環境の準備 (GUI の npm 依存をインストールする)
	cd crates/pae-app && npm install

.PHONY: run-dev
run-dev: ## デスクトップアプリを開発モードで起動する
	cd crates/pae-app && npm run tauri dev

.PHONY: run-dev-cuda
run-dev-cuda: ## CUDA 有効でデスクトップアプリを起動する (要 NVIDIA GPU + CUDA Toolkit)
	cd crates/pae-app && npm run tauri -- dev --features cuda

.PHONY: build
build: ## ワークスペース全体をビルドする
	cargo build

.PHONY: test
test: ## ユニットテストと統合テストを実行する (ffmpeg 必須)
	cargo test

.PHONY: test-all
test-all: test ## モデルダウンロードが必要なテストも含めて実行する
	cargo test -p pae-core -- --ignored

.PHONY: fixtures
fixtures: ## テスト用のメディアファイルを fixtures/ に生成する
	./scripts/make-fixtures.sh

.PHONY: fmt
fmt: ## コードを整形する
	cargo fmt

.PHONY: lint
lint: ## clippy と TypeScript の型チェックを実行する
	cargo clippy --all-targets
	cd crates/pae-app && npx tsc --noEmit

.PHONY: check
check: fmt lint test ## 変更後の標準検証 (整形 + lint + テスト) をまとめて実行する
