# AGENTS.md

本文件定义了本项目的 Rust 开发规范与自动化流程。所有贡献者必须严格遵守，并在每次修改代码后及时更新本文件（如有新增规范或调整）。

---

## 1. Git 提交规范

- **范围**：每次提交应独立且完整地对应一个逻辑变更（如单一功能点、缺陷修复或配置调整），禁止混合多个不相关改动；按功能批次顺序组织，单次提交代码变动量建议控制在 300 行以内(仅建议，非强制，可适当突破)，避免大批量改动挤在同一条提交信息中。
- **格式**：`<type>: <中文描述>`
- **常用 type**：
  - `feat` – 新功能
  - `fix` – 修复 bug
  - `docs` – 文档更新
  - `style` – 代码格式（不影响逻辑）
  - `refactor` – 重构
  - `perf` – 性能优化
  - `test` – 测试相关
  - `build` – 构建系统或外部依赖变更
  - `ci` – CI 配置变更
  - `chore` – 杂项（如工具、配置等）
  - `revert` – 回退提交

示例：`feat: 添加用户登录接口`

---

## 2. Rust CI 标准（GitHub Actions）

确保 `.github/workflows/rust-ci.yml` 存在，内容如下：

```yaml
name: Rust CI

on:
  push:
    branches: [ "main", "master" ]
    paths:
      - "**.rs"
      - "**.proto"
      - "**/Cargo.toml"
      - "**/Cargo.lock"
      - ".rustfmt.toml"
      - ".clippy.toml"
      - "rust-toolchain.toml"
      - ".github/workflows/rust-ci.yml"
  pull_request:
    branches: [ "main", "master" ]
    paths:
      - "**.rs"
      - "**.proto"
      - "**/Cargo.toml"
      - "**/Cargo.lock"
      - ".rustfmt.toml"
      - ".clippy.toml"
      - "rust-toolchain.toml"
      - ".github/workflows/rust-ci.yml"

env:
  CARGO_TERM_COLOR: always

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: true

jobs:
  check:
    name: Check & Test
    runs-on: ubuntu-latest
    steps:
      - name: Checkout repository
        uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@master
        with:
          toolchain: "1.99.1"
          components: rustfmt, clippy

      - name: Show rustup info
        run: rustup show

      - name: Cache Cargo dependencies
        uses: Swatinem/rust-cache@v2

      - name: Check formatting
        run: cargo fmt --all -- --check

      - name: Run Clippy (lints)
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings

      - name: Check (fast compile)
        run: cargo check --workspace --all-features --verbose

      - name: Build the project
        run: cargo build --workspace --all-features --verbose

      - name: Run tests
        run: cargo test --workspace --all-features --verbose
```

---

## 3. Cargo.toml 配置

- 必须包含完整的包元数据（满足可发布到 [crates.io](https://crates.io) 的要求），例如：
  - `name`、`version`、`edition`、`authors`、`description`、`license`、`repository` 等。
- 依赖项必须**归类**，使用 `#` 注释说明每组依赖的用途。
- 每个依赖必须使用 `version = "x.y.z"` **锁定具体版本**（使用 `=` 号），不得使用范围限定符。
- 使用 `edition = "2024"` 以及环境中的 Rust 版本，例如:`rust-version = "1.99.1"`

示例结构：

```toml
[package]
name = "my_crate"
version = "0.1.0"
edition = "2024"
rust-version = "1.99.1"
authors = ["Your Name <email@example.com>"]
description = "A short description"
license = "MIT OR Apache-2.0"
repository = "https://github.com/your/repo"

# 核心依赖
[dependencies]
# 序列化
serde = { version = "=1.0.210", features = ["derive"] }

# 异步并发
tokio = { version = "=1.42.0", features = ["full"] }

# 开发依赖
[dev-dependencies]
# 基准测试
criterion = { version = "=0.5.1" }

# 编译优化配置
[profile.dev]
opt-level = 1
[profile.dev.package."*"]
opt-level = 3
```

---

## 4. 代码格式化（.rustfmt.toml）

项目根目录必须包含 `.rustfmt.toml`，内容如下：

```toml
edition = "2024"
style_edition = "2024"
max_width = 100
use_small_heuristics = "Max"
tab_spaces = 4
reorder_imports = true
reorder_modules = true
newline_style = "Auto"
match_block_trailing_comma = true
merge_derives = false
group_imports = "StdExternalCrate"
imports_granularity = "Module"
use_field_init_shorthand = true
```

所有代码必须通过 `cargo fmt --all -- --check` 检查。

---

## 5. Clippy 配置（.clippy.toml）

项目根目录必须包含 `.clippy.toml`，内容如下：

```toml
# ── Clippy Configuration ──
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 5
too-many-lines-threshold = 30
allow-unwrap-in-tests = true
msrv = "1.99.1"
```

所有代码必须通过 `cargo clippy --all-targets -- -D warnings` 检查，无警告。

---

## 6. README.md

- 每次完成任务后需及时更新 `README.md`，至少包含：
  - 项目简介
  - 构建与运行说明
  - 主要功能或使用示例
  - 贡献指南（引用本 AGENTS.md）

---

## 7. .gitignore

必须排除以下内容（示例）：

```
# Rust
/target/
**/*.rs.bk
*.pdb

# macOS
.DS_Store

# IDE
.vscode/
.idea/
*.swp
```

---

## 8. 项目结构

- **使用DDD+洋葱结构，严格遵循此结构开发**
- **禁止单`mod.rs`文件写入大量代码，代码较多时候将代码拆分到更小的带具体名称的代码文件中(如 utils.rs)**
- **保持单代码文件简洁和更细致的 crate 划分以加速增量编译**

---

## 9. 通用原则

- **保持本文件（AGENTS.md）更新**：每次修正代码或引入新规范后，请同步更新此文档。
- **所有变更**必须通过 CI 检查（格式、lint、构建、测试）。
- **版本锁定**：工具链版本统一使用环境中的版本，但需>=1.99.1（如 CI 和 clippy 配置所示）。
- **遵循设计**：改动必须遵循原有结构设计，不得私自添加和修改，除非用户发出明确重构指令。
- **后续开发追加 AGENTS.md 内容**：写入第 10 章节。
- **测试**：编写单元测试，如果已经安装`cargo-llvm-cov`则检测测试覆盖率>=80%。
- **隐私**：任何文件/代码/图片/视频，应注意避免隐私泄露。(只关注项目文件本身，不关注 git 等外部工具)
- **git hooks**：如有`.pre-commit-config.yaml`则自动完成钩子检查建议内容

---

**本文件是项目的“开发宪法”，所有 pull request 和代码审查均应参照其内容。**

## 10. 其他追加内容

### 10.1 黑盒测试是上线硬门槛

**任何新功能、协议改动、性能优化，未通过全方位黑盒测试不得合并、不得发布。**

#### 背景：为什么把黑盒测试提到门槛级

本项目的每一轮黑盒测试扩充，都稳定地挖出了新缺陷，至今已累计发现并修复 28 个真实问题，
其中 7 个会导致线上连接永久卡死、panic 或静默丢数据，另有 4 个会让吞吐严重劣化或配置失效。
下文只是其中一部分典型样本，完整台账与回归测试索引见 10.3 节。

| 缺陷 | 表现 | 只有黑盒才暴露的原因 |
| --- | --- | --- |
| 重复段不回 ACK | ACK 丢失后收发双方永久不对账，传输卡在半途 | 需要「链路丢 ACK + 发送方重传」两个事件同时发生在真实时序里 |
| ACK 判重上界用 snd_una | 首段 sn=0 永远无法被确认 | 需要从空连接开始连续多段传输才走得到边界 |
| 接收窗口上界 u64 展开 | SN 回绕点另一侧的合法段被判为窗口外 | 需要序号真的跨过 `u32::MAX` |
| 重传段参与 RTT 采样 | `rx_rttval` 比 `rx_srtt` 大一个数量级，RTO 退避到十几秒 | 需要 20% 丢包连续跑几十轮才能积累出来 |
| `frg + 1` u8 溢出 | 对端发 `frg=255` 的孤立分片即 panic | 需要构造畸形输入 |
| `check()` 等待探测时返回 current | 调用方按 flush 间隔空转，探测退避失效 | 需要零窗口态持续一段时间 |
| 死链阈值未覆盖 | 长断网后行为不确定 | 需要仿真链路黑洞 |
| `parse_una` 无条件回写 | 回绕点附近 `snd_una` 与 `snd_buf` 脱节 | 需要乱序 + 回绕组合 |
| una 越过 `snd_nxt` 被照单全收 | 在途段被清空、`snd_una` 冲出已发送范围，之后真实 ACK 全部判为过期，数据静默丢失 | 需要链路比特翻转或伪造包与正常传输交织 |
| 窗口额度未扣除在途段 | 每轮 flush 再塞满一个窗口，在途量随轮次无界增长，对端整片丢弃 | 需要对端接收窗口小于发送窗口且链路有延迟 |

这些缺陷的共同点是：**单元测试覆盖不到，代码走查看不出，只有把真实实例放进
真实时序、真实丢包、真实数据量里长时间对拍才会浮现。** 因此黑盒测试不是补充，
而是上线前的最后一道闸门。

#### 强制要求

新功能提交前，必须在 `tests/` 下补齐对应黑盒用例，并至少覆盖以下五个维度：

1. **协议不变式**：字节精确、消息边界、窗口上限、无静默丢数据。
   任何改动都要先问「哪条不变式可能被破坏」，并为它补一条断言。
2. **配置组合**：新配置项必须进入 `tests/common/stress.rs::config_matrix()`，
   保证与既有配置组合都能互连。非对称配置（两端参数不同）必须单独覆盖。
3. **真实网络形态**：丢包、乱序、重复、抖动、延迟、**黑洞断网**、断续闪断、长肥管道。
   涉及超时/重传/窗口/退避的改动，必须包含 `Net::with_blackout` 用例。
4. **发恶意输入**：任意字节、截断头、伪造长度、未知命令、超大窗口、畸形分片链、
   伪造 conv、ACK 洪水、重复轰炸。解析路径的任何改动都必须过这一关。
5. **规模与并发**：大数据量（≥1MB）、长时间（≥1000 轮）、高连接数（≥20 客户端）、
   SN 回绕。涉及队列、序号、状态机的改动必须补规模用例。

#### 黑盒用例的书写规范

- **只用公开 API**：`Kcp::new` / `set_output` / `send` / `recv` / `input` / `update` /
  `check` / `peeksize`。允许直接读写文档化的公开字段（如 `rcv_nxt`、`snd_buf`），
  但不得调用私有函数，否则用例会跟着实现一起漂移，失去发现缺陷的能力。
- **确定性**：随机源必须来自 `Rng::new(固定种子)`。失败时能靠同一 seed 复现，
  是黑盒用例可用的前提。禁止 `thread_rng`、`SystemTime`、真实 sleep。
- **虚拟时钟**：一律用 `Sim` 的虚拟毫秒时钟推进，不依赖真实时间，
  这样「跑一分钟」只花毫秒级 CPU，可以放心把时长拉长。
- **不放宽断言换通过**：用例失败先判断是产品缺陷还是用例前提错误。
  若是产品缺陷，改产品；若是用例前提错误（如消息模式下分片数超过 `rcv_wnd`
  本就按协议阻塞、`snd_buf` 语义被误解），修正用例并在提交信息里写明原因。
  **严禁删除断言、缩小覆盖范围或改成恒真来让测试变绿。**
- **禁止无界等待**：所有循环必须有明确的 tick 上限，超时即断言失败，
  不允许让用例挂死。
- **属性测试**：`tests/property.rs` 用 proptest 在参数空间内搜索并收缩反例，
  用来覆盖手写场景想不到的输入组合。失败时 proptest 会把最小反例写入
  `tests/property.proptest-regressions`，该文件必须随修复一起提交，
  使同一反例在后续每轮 CI 中都会重放。属性测试的 `cases` 必须显式设定，
  不要依赖默认 256 例把时长推高。
- **验收时长**：整套黑盒用例应在 30 秒内跑完（当前约 2 秒）。超出说明仿真规模失控，
  应压缩数据量而不是砍掉场景。

#### 验收命令

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo llvm-cov --workspace --all-features --summary-only   # 覆盖率需 >= 80%
```

四项全绿才允许提交。覆盖率门槛不替代黑盒场景覆盖：行覆盖率高不代表
协议不变式被验证过，两者必须同时满足。

#### 新增黑盒文件的流程

1. 在 `tests/` 下新建文件，并在 `Cargo.toml` 里补 `[[test]]` 块
   （本项目 `autotests = false`，不补就不会被执行，不要依赖自动发现）。
2. 公共辅助放进 `tests/common/`，不要在多个测试文件里复制粘贴工具代码。
3. 在 `README.md` 的「黑盒测试」表里补一行，说明该文件覆盖的关注点。
4. 若用例挖出产品缺陷，**先提交修复**，再提交用例，并在提交信息里写清缺陷成因。

### 10.2 原版实现只作参考，不是正确性依据

`skywind3000/kcp` 的 C 实现是本项目的重要参考，但它**并非完美无瑕**，
同样存在设计缺陷、已知问题与历史包袱（序号比较、窗口边界、探测退避、
心跳与死链判定等处都有可讨论空间，部分问题在上游 issue 中长期未解决）。

因此：

1. **不得以「和原版一致」作为正确性论据**。判断一段逻辑是否正确，
   依据是协议不变式、状态机自洽性和可验证的行为断言，而不是「C 版也这么写」。
   对齐原版只是发现差异时的**起点**，之后必须独立判断这个差异是该修的偏差，
   还是该保留的改进。
2. **遇到原版本身的错误要明确记录**。若确认原版行为有缺陷，本项目可以选择
   不跟随，但必须在缺陷登记表里写清「原版如此、本项目为何不同」，
   并在对应位置补上能体现差异的用例，避免后人「照着原版改回去」。
3. **与 C 版互通时区分两类差异**。线格式（包头布局、命令字、序号语义、
   `una` 推进规则）必须保持一致，否则无法互通；而 RTO 策略、退避曲线、
   拥塞窗口起效方式等内部策略属于实现自由，按本项目自己的判断取舍。
4. **回归验证同样适用**：拿原版行为当基准写断言前，先确认该行为本身正确。
   把原版的缺陷固化成断言，等于把缺陷永久焊死在代码里。

### 10.3 缺陷登记表与回归测试索引

本节是**缺陷台账**：每个曾经发现并修复的缺陷，都必须在此登记成因、修复位置与
回归测试位置。新增缺陷时按 `BUG-<序号>` 顺延编号，并补齐对应行。

修改协议核心（`src/core/kcp.rs`）前，先查本表，确认不会让已修复的缺陷复发；
回归时优先跑下表列出的用例。

#### 一、缺陷总表

| 编号 | 缺陷 | 层面 | 后果 | 修复位置 |
| --- | --- | --- | --- | --- |
| BUG-1 | `cwnd` 初值为 0 | 发送准入 | 默认配置下数据永不发出，连接完全不可用 | `kcp.rs` 构造函数 |
| BUG-2 | `parse_ack` 用 `>= 0` 匹配分段 | RTT 计算 | RTT 始终取第一段的发送时刻，RTO 计算完全错误 | `kcp.rs::parse_ack` |
| BUG-3 | `snd_una` 初始化后从不更新 | 确认推进 | 每个包都判定「有新确认」，并连带使 BUG-7 失效 | `kcp.rs::parse_una` |
| BUG-4 | `parse_una` 无条件回写 `snd_una` | 确认推进 | 乱序/过期 una 把它拉回去，与 `snd_buf` 脱节 | `kcp.rs::parse_una` |
| BUG-5 | 超时重传与快速重传是两个独立 `if` | 重传 | 同一段被重发两次，重复占用带宽 | `kcp.rs::flush` |
| BUG-6 | `rmt_wnd` 初值非 0 | 发送准入 | 首包前就按「对端窗口已满」停发 | `kcp.rs` 构造函数 |
| BUG-7 | `parse_fastack` 晚于 `parse_una` | 快速重传 | 快速重传完全失效 | `kcp.rs::input`（现按去重后的 ACK 序列号计数） |
| BUG-8 | ACK 处理用弹出-重压模式 | 性能 | 每次 ACK 触发一次分配，长连接下抖动明显 | `kcp.rs::parse_ack` |
| BUG-9 | 重复 Push 段不回 ACK | 协议不变式 | **ACK 丢失后收发双方永久不对账，传输卡死** | `kcp.rs::input` 的 `Push` 分支 |
| BUG-10 | ACK 判重上界用 `snd_una` | 协议不变式 | **首段（sn=0）永远无法被确认** | `kcp.rs::input` 的 `Ack` 分支 |
| BUG-11 | 窗口通告用 `rcv_wnd` 而非剩余空间 | 流控 | 接收端已满时仍通告满窗，对端持续超发 | `kcp.rs::flush` |
| BUG-12 | `ts_lastack` 从未被赋值 | 拥塞控制 | 「每 ACK 周期最多增长一次」门限恒真，`cwnd` 每轮 flush 都涨 | `kcp.rs::parse_ack` / `parse_una` |
| BUG-13 | `ssthresh` 初值等于 `cwnd` 初值 | 拥塞控制 | **慢启动被整段跳过，`normal` 模式 `cwnd` 卡在 1，吞吐退化为 1 段/RTT** | `kcp_const_config.rs::IKCP_THRESH_INIT_WND` |
| BUG-14 | `fastresend == 0` 未做特判 | 拥塞控制 | `fastack >= 0` 恒真，干净链路上每轮都误判丢包并把 `cwnd` 砸回 1 | `kcp.rs::flush` 快速重传分支 |
| BUG-15 | `peeksize` 用 `frg + 1` 直接相加 | 内存安全 | 对端发 `frg=255` 即 u8 溢出 panic | `kcp.rs::peeksize` |
| BUG-16 | `check()` 等待探测时返回 `current` | 时序 | 调用方按 flush 间隔空转，探测退避形同虚设 | `kcp.rs::check` |
| BUG-17 | 重传段参与 RTT 采样 | RTT 计算 | `rx_rttval` 比 `rx_srtt` 大一个数量级，RTO 退避到 10 秒以上 | `kcp.rs::parse_ack`（Karn 算法） |
| BUG-18 | 接收窗口上界用 u64 展开比较 | 协议不变式 | **SN 回绕点另一侧的合法段被判为窗口外丢弃** | `kcp.rs::input` 的 `Push` 分支 |
| BUG-19 | ACK 按 sn 精确删除与 una 推进并存 | 确认语义 | **sn 与 una 矛盾时会删除对端未收到的段，静默丢数据** | `kcp.rs::parse_ack`（改为只做 RTT 采样） |
| BUG-20 | 零窗口探测退避从 `interval` 起步 | 窗口探测 | 零窗口连接被 WASK 每 20ms 轰炸，`IKCP_PROBE_INIT` 定义了从未被使用 | `kcp.rs::flush` 的探测分支 |
| BUG-21 | `nodelay` 字段只存不用 | 配置语义 | 把 `nodelay` 改成 true 行为完全不变，低延迟配置形同虚设 | `kcp.rs` 构造 / `flush` |
| BUG-22 | `IKCP_DEADLINK` 为 100，C 原版为 20 | 死链判定 | 弱网下要重传 100 次才判死链，坏连接长时间占用资源 | `kcp_const_config.rs::IKCP_DEADLINK` |
| BUG-23 | `IKCP_PROBE_INIT` 为 7000 且退避按 2 倍 | 窗口探测 | 与 C 原版（5000、1.5 倍）不一致，探测流量偏高 | `kcp_const_config.rs` / `kcp.rs::flush` |
| BUG-24 | `input` 无条件接受越过 `snd_nxt` 的 una | 确认语义 | **伪造/翻转的 una 清空在途段并把 `snd_una` 推出已发送范围，之后真实 ACK 全被判过期，数据静默丢失** | `kcp.rs::input` 的 una 判定 |
| BUG-25 | `flush` 每轮按整个窗口放行新段，不扣除在途段 | 流控 | **在途量随 flush 次数无界增长，远超对端接收窗口，引发整片丢弃与重传风暴；对端未响应时每轮再塞一段探路** | `kcp.rs::flush` 的准入额度 |
| BUG-26 | 同轮有 ACK 待发时探测让路 | 窗口探测 | `IKCP_ASK_SEND` 长期挂起，`check()` 持续返回 `current`，零窗口期间调用方全速空转 | `kcp.rs::flush` 的 `need_wask` |
| BUG-27 | 构造期不校验 `mtu` 与收发窗口 | 配置校验 | `mtu < 28` 下溢 panic、`mtu == 28` 时 `mss = 0` 触发除零 panic、窗口为 0 的连接永久不可用 | `kcp.rs` 构造函数 |
| BUG-28 | `check()` 在零窗口时无条件返回 `ts_probe` | 时序 | 零窗口时 `snd_buf` 仍有在途段等待重传，其期限远早于 5000ms 的探测时刻；调用方按返回值调度会睡过重传期限，连接被拖到探测退避的粒度 | `kcp.rs::check` 的零窗口分支 |
| BUG-34 | 非法分片链被当作完整消息交付 | 消息边界 | 缺少终止分片或倒数不连续时仍返回成功 | `kcp.rs::peeksize` 与 `recv` |
| BUG-35 | Stream 每个 UDP 包只交付一条消息 | 应用投递 | 乱序恢复后剩余消息滞留，直到收到下一包 | `stream.rs` 的接收循环 |
| BUG-36 | Stream 接收通道满时阻塞协议任务 | 背压 | 停止发送、确认和定时更新，可能造成双向死锁 | `stream.rs` 客户端任务 |
| BUG-37 | close/Drop 不取消后台及会话任务 | 生命周期 | 关闭后仍收发、端口与任务泄漏 | `stream.rs` 的 close/Drop |
| BUG-38 | Stream 发送超大消息只记录后台错误 | 应用投递 | 调用方收到成功但消息被丢弃 | `kcp.rs::validate_send_size`、`stream.rs` 发送入口 |
| BUG-40 | 文件示例先截断目标再解析元数据，且发送端等待不存在的回复 | 文件安全 | 畸形输入破坏原文件或使发送端无限等待 | `examples/file-transfer.rs` |
| BUG-39 | 监听器无效首包创建会话且无数量和空闲限制 | 资源管理 | 外部来源可持续累积会话和任务 | `stream.rs` 的监听器准入和清理 |
| BUG-44 | 客户端固定使用 IPv4 socket | 地址族 | 无法连接 IPv6 地址 | `stream.rs::connect_with_config` |
| BUG-43 | 零更新间隔触发任务 panic，任务退出后连接状态仍正常 | 生命周期 | 配置错误和后台故障被隐藏 | `stream.rs` 构造及连接状态 |
| BUG-33 | 报文实际载荷超出声明长度 | 输入解析 | 声明范围外的字节被交付给应用 | `kcp.rs::input` 与 `packet.rs::from_header_and_data` |
| BUG-31 | 接收队列和乱序缓冲中的重复段不补 ACK | 确认语义 | 首个 ACK 丢失后，发送方需等待应用读取才能恢复 | `kcp.rs::input` 的 Push 分支 |
| BUG-30 | `check()` 返回的重传期限早于定期刷新 | 时序 | 到期调用 `update()` 不重传，事件循环在同一时刻自旋 | `kcp.rs::update` 与 `check` |
| BUG-32 | 取消发送时移走回调并提前清空确认 | 取消安全 | 后续更新丢失输出回调和待发确认，部分发送状态不一致 | `kcp.rs::send_packet` 与 `flush` |
| BUG-41 | RTT 方差只累加不衰减 | 重传时序 | 有界抖动下 RTO 持续增长 | `kcp.rs::parse_ack` |
| BUG-42 | 快速重传按累计 una 而非较高 ACK 计数 | 重传时序 | 乱序 ACK 无法提前恢复丢失的段 | `kcp.rs::input`、`parse_fastack` |
| BUG-29 | 空闲刷新驱动拥塞窗口增长 | 拥塞控制 | 无确认时窗口持续增长，最终整数溢出 | `kcp.rs::input` 的确认推进分支 |
| BUG-45 | 库无条件声明全局 mimalloc | 库集成 | 下游自选全局分配器时编译失败，关闭默认 feature 也无法避开 | `src/lib.rs`、`Cargo.toml` 移除分配器声明和依赖 |
| BUG-46 | normal/fast 超时退避语义反转，期限使用旧 RTO | 重传时序 | 普通模式不累加、快速模式翻倍，连续丢包恢复不符合配置契约 | `kcp.rs::flush`：普通模式加 `max(rto, rx_rto)`，快速模式加 `rto/2`，饱和累加并限幅后安排下一期限 |
| BUG-47 | 客户端 output 忽略 UDP send 的内层错误 | 高层输出 | 系统拒绝的 UDP 输出被报成成功，协议状态和等待结果失真 | `stream.rs::send_udp` 保留 socket 错误，后台输出失败退出并唤醒等待者 |

其中 BUG-9、BUG-10、BUG-18、BUG-19、BUG-24、BUG-25、BUG-27 属于「线上会永久卡死、
panic 或静默丢数据」的严重级缺陷，BUG-13、BUG-14、BUG-17、BUG-26 属于「不丢数据但
吞吐或 CPU 严重劣化」的性能级缺陷。

#### 二、回归测试索引

**严重级缺陷（卡死/丢数据）**

| 编号 | 回归测试 | 位置 |
| --- | --- | --- |
| BUG-9 | `duplicate_segment_triggers_ack` | `tests/timing.rs:18` |
| BUG-9 | `ack_loss_does_not_stall_connection` | `tests/timing.rs:38` |
| BUG-31 | `unread_and_out_of_order_duplicates_are_acknowledged`、`lost_ack_recovers_before_application_reads` | `tests/timing.rs` |
| BUG-10 | `ack_is_per_segment`（已按 una 驱动语义重写） | `tests/timing.rs` |
| BUG-10 | `sender_fully_acknowledged_after_delivery` | `tests/timing.rs:66` |
| BUG-18 | `sequence_wraparound_is_transparent` | `tests/soak.rs:108` |
| BUG-18 | `wraparound_with_bidirectional_window_span` | `tests/soak.rs` |
| BUG-18 | `wraparound_mixed_with_retransmission` | `tests/soak.rs` |
| BUG-18 | `stale_sn_not_redelivered` | `tests/malformed.rs:101` |
| BUG-4 | `test_parse_una_never_moves_backward` | `src/core/kcp.rs` 的 `mod tests` |
| BUG-4 | `reordered_delivery_is_reassembled` | `tests/lossy.rs:64` |
| BUG-19 | `ack_sn_must_not_delete_beyond_una` | `tests/timing.rs` |
| BUG-19 | `ack_without_una_does_not_shrink_queue` | `tests/timing.rs` |
| BUG-19 | `test_parse_ack_retain` | `src/core/kcp.rs` 的 `mod tests` |
| BUG-24 | `una_ahead_of_snd_nxt_must_not_discard_unacked_segments` | `tests/adversarial.rs` |
| BUG-24 | `valid_una_still_acknowledges_after_forward_una_rejected` | `tests/adversarial.rs` |
| BUG-24 | `forward_una_flood_does_not_kill_live_connection` | `tests/adversarial.rs` |
| BUG-25 | `in_flight_segments_never_exceed_peer_receive_window` | `tests/adversarial.rs` |
| BUG-25 | `unknown_peer_admits_only_one_probe_segment` | `tests/adversarial.rs` |
| BUG-25 | `small_receive_window_bounds_in_flight` | `tests/property.rs` |
| BUG-27 | `degenerate_configs_are_rejected` | `tests/adversarial.rs` |
| BUG-27 | `arbitrary_config_is_rejected_or_usable` | `tests/property.rs` |
| BUG-27 | `minimum_usable_config_transfers` | `tests/adversarial.rs` |

**性能级缺陷（吞吐劣化）**

| 编号 | 回归测试 | 位置 |
| --- | --- | --- |
| BUG-12 | `test_parse_ack_sets_ts_lastack` | `src/core/kcp.rs:1610` |
| BUG-12 | `test_parse_una_sets_ts_lastack` | `src/core/kcp.rs:1645` |
| BUG-12 | `cwnd_grows_at_most_once_per_ack_cycle` | `tests/congestion.rs:150` |
| BUG-13 | `test_ssthresh_init_enables_slow_start` | `src/core/kcp.rs:1654` |
| BUG-13 | `ssthresh_init_allows_slow_start` | `tests/congestion.rs:80` |
| BUG-13 | `cwnd_grows_on_clean_link` | `tests/congestion.rs:40` |
| BUG-13 | `slow_start_increments_each_round` | `tests/congestion.rs:62` |
| BUG-14 | `test_fastresend_zero_does_not_trigger_fast_retransmit` | `src/core/kcp.rs:1666` |
| BUG-14 | `fastresend_zero_disables_fast_retransmit` | `tests/congestion.rs:92` |
| BUG-14 | `fastresend_disabled_still_completes` | `tests/retransmit.rs:31` |
| BUG-14 | `fastresend_variations_are_correct` | `tests/matrix.rs:88` |
| BUG-17 | `test_retransmitted_segment_skips_rtt_sample` | `src/core/kcp.rs:1327` |
| BUG-17 | `queues_stay_bounded_over_time` | `tests/soak.rs:181` |
| BUG-17 | `hostile_network_still_converges` | `tests/soak.rs:126` |
| BUG-41 | `bounded_rtt_jitter_and_spike_decay` | `tests/timing.rs` |
| BUG-42 | `distinct_reordered_acks_trigger_fast_retransmit_before_rto` | `tests/timing.rs` |
| BUG-12/13/14 | `loss_halves_ssthresh_and_resets_cwnd` | `tests/congestion.rs:121` |
| BUG-12/13/14 | `modes_have_comparable_throughput` | `tests/congestion.rs:178` |
| BUG-8 | `bulk_transfer_one_megabyte` | `tests/soak.rs:18` |
| BUG-26 | `probe_is_not_swallowed_by_pending_ack` | `tests/adversarial.rs` |
| BUG-26 | `stalled_receiver_recovers_with_bounded_probe_traffic` | `tests/torture.rs` |
| BUG-25 | `zero_window_does_not_inflate_send_buffer` | `tests/torture.rs` |

| BUG-37 | `closed_stream_rejects_try_operations`、`listener_close_or_drop_releases_port_and_rejects_io`、`stream_close_or_drop_releases_socket` | `tests/stream_api.rs` |

**正确性缺陷**

| 编号 | 回归测试 | 位置 |
| --- | --- | --- |
| BUG-1 | `test_cwnd_init_nonzero` | `src/core/kcp.rs:1282` |
| BUG-1 | `test_default_mode_can_send` | `src/core/kcp.rs:1288` |
| BUG-2 | `test_parse_ack_rtt_correct` | `src/core/kcp.rs:1303` |
| BUG-2 | `test_parse_ack_nsnd_buf_integrity` | `src/core/kcp.rs:1126` |
| BUG-3 | `test_snd_una_updated` | `src/core/kcp.rs:1345` |
| BUG-5 | `test_no_dup_retransmit` | `src/core/kcp.rs:1447` |
| BUG-5 | `test_fast_retransmit_emits_again` | `tests/timing.rs:159` |
| BUG-6 | `test_rmt_wnd_zero_at_init` | `src/core/kcp.rs:1367` |
| BUG-7 | `test_fastack_incremented` | `src/core/kcp.rs:1374` |
| BUG-7 | `fast_retransmit_recovers_lost_segment` | `tests/retransmit.rs:17` |
| BUG-11 | `window_advertisement_follows_queue` | `tests/timing.rs:142` |
| BUG-11 | `sender_never_exceeds_peer_window` | `tests/fuzz_transfer.rs:98` |
| BUG-11 | `receiver_never_exceeds_own_window` | `tests/fuzz_transfer.rs:137` |
| BUG-11 | `probe_does_not_suppress_ack` | `tests/window.rs` |
| BUG-15 | `oversized_payload_parsed_safely` | `tests/malformed.rs:206` |
| BUG-15 | `stream_mode_accepts_large_message` | `tests/window.rs:66` |
| BUG-16 | `check_waits_for_probe_deadline` | `tests/timing.rs:222` |
| BUG-16 | `window_probe_is_emitted_when_peer_window_closed` | `tests/window.rs:86` |
| BUG-20 | `probe_backoff_starts_at_probe_init` | `tests/window.rs` |
| BUG-20 | `test_window_probe_backoff` | `src/core/kcp.rs` 的 `mod tests` |
| BUG-20 | `long_zero_window_still_probes_and_resumes` | `tests/window.rs` |
| BUG-21 | `nodelay_changes_min_rto` | `tests/matrix.rs` |
| BUG-21 | `nodelay_affects_first_retransmit_deadline` | `tests/matrix.rs` |
| BUG-22 | `default_dead_link_matches_ikcp` | `tests/retransmit.rs` |
| BUG-23 | `probe_backoff_starts_at_probe_init` | `tests/window.rs` |
| BUG-28 | `check_prefers_retransmit_deadline_over_probe` | `tests/timing.rs` |
| BUG-34 | `malformed_fragment_chain_reports_error_and_preserves_next_message` | `tests/malformed.rs` |
| BUG-35/36 | `listener_delivers_all_messages_released_by_gap`、`client_delivers_all_messages_released_by_gap`、`client_backpressure_keeps_protocol_progress_and_drains_messages` | `tests/stream_api.rs` |
| BUG-38 | `oversized_messages_are_rejected_before_enqueue` | `tests/stream_api.rs` |
| BUG-40 | `file_roundtrips_and_invalid_input_preserves_destination` | `examples/file-transfer.rs` |
| BUG-39 | `listener_limits_and_reclaims_sessions` | `tests/stream_api.rs` |
| BUG-44 | `loopback_address_families_and_hostname` | `tests/stream_api.rs` |
| BUG-43 | `zero_update_interval_is_rejected`、`stopped_worker_reports_disconnection` | `tests/stream_api.rs`、`src/stream.rs` |
| BUG-33 | `mismatched_payload_lengths_leave_connection_unchanged` | `tests/malformed.rs` |
| BUG-30 | `check_deadline_executes_due_retransmission`、`pending_ack_is_not_delayed_until_zero_window_probe` | `tests/timing.rs` |
| BUG-32 | `cancelled_output_retains_callback_and_pending_ack`、`cancelled_batch_keeps_successful_segment_state` | `tests/timing.rs` |
| BUG-29 | `idle_updates_do_not_grow_congestion_window`、`congestion_window_grows_only_on_new_confirmation_and_stays_bounded` | `tests/congestion.rs` |
| BUG-45 | 集成测试 crate 显式声明 System 全局分配器，默认与无默认 feature 均须编译通过 | `tests/api_contract.rs`、Rust CI 与 Release 工作流 |
| BUG-46 | `normal_timeout_backoff_uses_new_deadline`、`fast_timeout_backoff_uses_new_deadline`、`timeout_backoff_is_bounded_and_obeys_dead_link`、`timeout_and_fastack_are_exclusive_and_ack_recovery_preserves_rtt`、`backoff_modes_deliver_on_clean_burst_and_blackout_links` | `tests/retransmit.rs` |
| BUG-47 | `udp_output_preserves_socket_error`（65508 字节 UDP 确定报错）、`stopped_stream_worker_wakes_confirmation_waiter` | `src/stream.rs` |

#### 三、维护规则

1. **修缺陷必须先补测试**：任何一个 `fix:` 提交，都要在同一批次里带上能复现该缺陷的
   回归用例。用例必须在修复前失败、修复后通过。
2. **验证用例真的有效**：提交前手动回退修复，确认新用例会失败。
   「修复后通过」不足以证明用例有效——断言写得太松的用例在缺陷存在时也会通过，
   这类用例是负资产。本项目就出现过 `cwnd > start` 在 `cwnd` 卡于 1 时仍然通过的情况。
3. **行号会漂移**：本表的行号以登记时刻为准，代码改动后行号可能偏移，
   以函数名为主索引、行号为辅。找不到行号时按函数名搜索。
4. **性能缺陷同样要断言**：只断言「数据完整送达」会漏掉所有吞吐退化类缺陷。
   涉及拥塞控制、窗口、缓存的改动，必须同时断言行为特征（增长节奏、收缩幅度、
   tick 上限），参考 `tests/congestion.rs` 的写法。
5. **禁止只放宽断言**：调整阈值前必须说明为何原阈值不合理。
   「跑不过就把阈值调大」等于删除该用例。

### 10.4 注释规范

注释只解释**意图、边界和取舍**，不复述代码字面意思。

- **允许**：文档注释（`///`、`//!`）描述公开 API 的语义、参数约束、错误条件；
  关键路径上的解释型注释说明「为什么这样写」——隐藏约束、微妙不变量、
  反直觉的取舍、特定写法规避的陷阱。
- **禁止**：复述代码在做什么（如 `// 更新远端窗口大小` 后面紧跟
  `self.rmt_wnd = header.wnd;`）。
- **严禁**：在代码里写缺陷修复说明。不写 `// 修复 BUG-XX`、
  `// BUG-XX 回归`、`// 原实现……现在改为……`、`// C 原版如何、Rust 原译如何`。
  缺陷的编号、成因、修复位置与回归用例统一登记在 10.3 节的缺陷台账里，
  代码中只保留与缺陷无关、但当前仍然成立的约束说明。
- 测试用例的文档注释描述**被验证的行为**，而不是它修复了哪个缺陷。

### 10.5 发布流程

推送 `v*` 标签即触发发布，三个环节严格串行，前一个失败则后一个不执行：

```
ci 校验  →  GitHub Release  →  cargo publish
```

1. **ci 校验**（`release.yml` 的 `verify` job）：在标签提交上跑格式、lint 与全量测试，
   不通过则不创建 Release，也不会发布到 crates.io。
2. **GitHub Release**：内容读取 `.version/RELEASE-<tag>.md`（也接受 `.version/<tag>.md`）。
   文件缺失或为空时写入默认提示，引导用 `git log` 确认变动，不阻断发布。
3. **cargo publish**（`release.yml` 的 `publish` job）：Release 创建成功后执行
   `cargo publish`，Environment 为 `release`，使用 crates.io 的 OIDC 可信发布，
   无需长期令牌。

发布步骤必须写在 `release.yml` 内，不能拆到独立工作流：crates.io 可信发布按 JWT 中的
工作流文件名与环境校验，本仓库绑定的是 `release.yml` + `release`。已实测三条串接路径
都不可行，改动前不要再试：

- `workflow_run` 事件被 crates.io 直接拒绝（400，不支持该触发方式）。
- `GITHUB_TOKEN` 创建的 Release 不会触发 `release: published` 事件。
- 用 `workflow_call` 复用其它工作流时，JWT 上报的仍是调用方文件名。

发布新版本时按顺序做三件事：改 `Cargo.toml` 与 `Cargo.lock` 的版本号、
补 `.version/RELEASE-<tag>.md`、打标签并推送。版本说明面向使用者，
写「变了什么、需要注意什么」，不复述提交清单。

### 10.6 文档示例一致性

README 的 Echo 代码与 `examples/echo-server.rs`、`examples/echo-client.rs` 保持一致；`readme_echo_examples_match_compiled_sources` 检查内容，CI 编译示例。修改示例时同步更新 README。

### 10.7 黑盒边界与恢复覆盖

以下是行为覆盖索引，不代表新增产品缺陷；不得将内部实现行为或未经实测的候选当作正确性依据。

| 测试 | 关注点 |
| --- | --- |
| `smoke::learned_conversation_completes_request_and_reply` | 首包学习 conv 后双向消息闭环 |
| `transfer::maximum_message_survives_loss_and_blackout` | 最大 255 分片在 MTU 29/128/1400、丢包、乱序、重复与黑洞组合下精确交付 |
| `transfer::rejected_send_preserves_pending_and_subsequent_messages` | 超限发送失败不改变待发消息，后续合法发送可恢复 |
| `transfer::repeated_peek_and_incomplete_recv_preserve_message_boundaries` | 重复查看和不完整读取不消费消息 |
| `timing::output_error_mid_batch_recovers_data_and_acknowledgements` | 数据与 ACK 输出中途报错后重试，不漏发或重复成功输出 |
| `malformed::malformed_ack_cannot_discard_pending_messages` | 畸形确认不能消耗在途数据，之后真实交付仍完成 |
| `malformed::rejected_first_packet_does_not_choose_conversation` | 解析失败的首包不能改变 conv，合法首包仍可建立会话 |
| `stream_api::nonblocking_queue_full_then_recovery_preserves_messages` | 非阻塞发送队列满后恢复，混合长度消息精确回显 |
| `stream_api::cancelled_pending_receives_preserve_subsequent_messages` | 客户端与监听器取消 pending recv 后仍能接收 |
| `stream_api::full_listener_keeps_active_session_across_cleanup_cycles` | 会话满额时现有活跃会话跨回收周期持续可用 |
| `stream_api::shared_receive_backlog_preserves_other_session_progress` | 接收积压不阻止另一会话确认、回复，恢复读取后完整排空 |
| `stream_api::foreign_conversation_does_not_replace_active_session` | 同源地址外来 conv 不污染活动会话 |

第 10.1 节的虚拟时钟规则适用于协议仿真；高层 Stream/Listener 的真实 UDP 测试允许本机回环及有界超时，不用真实 sleep 堆等待，不将公网或跨平台行为视为已验证。

`udp_chaos::sustained_loopback_load_is_byte_exact` 覆盖 1/8/20 客户端持续回显、精确内容与会话隔离，并输出吞吐及 RTT 分位数。固定种子用于载荷，不代表操作系统调度时间确定；性能无可靠 SLO 时只作观测，不设置任意速度阈值。测量前须暖机，失败与超时不得计入成功数据，后台任务必须清理。

本轮测试矩阵、命令、实测结果及覆盖率工具限制见 `docs/blackbox-2026-11-03.md`。

### 10.8 全局分配器由应用选择

库不得声明 `#[global_allocator]` 或通过默认 feature 隐式选择全局分配器。
`tests/api_contract.rs` 作为下游 crate 显式使用 System，编译阶段即验证分配器可组合性；
CI 与发布校验必须覆盖全 feature 和 `--no-default-features` 两种配置。

### 10.9 核心性能验证约束

- CPU 处理成本用 Criterion 的真实耗时衡量，不把虚拟协议时长混入测量值；仿真固定输入与随机种子，数据不完整或未确认完毕必须失败。
- 优化前先固定 bench 并保存命名基线，优化后比较同一工作负载；不得拿不同丢包档位的数据量或旧版虚拟时间报告比较速度。
- 新旧源码并存对照时必须使用独立 target 目录并核验二进制，防止复用错误构建产物；保留负向结果与环境漂移记录，不以单轮显著性证明跨轮稳定收益。
- 核心实例由单个任务独占驱动，未经并发证据不添加缓存行填充；公开 `Segment` 字段不借用高位存放隐藏状态，也不以破坏公开语义换取位掩码优化。
- `timing::dead_link_rejects_entire_data_batch_before_output` 保证死链预检发生在数据输出前；逐段输出只在 await 成功后提交状态，取消安全测试必须保留。
- `timing::ack_deduplication_handles_gaps_unsent_and_wrapped_sequences`、`timing::reordered_receive_queue_preserves_messages_across_reuse_and_wrap` 覆盖去重、乱序插入与回绕；`api_contract` 的所有权测试验证单段接收复用已有分配、包解析保留切片推进语义且载荷独立存活。

### 10.10 高层确认等待

- `drain` / `drain_to` 的控制请求必须和应用发送共用有界 FIFO；不能只检查调用瞬间的 `waitsnd()`，否则尚在高层队列的数据会被漏掉。
- 成功需同时满足此前数据获得 KCP 累计确认、当前待发 ACK 与窗口通告已输出到 UDP socket；读取缓冲后推进的累计确认也必须输出。监听器输出入队不算 socket 输出成功；错误与后台退出必须唤醒等待者。
- 单次总期限覆盖入队及确认等待，取消/超时不得撤回数据或留下常驻等待控制；close/Drop 仍立即终止。等待绑定原会话，新会话不得完成旧等待。
- `tests/stream_drain.rs` 覆盖高层链路与生命周期，`src/stream/drain.rs` 的单测覆盖控制队列期限、worker 退出、SN 回绕与 ACK 输出边界；文件示例须双端显式收尾，并区分已落盘与最终确认结果。

### 10.11 高层协议配置契约

- `StreamConfig` 只配置后台传输和会话生命周期；其字段保持兼容。显式协议参数通过 `connect_with_kcp_config` / `bind_with_kcp_config` 传入，旧入口保留 fast 默认值。
- 高层接口只支持消息模式；构造时校验 UDP MTU `29..=65507` 和非零窗口，所有发送入口必须与后台控制块使用同一 MSS 的 255 分片校验。不得在后台才发现超限发送。
- `tests/stream_config.rs` 用真实 UDP 抓包验证 MTU、窗口与旧默认行为，覆盖非对称配置、拒绝后恢复、最大消息、受损链路及 20 客户端超过 1 MiB 的分块传输；非对称 MTU 下两端 `drain` 黑洞超时后须恢复精确收发与确认。新增协议组合同步加入 `tests/common/stress.rs::config_matrix()`。
- `tests/common/udp.rs` 统一提供固定种子网络损伤及按报文过滤的回环代理，所有网络测试使用有界超时并清理后台任务；固定种子不保证操作系统调度顺序固定。

### 10.12 公开 API 与依赖兼容性

- 公开数据接口使用 `Vec<u8>`、`&[u8]` 等标准库类型，不重导出 Tokio/bytes，也不在公开字段、返回值、回调或 trait 约束中暴露它们。高层地址使用 `std::net::ToSocketAddrs + Send + 'static`，解析在后台执行，配置校验必须先于解析；高层内部仍使用 Tokio 运行时。
- Tokio 与 bytes 是精确版本锁定规范的明确例外：Cargo.toml 使用兼容范围 `1.52.3` / `1.11.1`，Cargo.lock 记录项目验证版本；不得重新使用 `=` 强制下游版本一致。其他依赖规则不变，不承诺超出声明范围的版本可用。
- 优先移动已有 `Vec<u8>`，不得在单段接收或跨内部通道交付时复制；发送编码借用队列载荷，避免为输出克隆整段。借用输入转为独立载荷、多段组装与线格式编码保留必要复制。`Segment::get_data()` 返回借用切片，独立所有权由调用者显式复制取得。
- `tests/downstream/core/` 与 `tests/downstream/stream/` 是独立 Cargo 项目，根 workspace 测试不会自动覆盖；CI 与发布必须分别执行它们。前者仅直接依赖本库并关闭默认 feature，后者用不同于库锁文件的兼容 Tokio/bytes 版本验证解析与真实回环收发。
