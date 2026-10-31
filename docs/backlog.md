# 待办

尚未完成的工作与待用户决定的事项都列在这里，细节写在各自的文档里。做完的条目从这里删掉，并在验收或发行记录里注明。顺序由用户定；Host 的建议标明是建议。

## 进行中

- **接口精简第一批**
  - 范围：[接口审查](plans/2026-10-31-api-surface-review.md)的第一、二节，以及决定 2、3。
  - 状态：已在分支 `socu/interface-slimming` 实施并验收；macOS 上与 FastDB 0.2.4 并排验证通过，fork 全量 CI [41173746331](https://github.com/Dsssyc/c-two/actions/runs/41173746331)（`f0f9efaa`）在 Linux、macOS、Windows x64 与 ARM64 上全部通过。
  - 之后：向上游的 PR 暂缓，等后面几批一起开（用户 2026-10-31）。

- **过度防御复审的真缺陷与死代码两批**
  - 范围与分工：见[执行计划](plans/2026-10-31-defect-and-dead-code-batches.md)。按 crate 分成四个微任务：内存、wire 与服务端；IPC 与本地端点；Core、配置与 Python；relay、代码生成与 CLI。
  - 状态：五个微任务都已集成到分支 `socu/over-defense-batches`。M1 至 M4 按 crate 并行；M5 迁移跨范围调用点，并补做 C1、C3、C8 零碎与 H13。macOS 上与 FastDB 0.2.4 并排验证通过：Python 1375 项、代码生成 33 项、Rust SDK 23 项、CLI 78 项、c2-core 145 项、native 16 项、`tests/repo` 612 项。
  - 之后：完整的 Core relay 测试套件要在干净的 runner 上跑，交给 fork 全量 CI；CI 通过后，在看板上验收这五个微任务。

## 顺序（Host 建议，待用户确认）

1. 重复校验；tmpfiles 先在 Linux 上复现。
2. 结构调整，先出方案。
3. 协议批，先出详细设计，附一份流式 RPC 草图。
4. 配置收敛（逐项表定下之后）、AGENTS.md 重组、错误类型、流式 RPC。

## 过度防御复审

[复审](plans/2026-10-31-over-defense-review.md)第八节列出的批次。真缺陷与死代码已开工（见上），其余还没有开始：

- **真缺陷**（第一节，不涉及协议）：B8、D-drop、D4、A8、H3、H10、H11、H12、G4、B10、C8、F5c、F6、D9、F9。E-host 归入结构调整。
- **tmpfiles**（D-tmp）：先在 Linux 上复现 systemd-tmpfiles 清掉 gate 与 marker 的情形，再实施目录 flock。本机与 Windows 测试机都不是 Linux。
- **死代码**（第二节）：C1、B1、F1、F7、G1、C3、A3、G6、A2、E3/E6/E7、H2/H5、G-slot，以及零碎各项。F7 与 G2 牵涉的几项，等用户的决定。
- **重复校验**（第三节）：按区域进行。
- **结构调整**（第四节，先出方案）：
  - Core 生命周期：E1、E2、E4、E-host；
  - IPC 结算：C-U1、C2；
  - 分片与池锁：B2；
  - relay：F2、F3、F6。
- **协议批**（第五节）：
  - 核心：[单一属主释放](plans/2026-10-31-single-owner-release-evaluation.md)，即握手 15、SHM 格式 4、C ABI 7，A9 也在其中。
  - 先写详细设计，包括消息格式、借出表、两个方向的发送时机、各条关闭路径与端到端基准；附一份流式 RPC 草图。经用户批准后再实施。
- **测试**：随各批一起删除。

附录里各区还没裁决的待议项，实施到对应区域时再定。

## 接口审查的余项

- **配置收敛**（第三节）：逐项表待用户决定。它与 B8、H3、H12 交叉，实施时一并处理。
- **AGENTS.md 重组**（第五节）：在接口精简第一批合入之后进行。
- **错误类型**（第四节）：排在协议批之后。
- **所有者子系统去留**（第六节）：待用户决定。在此之前，它留在 `c_two.owner`，行为不变。

## 瘦身账本的剩余项

[账本](plans/2026-10-13-local-slimming-batches.md)里还有尚未单独验收的 R09、R10、R19、R21、R35，以及暂缓表中的各项。与复审重叠的，按复审第七节的修正处理，例如 R19 就是 H10，R26 与 R34 的保留结论已被修正；其余仍按账本写明的暂缓条件。

## 将来

- **流式 RPC**（[路线图](roadmap.zh-CN.md)第 7 项）：用户确认这是必备能力。排在协议批之后；设计要回答单一属主评估"与流式 RPC 的关系"一节列出的八个问题。

## 待用户决定

- 配置项的逐项表。
- 所有者子系统是否删除。
- relay 轮询的间隔。
- `/_admin/unregister` 有没有人手动使用（F7）。
- `crm.client = <对象>` 与 `HeldResult(release_cb=…)` 算不算扩展点（G2）。
- `c_two.config.settings` 的直接写入是否受支持（G8）。
- 单一属主的决定 2、3：hold 与连接绑定；属主一方结束连接时只停止服务。随详细设计确认。

## 顺带项

下一批碰到同一块代码时顺带做掉：

- 本地直接跑 pytest 时，TypeScript 测试要先构建 c2-mem-ffi 的 Node 绑定（`core/foundation/c2-mem-ffi/bindings/typescript` 下 `npm ci && npm run build`）。没构建时，有 2 项报错、约 30 项被跳过，看上去像回归。可以让夹具缺这个绑定时直接报出来，或者在开发文档里写明这一步。

- `direct_ipc_endpoint`、`ping_direct_ipc`、`shutdown_direct_ipc` 收到上下文时每次都克隆一份，可以直接借用（`core/runtime/c2-core/src/control.rs`）。
- `register` 的签名直接写 `Literal['materialized', 'borrowed']`，服务端桥里却另有一个私有别名 `_InputLifetime`。统一成一处。
- 直接用 pytest 跑 Python 套件时，`test_typescript_persistent_ipc.py` 依赖别的测试先构建好 fastdb4ts；运行顺序不同就会报"尚未构建"。CI 的运行器会预先构建，所以只在本地出现。

## 下游跟进（由用户决定）

- **hey-my-buddy：** 导入改到新的子模块，`ConcurrencyMode` 改为字符串。配置收敛之后，它设的 9 个 IPC 覆盖键大多会消失。
- **toodle 与一个水文模拟后端：** 升级时核对导入；后者用到了 `ConcurrencyMode`，要改为字符串。
