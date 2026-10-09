# 第一版及使用体验改进验收记录

2026-10-08，在 macOS / aarch64 上使用 rustc、cargo 1.99.0 完成第一版及使用体验改进后的复验。
所有测试只使用虚构凭据。Rust 1.89 是声明的最低要求，本轮未使用该旧版本构建。

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --all-targets --locked -- -D warnings` | 通过 |
| `cargo test --locked` | 30 项通过；1 个辅助进程入口默认忽略，由 exec 测试显式调用 |
| `cargo clippy --all-targets --target x86_64-unknown-linux-gnu --locked -- -D warnings` | 通过，包含测试代码编译 |
| `cargo clippy --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings` | 通过，包含 Windows 专属测试代码编译 |
| `cargo build --release` | 通过 |
| `cargo install --path . --root <临时安装目录> --locked --offline` | 通过，只安装 `molso` |
| 安装后的可执行文件独立 CLI 验收 | 前轮 11 组通过；本轮复验空更新、选项拼写和空列表 |

项目测试覆盖存储闭环、任意字节、显式更新、错密钥、篡改与截断、初始化恢复、
并发写入、环境及 stdin 注入、启动前校验、子进程退出码、Unix 信号与断管、TTY 拒绝。
`tests/compatibility.rs` 验证读取并更新由 macOS 生成的公开 v1 加密测试文件。

使用体验改进增加了 7 项行为测试：首次调用及帮助主题、全局选项位置与等号写法、
子进程帮助参数透传、输入前报告已知写入错误、真实终端隐藏输入、误传多余参数不回显，
以及无凭据映射的 exec 无需初始化。审查依据、改动及取舍见 [usability.md](usability.md)。

用户模拟实测后的三项修正又增加 4 项回归测试：终端空输入不能创建或覆盖凭据、
`--stdin --allow-empty` 可创建和更新为空值、选项拼写建议不会回显误传内容、终端空列表只在
stderr 提示。复用的 PTY 测试分别捕获终端 stdout 和 stderr；原有非终端空列表
测试补充检查两个输出流都保持静默。

本轮 release 构建及临时安装后，再以真实终端直接提交空更新，确认退出 2，且
vault 与旧凭据的摘要都未改变；直接调用拼错选项确认出现 --update 建议；删除
测试项后确认终端空列表给出下一步，非终端捕获仍完全静默。已安装文件与项目
release 文件的 SHA-256 一致。

发布前增加失败生产端空导入回归、allow-empty 必须配合 stdin 的参数约束，以及
exec 漏写分隔符的提示与启动前失败验证。重复初始化测试检查下一步提示；空值
导入测试改为显式 allow-empty。本机合计 30 项通过。

独立 CLI 验收还检查了 Unix 创建权限、空值与 Unicode 名称、8 个进程同时写入、
慢管道输出时释放锁，以及 exec 子进程回调写入同一 vault 不死锁。
该本机验收使用临时存储并自动清理。仓库内可重复执行的检查见 `tests/`；演示
录制脚本 `scripts/record-demo.py` 实际检查三种交付方式完成本地 HTTP 认证并返回
账户信息；空导入失败后的旧值保留由 `tests/cli.rs` 的行为回归覆盖。

已检查依赖 feature 图，`chacha20poly1305` 的 `zeroize` feature 确实启用。
敏感对象在应用退出前结束作用域；这不构成对标准库内部、分配器、OS 或子进程副本全部擦除的证明。

## 发布后的原生验证

公开仓库首次提交 `12ffbc9034195e4e26f0c98c368f957a6b0142e4` 触发了
[原生 CI](https://github.com/HeimaoLST/molso/actions/runs/37801319799)。
三个环境均使用 Rust 1.99.0，原生 CI 的 fmt、clippy 和测试全部通过。

| 平台 | 原生结果 | 证据 |
| --- | --- | --- |
| macOS / aarch64 | 本机及 CI 的 fmt、clippy、30 项测试通过 | [macOS job](https://github.com/HeimaoLST/molso/actions/runs/37801319799/job/113393986636) |
| Linux / x86_64 GNU | fmt、clippy、30 项测试通过；包含 Unix PTY 用例 | [Ubuntu job](https://github.com/HeimaoLST/molso/actions/runs/37801319799/job/113393986537) |
| Windows / x86_64 MSVC | fmt、clippy、26 项测试通过 | [Windows job](https://github.com/HeimaoLST/molso/actions/runs/37801319799/job/113393986265) |

Windows 创建时的当前用户 DACL、Unicode 环境名大小写碰撞和 32 位退出码保留
已经在原生 CI 实际执行。macOS 格式 v1 测试文件在三个平台均可读取，并在原生
更新后保持原条目可读。Linux 的隐藏输入和空输入保护也经过真实 PTY 验证。

## 剩余验证

尚未专项验证 Windows 交互终端输入、Windows 目录持久化、各 shell 的管道转换和
真实掉电恢复。Rust 1.89 是声明的最低版本，目前验证使用的是 1.99.0。

使用契约和已接受的密钥文件、管道及子进程输出边界见 [usage.md](usage.md)。
