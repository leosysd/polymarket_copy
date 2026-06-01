#!/usr/bin/env bash
# 一键安装：编译 pmcopy，并创建 `poly` 命令（输入 poly 即打开交互式菜单）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN=/usr/local/bin

echo "==> 编译 release 版本..."
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> 安装 pmcopy 到 $BIN/pmcopy"
sudo install -m 0755 "$ROOT/target/release/pmcopy" "$BIN/pmcopy"

echo "==> 创建 poly 启动器 -> $BIN/poly"
sudo tee "$BIN/poly" >/dev/null <<EOF
#!/usr/bin/env bash
# 在项目目录里打开 pmcopy 交互式菜单（这样能读到 config.toml / .env）。
cd "$ROOT" && exec "$BIN/pmcopy" menu "\$@"
EOF
sudo chmod 0755 "$BIN/poly"

echo
echo "完成。现在任何地方输入：  poly   就能打开菜单。"
