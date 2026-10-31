# 待办

尚未完成的工作与待用户决定的事项都列在这里，细节写在各自的文档里。做完的条目从这里删掉，并在验收或发行记录里注明。顺序由用户定；Host 的建议标明是建议。

## 进行中

- **接口精简第一批**
  - 范围：[接口审查](2026-10-31-api-surface-review.md)的第一、二节，以及决定 2、3。
  - 状态：已在分支 `socu/interface-slimming` 实施并验收；macOS 上与 FastDB 0.2.4 并排验证通过，fork 全量 CI [41173746331](https://github.com/Dsssyc/c-two/actions/runs/41173746331)（`f0f9efaa`）在 Linux、macOS、Windows x64 与 ARM64 上全部通过。
  - 之后：向上游的 PR 暂缓，等后面几批一起开（用户 2026-10-31）。

- **过度防御复审的真缺陷与死代码两批**
  - 范围与分工：见[执行计划](2026-10-31-defect-and-dead-code-batches.md)。按 crate 分成四个微任务：内存、wire 与服务端；IPC 与本地端点；Core、配置与 Python；relay、代码生成与 CLI。
  - 状态：已在分支 `socu/over-defense-batches` 实施并验收。M1 至 M4 按 crate 并行；M5 迁移跨范围调用点，并补做 C1、C3、C8 零碎与 H13。macOS 上与 FastDB 0.2.4 并排验证通过：Python 1375 项、代码生成 33 项、Rust SDK 23 项、CLI 78 项、c2-core 145 项、native 16 项、`tests/repo` 612 项。
  - CI：首轮 fork 全量 CI（`8b756c74`）暴露两个遗留测试：relay 的容量测试还依赖 F9 删掉的 1024 上限，c2-local 的一个 Drop 测试与后台退役线程有时序竞争。打回 M4、M2 修正后，fork 全量 CI [41192397960](https://github.com/Dsssyc/c-two/actions/runs/41192397960)（`e1630cf1`）在 Linux、macOS、Windows x64 与 ARM64 上全部通过。五个微任务已在看板上验收。
  - 之后：向上游的 PR 暂缓，与接口精简第一批一起开（用户 2026-10-31）。

- **文档与 AGENTS.md 重组**
  - 范围：[执行计划](2026-10-31-docs-and-agents-restructure.md)。
  - 状态：第一步已完成并验收，合入分支 `socu/docs-restructure`（`a86403af`）。根 AGENTS.md 从 64,905 字节瘦到 21,934 字节，机制细节移入 `docs/reference/`；文档卫生检查守住 32 KiB、相对链接与中英两版的互链；独立审查确认旧 AGENTS.md 的规则都有着落。
  - 第二步：用户 2026-10-31 批准执行计划第八节的清退清单，已实施并验收，合入为 `143c11e3`。`docs/` 只剩 reference、design、acceptance、research、releases、assets 六个目录；删除 151 份，另有 15 份在核对代码、挪出仍然成立的内容后删除；链接全部迁移。文档卫生检查改用 markdown-it-py，覆盖全部 Markdown，并拒绝本机的主目录与工作目录布局（hosted CI 上跳过）。
  - 之后：`CONTEXT.md` 术语表待用户确认词条；然后跑一次 fork 全量 CI，向上游的 PR 在代码批次之后单独开。

## 顺序（Host 建议，待用户确认）

1. 文档与 AGENTS.md 重组的第二步（见进行中）。
2. 重复校验；tmpfiles 先在 Linux 上复现。
3. 结构调整，先出方案。
4. 协议批，先出详细设计，附一份流式 RPC 草图。
5. 配置收敛（逐项表定下之后）、错误类型、流式 RPC。

## 过度防御复审

[复审](2026-10-31-over-defense-review.md)第八节列出的批次。真缺陷与死代码两批已完成（见上），只剩下面写明的余项；其余还没有开始：

- **真缺陷与死代码的余项**：E-host 归入结构调整，B1 归入协议批；F7 中的 `/_admin/unregister` 与注销状态码的区分、G2、G8 等用户决定；各区零碎里属于重复校验的部分归入重复校验。
- **tmpfiles**（D-tmp）：先在 Linux 上复现 systemd-tmpfiles 清掉 gate 与 marker 的情形，再实施目录 flock。本机与 Windows 测试机都不是 Linux。
- **重复校验**（第三节）：按区域进行。
- **结构调整**（第四节，先出方案）：
  - Core 生命周期：E1、E2、E4、E-host；
  - IPC 结算：C-U1、C2；
  - 分片与池锁：B2；
  - relay：F2、F3、F6。
- **协议批**（第五节）：
  - 核心：[单一属主释放](2026-10-31-single-owner-release-evaluation.md)，即握手 15、SHM 格式 4、C ABI 7，A9 与 B1 也在其中。
  - 先写详细设计，包括消息格式、借出表、两个方向的发送时机、各条关闭路径与端到端基准；附一份流式 RPC 草图。经用户批准后再实施。
- **测试**：随各批一起删除。

附录里各区还没裁决的待议项，实施到对应区域时再定。

## 接口审查的余项

- **配置收敛**（第三节）：逐项表待用户决定。它与 B8、H3、H12 交叉，实施时一并处理。`chunk_threshold_ratio`（`C2_IPC_CHUNK_THRESHOLD_RATIO`）已证实没有作用：只在 c2-config 里解析与校验，c2-ipc 只在比较两份客户端池配置时用到它，分片判断不读它；收敛时连同环境变量、Python 字段与测试一起删除。
- **AGENTS.md 重组**（第五节）：并入[文档与 AGENTS.md 重组](2026-10-31-docs-and-agents-restructure.md)，叠在 `socu/over-defense-batches` 上做，不再等接口精简合入上游。
- **错误类型**（第四节）：排在协议批之后。
- **所有者子系统去留**（第六节）：待用户决定。在此之前，它留在 `c_two.owner`，行为不变。

## 瘦身账本的剩余项

[账本](2026-10-13-local-slimming-batches.md)里还有尚未单独验收的 R09、R10、R19、R21、R35，以及暂缓表中的各项。与复审重叠的，按复审第七节的修正处理，例如 R19 就是 H10，R26 与 R34 的保留结论已被修正；其余仍按账本写明的暂缓条件。

## 盘点中发现的未完成事项

2026-10-31 文档盘点从历史文档里找出、此前没有登记的事项。出处见[执行计划](2026-10-31-docs-and-agents-restructure.md)第八节涉及的原文档。

- **协议批一并处理：** 客户端的请求 id 计数器是 `AtomicU32`，线上格式是 u64，要核实传输时是否截断（`c2-ipc/src/client.rs`）；SHM 释放坐标无法认证，要在单一属主释放的详细设计里点名。
- **relay：** relay-aware 调用没有每次调用的路由回执，`observed_route` 仍是连接时的值；relay 的 HTTP 响应仍整体物化，没有流式，与流式 RPC 一并考虑；HTTP 载荷的内存预算与消费生命周期只有设计提案，没有实施（`plans/2026-10-06-http-memory-budget.md`）。
- **TypeScript：** 公共错误类不暴露 `pre_dispatch` 与 `dispatch_uncertain` 的区分；resolve 返回的 `route_revision` 应以十进制字符串保全 u64，现在只到 2^53−1，这是协议改动；无法解码不透明的响应 allocator。
- **内存：** buddy 中间空洞的复用；探针退出后客户端 backing 仍能打开，这一现象待核实；逐处复制字节的归因测量（`plans/2026-09-26-memory-policy.md` 的遗留项）。
- **发版敏捷性方案 B：** B1 候选重整、B2 删除约 8 千行（`tools/local_rc` 与 `local-endpoint-validation.yml` 仍在）、B3 FastDB 版本只写一处、B5 精简各条线的 gate、B6 修 `PROBE_BUDGET` 的偶发失败；决定 3、7 待定（`plans/2026-10-30-release-agility-review.md` 第三、四节）。
- **瘦身账本条目边界表里的余项：** R17 的 gate/context 与目录 pin 的错误分类；R06 回调重入导致自锁的风险；R13 写锁前取消时保留未发布的分配；R16 完整的 Node→Core 投影迁移；R29、R32、R33 的剩余，含孤立的 `wrap_error`；两份控制会话重构；C04；R14 与 C03 的剩余；R07：`abandoned_releases` 出现非零时重新评估。与复审重叠的部分按复审第七节处理。
- **要核实的保证：** 旧 Python 服务端用写屏障保证同一连接内先写后读的顺序，当时的规格说 Rust 有等价机制，但现行文档没写，`c2-server` 里也没找到；执行准入规格的不变量 I14（连接屏障）同样没有实现。查清楚之后，要么写进文档，要么确认已经不再保证。
- **契约发布的延后能力：** [延后能力表](contract-release-deferred-capabilities.md)里仍开着、但目前没有使用者的几项（Python portable 参数形状的扩展、原始 `unsafe_buffer` 别名的访问边界、产物里的非普通文件、C++ SDK，以及 descriptor 存储、签名与信任这类下游 catalog 的事）不排期；有了使用者再登记成具体条目。


## 将来

- **多副本一致性**（relay mesh 原先设想的第二、三阶段）：2026-07 的草稿设想过同一资源在多个节点上的副本：路由带版本号，写成功后经 gossip 通知其他副本过期，写之前广播意图做跨节点写锁，读到过期副本时按策略处理，并提供内置的刷新方式与一致性级别。这些从没实现，现在不同节点上的同名路由彼此不知道对方。用户 2026-10-31 认为那套做法不一定是最佳策略，留作蓝图：协议批与流式 RPC 落地之后，或者有下游需要同一资源的多个副本时，再从头讨论设计。原文见上游的[固定版本](https://github.com/world-in-progress/c-two/blob/606b8f13290c15ebff3a04a02e2ac5c6463afa7c/docs/superpowers/specs/2026-07-20-relay-mesh-resource-discovery-design.md)第二、三阶段。
- **流式 RPC**（[路线图](roadmap.zh-CN.md)第 7 项）：用户确认这是必备能力。排在协议批之后；设计要回答单一属主评估"与流式 RPC 的关系"一节列出的八个问题。

## 待用户决定

- 配置项的逐项表。
- `CONTEXT.md` 的词条（文档清退清单已于 2026-10-31 批准）。
- 所有者子系统是否删除。
- relay 轮询的间隔。
- `/_admin/unregister` 有没有人手动使用（F7）。
- `crm.client = <对象>` 与 `HeldResult(release_cb=…)` 算不算扩展点（G2）。
- `c_two.config.settings` 的直接写入是否受支持（G8）。
- 单一属主的决定 2、3：hold 与连接绑定；属主一方结束连接时只停止服务。随详细设计确认。

## 顺带项

下一批碰到同一块代码时顺带做掉：

- 删除从没产生过的错误码 706 `WriteConflict`，连同 Python 的错误类、映射与相关测试（用户 2026-10-31 同意）。它原本属于上面"多副本一致性"的写锁设想；以后真要做，按新设计重新登记。704 是还在使用的 `RouteStale`，不动。

- 本地直接跑 pytest 时，TypeScript 测试要先构建 c2-mem-ffi 的 Node 绑定（`core/foundation/c2-mem-ffi/bindings/typescript` 下 `npm ci && npm run build`）。没构建时，有 2 项报错、约 30 项被跳过，看上去像回归。可以让夹具缺这个绑定时直接报出来，或者在开发文档里写明这一步。

- c2-local 的 Drop 在门锁短暂争用后由后台线程完成退役，这条正向路径已没有确定性的测试守住：`managed_drop_after_gate_contention_settles_ownership` 两种终态都接受。要守住，需要一个能观察后台重试的点。
- `direct_ipc_endpoint`、`ping_direct_ipc`、`shutdown_direct_ipc` 收到上下文时每次都克隆一份，可以直接借用（`core/runtime/c2-core/src/control.rs`）。
- `register` 的签名直接写 `Literal['materialized', 'borrowed']`，服务端桥里却另有一个私有别名 `_InputLifetime`。统一成一处。
- 直接用 pytest 跑 Python 套件时，`test_typescript_persistent_ipc.py` 依赖别的测试先构建好 fastdb4ts；运行顺序不同就会报"尚未构建"。CI 的运行器会预先构建，所以只在本地出现。

## 下游跟进（由用户决定）

- **hey-my-buddy：** 导入改到新的子模块，`ConcurrencyMode` 改为字符串。配置收敛之后，它设的 9 个 IPC 覆盖键大多会消失。
- **toodle 与一个水文模拟后端：** 升级时核对导入；后者用到了 `ConcurrencyMode`，要改为字符串。
