# polymarket_copy

Polymarket 跟单机器人（Rust）。链上实时监听目标钱包的成交，按比例自动跟单。
默认 **dry_run**（只记录、不真实下单）。

## 安装（一条命令）

```bash
curl -fsSL https://raw.githubusercontent.com/leosysd/polymarket_copy/main/install.sh | bash
```

它会自动装 Rust（如果没有）、拉代码、编译、并创建 `poly` 命令。
你还需要一个 Polygon 的 **WebSocket** RPC 地址（`wss://…`，[Alchemy](https://www.alchemy.com)
免费档即可），在菜单里填进去。

## 使用

输入 **`poly`** 打开交互式中文菜单，所有操作都在里面：

```bash
poly
```

菜单里能：填节点/私钥、加跟单地址、调跟单比例和滑点、切模拟/实盘、装/启停服务、看账本、**更新程序**。

> 先跑 **模拟**（默认）确认跟单逻辑没问题，再切实盘。开实盘需在「连接」里填私钥，
> API 凭证会自动派生；第一次用很小的跟单比例试。

### 同时多笔 / 只买入持有

- **合并下单**：目标在同一结果上、极短时间内（`aggregate_window_ms`，默认 400ms）打出的
  多笔成交会被合并成「一笔」再跟，避免小切片被 `min_order_usdc` 卡掉，也减少下单次数和
  同块多笔互相排队的延迟。设 `0` 关闭、逐笔跟。
- **只买入持有的目标**：把 `only_buys = true`，并按需用 `max_market_usdc` 给单个结果的
  累计下单额封顶（只加仓不减仓时的一道总量闸；token id 每个窗口都变，所以天然按窗口重置）。
  这两项都能在菜单「⚙️ 设置」里直接调。

## 更新

两种方式都行：

- 菜单里选 **⬆️ 更新程序**（git pull + 重新编译）
- 或重跑上面那条一键安装命令

## 卸载

```bash
poly        # 进菜单 → 服务 → 卸载服务（若装过服务）
rm -rf ~/polymarket_copy
sudo rm -f /usr/local/bin/poly /usr/local/bin/pmcopy
```

如果创建过 CLOB API key，记得去 Polymarket 吊销。

---

MIT 许可，按现状提供，不作担保。跟单有风险，可能亏损。
