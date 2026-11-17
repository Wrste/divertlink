# DivertLink 开发与发布常用命令。
#
# 本机没有全局 Rust 时，以下目标会自动通过 scripts/env.sh 使用仓库内工具链
# （.toolchain/，见 scripts/env.sh）；CI 中直接使用系统 cargo。
#
#   make            # 同 make help
#   make check      # fmt-check + clippy + test + doc + deploy-check（与 CI 对齐）
#   make smoke      # 构建 debug 二进制并跑端到端冒烟测试
#   make release    # 可复现 release 构建（--locked）
#   make docker     # 本地构建容器镜像

SHELL := /bin/bash
.DEFAULT_GOAL := help

VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo 0.1.0)
IMAGE ?= ghcr.io/divertlink/divertlink
TAG ?= $(VERSION)

ifeq ($(shell command -v cargo 2>/dev/null),)
CARGO_RUN := bash -c 'set -e; . ./scripts/env.sh; exec cargo "$$@"' cargo
else
CARGO_RUN := cargo
endif

.PHONY: help fmt fmt-check clippy test test-all doc build release check ci clean \
        deploy-check smoke docker docker-multiarch docker-push install

help: ## 显示可用目标
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
		awk 'BEGIN { FS = ":.*?## " } { printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2 }'

fmt: ## 格式化代码（cargo fmt）
	$(CARGO_RUN) fmt --all

fmt-check: ## 检查格式（CI 用）
	$(CARGO_RUN) fmt --all -- --check

clippy: ## Clippy 严格检查（-D warnings，与 CI 参数一致）
	$(CARGO_RUN) clippy --locked --all-targets --all-features -- -D warnings

test: ## 运行测试（单元 + 集成，cargo test 默认目标）
	$(CARGO_RUN) test --locked

test-all: ## 运行全部 target 的测试
	$(CARGO_RUN) test --locked --all-targets

doc: ## 构建文档并把警告视为错误
	RUSTDOCFLAGS="-D warnings" $(CARGO_RUN) doc --locked --no-deps --all-features

build: ## Debug 构建
	$(CARGO_RUN) build --locked

release: ## Release 构建（--locked，可复现）
	$(CARGO_RUN) build --release --locked

check: fmt-check clippy test doc deploy-check ## 完整本地检查（与 CI 等价）

ci: check ## CI 等价检查

deploy-check: ## 校验部署资产（YAML/install.sh/compose/systemd/nginx 契约）
	bash deploy/check-assets.sh

console: build ## 启动内置图形控制台（本地 Web UI / PWA）
	$(CARGO_RUN) run --locked -- console --open

smoke: build ## 端到端冒烟测试（init -> mock 上游 -> gateway + agent -> curl 验证）
	./scripts/smoke.sh -b ./target/debug/divertlink

install: ## 从本仓库安装到 ~/.cargo/bin
	$(CARGO_RUN) install --path . --locked --force

clean: ## 清理构建产物
	$(CARGO_RUN) clean
	rm -rf target

docker: ## 构建当前平台容器镜像
	docker build \
		--label "org.opencontainers.image.title=DivertLink" \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--label "org.opencontainers.image.source=https://github.com/divertlink/divertlink" \
		-t $(IMAGE):$(TAG) .

docker-multiarch: ## 构建 amd64+arm64 镜像（本地验证，不推送）
	docker buildx build \
		--platform linux/amd64,linux/arm64 \
		--label "org.opencontainers.image.version=$(VERSION)" \
		-t $(IMAGE):$(TAG) .

docker-push: ## 构建并推送多架构镜像到镜像仓库
	docker buildx build \
		--platform linux/amd64,linux/arm64 \
		--label "org.opencontainers.image.version=$(VERSION)" \
		--push \
		-t $(IMAGE):$(TAG) .
