#!/usr/bin/env bash
# 一键安装 / 更新 Polymarket 跟单机器人。
#
# 全新安装（一条命令）：
#   curl -fsSL https://raw.githubusercontent.com/leosysd/polymarket_copy/main/install.sh | bash
#
# 装完后：输入  poly  打开交互式菜单。
set -euo pipefail

REPO="https://github.com/leosysd/polymarket_copy.git"
DEST="${PMCOPY_DIR:-$HOME/polymarket_copy}"
BIN=/usr/local/bin

echo "==> 检查 Rust..."
if ! command -v cargo >/dev/null 2>&1; then
  echo "    未装 Rust，正在安装..."
  curl -fsSL https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:$PATH"

if [ -d "$DEST/.git" ]; then
  echo "==> 已存在，更新源码 ($DEST)..."
  git -C "$DEST" pull --ff-only
else
  echo "==> 克隆到 $DEST ..."
  git clone "$REPO" "$DEST"
fi
cd "$DEST"

echo "==> 编译 release（首次可能要几分钟）..."
cargo build --release

# 首次准备配置文件（已存在则不覆盖）
[ -f config.toml ] || cp config.example.toml config.toml
[ -f .env ]        || cp .env.example .env

echo "==> 安装命令到 $BIN（需要 sudo）..."
sudo install -m 0755 "$DEST/target/release/pmcopy" "$BIN/pmcopy"
sudo tee "$BIN/poly" >/dev/null <<EOF
#!/usr/bin/env bash
cd "$DEST" && exec "$BIN/pmcopy" menu "\$@"
EOF
sudo chmod 0755 "$BIN/poly"

echo
echo "✅ 完成！输入下面这条打开菜单："
echo "      poly"
