# 交互面精简审查

日期：2026-10-31。输入：upstream main `606b8f13`。状态：第一轮审查，没有改动代码；方案待用户决定。

起因：用户要求按 C-Two 的品味审查各个交互面是否足够精简。理由不只是方便下游开发者：C-Two 不在模型已有的知识里，AI 只能把接口读进上下文，接口越大，越占上下文，也越容易丢掉关键细节，这不利于在 C-Two 之上继续做更复杂的下游应用。流式 RPC 是将来必须有的能力，它新增的接口也按本文的分层与预算来设计。

## 量一量

以下是 Host 实测，token 按字节数除以 4 估算：

| 交互面 | 现状 | 普通应用实际要用的 |
| --- | --- | --- |
| Python 顶层 `cc.*` | 79 个名字，签名加各自的文档约 6.8k token | 约 19 个，约 2.0k token |
| Rust SDK 根 `c_two::*` | 92 项（不含隐藏的 `generated` 模块） | 约 10 项 |
| 配置项 | 39 个环境变量，其中 IPC 27 个；多数在代码覆盖里还有一个同名的键 | 3–5 个 |
| 用户文档 | README 约 2.3k，`python-usage` 4.2k，`configuration` 3.9k，`memory-policy` 4.8k，`local-endpoint-lifecycle` 8.3k | README 加一份用法说明 |
| AGENTS.md | 64 KB，约 16k token；每个在本仓库工作的智能体都要读，包括每个微任务的 Worker | — |
| `c3` 命令行 | 4 组共 14 个命令 | 已经紧凑 |

Python 的 79 个名字里，日常接口与监督进程、工具链、诊断用的接口混在同一个命名空间，`dir(cc)` 和类型提示看不出哪些是日常用的。

## 依据

- **一件事一种写法。** 没有别名，没有第二个构造入口，同一个值不接受两种拼写。
- **按受众分层。** 日常接口放在顶层：声明契约、注册、连接、hold、关闭。监督进程（所有者控制、端点维护）、工具链（契约导出与代码生成）和诊断（连接事件、内存统计、生命周期快照）各进一个子模块。分层不是隐藏：这些能力仍然公开，只是不和日常接口挤在一起。
- **返回值有类型。** 调用方不读文档也能看出结构。
- **开关要有使用者。** 没有使用者的开关不留；能从别的设置推导出来的，不做成设置。
- **名字说明是什么。** 公开名字说明它是什么，不说明它在哪里实现。

## 一、可以直接修的缺陷（Host 核对）

1. `cc.Held` 是 `HeldResult` 的别名（`sdk/python/src/c_two/crm/transferable.py:180`）。代码里没有用到它；唯一一处测试只断言它是别名（`sdk/python/tests/unit/test_held_result.py:12-13`），其余只出现在旧文档里。删掉别名和这条断言。
2. 资源桥有三种写法：`cc.bridge(...)`、`cc.ResourceBridge(...)`，以及在 `register(bridge=...)` 里直接写字典（`crm/bridge.py:31-36`、`:40-56`）。`bridge` 与 `infer_crm_from_resource` 都没有文档字符串，AI 只能去读源码。如果保留（见第五节的决定 3），只留一种写法，并补上说明。
3. 采纳所有者控制有两个入口：`cc.adopt_owner_stdin()` 与 `NativeOwnerBootstrap.adopt_owner_stdin()`，而后者的文档自称是唯一入口。留一个。
4. `@cc.crm` 的 `namespace` 与 `version` 有默认值 `'cc'` 和 `'0.1.0'`（`crm/meta.py:127`），AGENTS.md 却写着必填。契约身份不该有默认值，否则两个包里同名的类都会成为 `cc/<类名>/0.1.0`。改为必填。
5. AGENTS.md 与 `.env.example` 把 `C2_IPC_MAX_PENDING_REQUESTS` 写作"每条连接"的上限，代码里却是整个 Server 共用一个信号量（`core/transport/c2-server/src/server.rs:622`）。照说明配置会配错。在第三节的配置收敛里决定它的语义，文档随之改正。
6. `hold_stats()` 返回的字段与 `memory_stats()["holds"]` 完全相同（Host 运行比对），是重复的入口。删掉 `hold_stats()`。`memory_stats()` 的结果里还带着一句固定的说明文字 `budget_cells_note`，它应该写在文档字符串里，不该出现在数据里。
7. 7 个公开函数返回裸 `dict`：`shutdown`、`hold_stats`、`call_execution_snapshot`、`native_lifecycle_snapshot`、`native_terminal_outcome`、`inspect_endpoint`、`reap_endpoint`。同一个包里，`memory_stats` 却返回有类型的 `MemoryStats`。全部改为有类型的返回值。
8. `native_lifecycle_snapshot` 与 `native_terminal_outcome` 的名字带着实现位置，而且只在测试和诊断里用到。随分层移入诊断子模块，并改名。
9. `c_two.error` 没有 `__all__`，`IntEnum`、`Literal`、`Mapping`、`unique` 这些导入进来的名字都露在模块里。补上 `__all__`。
10. Rust 的端点函数成对出现：`ping_direct_ipc` 与 `ping_direct_ipc_with_context`、`shutdown_direct_ipc` 与 `shutdown_direct_ipc_with_context`、`direct_ipc_endpoint` 与 `direct_ipc_endpoint_with_context`（`sdk/rust/src/lib.rs:15-32`）。每对合成一个，用一个可选的上下文参数。

## 二、按受众分层

Python 的建议布局：

| 位置 | 内容 |
| --- | --- |
| `c_two` 顶层 | `crm`、`read`、`write`、`transfer`、`on_shutdown`、`hold`、`HeldResult`、`register`、`connect`、`close`、`unregister`、`serve`、`shutdown`、`server_address`、`set_relay_anchor`、`set_server`、`set_client`、`Runtime`、`ConcurrencyConfig`、`error` |
| `c_two.owner` | 所有者控制与托管子进程，共 11 个名字 |
| `c_two.endpoint` | 端点维护：凭据、上下文、检查、回收、清扫、`set_local_endpoint`、`server_credential`、`server_id` |
| `c_two.observe` | 诊断：连接事件、内存统计、调用准入快照、生命周期快照 |
| `c_two.contract` | 工具链：契约导出、诊断、推断与代码生成 |

`set_transport_policy`、`set_call_execution_limits`、`with_call_options`、`InputLifetime` 和资源桥的位置，在第三节与第五节的决定之后再定。

- **默认实例与显式实例。** 模块级函数委托给默认 Runtime，`cc.Runtime()` 提供同样的方法，这是常见的"默认实例加显式实例"写法，予以保留。子模块里的函数改为接受一个可选的 `runtime` 参数，`Runtime` 自己只保留日常方法，避免每个高级能力都在两处各有一份。
- **Rust 同样按模块分。** 根上只留 `Runtime`、`Connect`、`ConnectOptions`、`Client`、`Host`、`HostOptions`、`ContractRelease`、`CallOptions`、`Error`、`Held` 这一类，其余进入 `endpoint`、`owner`、`observe`、`contract` 四个模块，两种 SDK 的分组保持一致。
- **效果。** AI 写一个普通应用时，只需读顶层，约 2k token；做监督进程或工具链时，再多读一个模块。

## 三、配置项

39 个环境变量里：

- 示例只设置了 `C2_RELAY_ANCHOR_ADDRESS`；CI 只设置了它和 `C2_ENV_FILE`。
- 大多数 IPC 变量在 Python 测试里只出现一次，都在列举全部变量的配置解析测试 `sdk/python/tests/unit/test_ipc_config.py` 里；在 `docs/configuration.md` 里也只出现一次。
- 每个 IPC 变量在 `ServerIPCOverrides` 或 `ClientIPCOverrides` 里还对应一个代码覆盖键，所以一个开关要占两到三处交互面。

建议保留确有使用者的几个：

- `C2_RELAY_ANCHOR_ADDRESS`、`C2_ENV_FILE`、`C2_IPC_ROOT`
- 决定共享内存上限的 `C2_IPC_POOL_SEGMENT_SIZE` 与 `C2_IPC_MAX_POOL_SEGMENTS`
- relay 服务端的部署参数，它们同时也是 `c3 relay` 的命令行选项

其余变量逐项决定三种去向：改为推导，改为固定常量，或者删除。例如：

- 分块阈值比例、分块总数上限：已有缺陷 B8。
- 重组段的大小与段数、重组与临时文件的预算。
- 分块的回收间隔与组装超时、池的衰减与预热段数。
- 心跳的间隔与超时：在单一属主方案里，心跳的作用也随之改变。
- `C2_IPC_MAX_EXECUTION_WORKERS`：已有缺陷 H12。

做法是出一张逐项表（保留、推导、固定或删除，附理由），经用户决定后，在同一批里把环境变量、代码覆盖键和文档一起删干净。

## 四、错误类型

`c_two.error` 有 29 个错误类。其中 8 个按"出错的一侧 × 阶段"命名，例如 `ResourceDeserializeInput`、`ClientOutputFromBuffer`。应用真正要分支处理的是：找不到资源、资源不可用、超时、契约不匹配、资源方法自己抛了错。建议只为这些可据以行动的区别保留类，一侧与阶段改为错误上的字段。错误码属于 Rust 的错误注册表，并且在线上传输，所以这一项放在协议批之后，优先级低于前三节。

## 五、文档也是交互面

- **AGENTS.md。** 约 16k token，每个在本仓库工作的智能体都要读。其中大部分是各个机制的细节，以及"不要恢复某某"的历史条目。建议 AGENTS.md 只留当前的架构地图、构建与测试命令和工作规则；机制细节放进各自的文档（内存、生命周期、传输），按路径链接；已退役接口的禁令合成一张简短的清单。
- **下游应用。** 普通应用的最短路径是 README 的快速开始，加上顶层模块的文档字符串；生命周期与内存策略的文档，只在用到相应子模块时才需要读。不另写一份给 AI 看的文档，免得多一份要同步的副本。

## 批次

1. **缺陷修正**：第一节，不改协议，可与死代码批同行。
2. **分层**：Python 与 Rust 一起做；属于导入路径的不兼容变更，按 0.x 的规则一次切干净。
3. **配置收敛**：先出逐项表，经用户决定后实施。
4. **错误类型**：协议批之后。
5. **AGENTS.md 重组**：由 Host 改写，经用户同意后进行。

## 需要用户决定

1. 第二节的分层与子模块名（`owner`、`endpoint`、`observe`、`contract`）。
2. `ConcurrencyMode` 与 `InputLifetime` 现在既收枚举也收字符串（`transport/input_lifetime.py:16`，`ConcurrencyConfig.mode: ConcurrencyMode | str`）。Host 建议只收字符串字面量（`Literal[...]`），从公开接口中删掉这两个枚举类：类型检查照样能发现拼写错误，也少导入两个名字。
3. 契约的第二种写法 `infer_crm_from_resource`（以及 `c3 contract infer`）和资源桥，都只有测试在用。它们提供便利，与"自由与便捷"的目标一致，但也让写契约有了两条路。是保留并补齐文档，还是删掉？
4. 是否按第三节的做法收敛配置项。
5. 是否按第五节重组 AGENTS.md。
