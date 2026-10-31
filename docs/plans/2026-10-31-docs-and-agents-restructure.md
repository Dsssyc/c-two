# 文档与 AGENTS.md 重组的执行计划

日期：2026-10-31。输入：分支 `socu/over-defense-batches`（`c4c53c08`）。依据：[接口审查](2026-10-31-api-surface-review.md)第五节用户已同意的 AGENTS.md 重组；用户 2026-10-31 同意先瘦 AGENTS.md、再按 hey-my-buddy 仓库的分法整理 `docs/`，两步都叠在 `socu/over-defense-batches` 上做，上游另开 PR，暂不建嵌套的 AGENTS.md。状态：计划，第五节的几项待用户决定。

## 一、量过的问题

- **根 AGENTS.md 太大。** 526 行、64,905 字节。Codex 只加载从项目根到当前目录这一路上的 AGENTS.md，合计上限是 `project_doc_max_bytes`，默认 32 KiB，超出的部分截断，只记一条警告（openai/codex main `67f0994b53` 的 `codex-rs/core/src/agents_md.rs`，官方文档的说法相同）。在仓库根启动的 Codex 因此读不到第 32 KiB 之后的内容：Rust Native Layer 的大部分、CLI、Key Conventions（FastDB 写法、hold、视图生命周期、错误、命名、性能）、环境变量表、Python 版本，以及全部 Agent Working Rules。截断是读源码和文档得出的，没有起会话实测。Claude Code（官方文档，v2.1.277 起）在没有 CLAUDE.md 时读 AGENTS.md（本仓库的会话里整份载入），子目录里的 AGENTS.md 要等它打开该目录的文件时才加载，并建议单个文件少于 200 行。
- **机制细节只在 AGENTS.md 里。** `open_peer`、`allocation_id`、`ServerStartupTimeout`、`ExpectedRouteContract`、错误码分类等，在现有参考文档里找不到；环境变量表则与 `configuration.md` 重复。
- **`docs/` 没有索引，现行文档与历史记录混放。** 490 个受版本管理的文件、约 11 MB：顶层 18 个文件，加 `designs`、`images`、`issues`、`logs`、`plans`、`releases`、`reports`、`reviews`、`spikes`、`superpowers`、`vision` 十一个目录。没有术语表。
- **双语文档有两套命名。** `configuration`、`local-endpoint-lifecycle`、`memory-policy` 是 `X.md`（中文）配 `X.en.md`；README 与路线图是 `X.md`（英文）配 `X.zh-CN.md`。`ipc-handshake.md` 只有中文。
- **被代码、测试或入口文档引用的。** `reports/evidence/`（124 个，`tests/repo/test_portable_matrix_receipt.py` 与 AGENTS.md）；`releases/`（CHANGELOG、各 README、`tests/repo/test_ci_scope.py`）；`images/`（各 README、`tools/dev/generate_banner.py`）；`issues/` 与 `roadmap.md`（`core/runtime/c2-core/tests/error_normalization.rs` 扫描它们，查一个已删除的 crate 名）；两份 0.7.4 报告（`sdk/python/tests/README.md`）。

对照：hey-my-buddy 的 AGENTS.md 有 63 行、19 KB，只写不变量、布局、验证与文档规则；`docs/README.md` 是索引；文档按 `reference`、`decisions`、`design`、`acceptance`、`research` 分放；根目录有术语表 `CONTEXT.md`；文档卫生检查要求相对链接能解析、不出现本机路径。

## 二、目标结构

| 位置 | 内容 |
| --- | --- |
| `AGENTS.md` | 架构地图、构建与测试命令、跨模块不变量、已退役接口的短清单、文档规则、工作规则、路由表；不超过 32 KiB，目标 24 KiB 以内 |
| `CONTEXT.md` | 术语表，中英成对 |
| `docs/README.md` | 索引：每个主题由哪份文档负责；自身不陈述行为 |
| `docs/reference/` | 现行行为与契约。现有的配置、IPC 握手、本地端点生命周期、内存策略、Python 用法、Windows、CI、开发与本地测试文档，加上从 AGENTS.md 挪出的机制说明 |
| `docs/decisions/` | ADR，一个决定一份，以后不改写，只追加带日期的修订。从下一个决定（单一属主释放）开始，不补写旧决定 |
| `docs/design/` | 待办、路线图、进行中的计划与评估、仍然作数的远景文档 |
| `docs/acceptance/` | 验收记录与证据：现在的 `reports/`（含 `evidence/`）与 `reviews/` |
| `docs/research/` | 决定之前的调研笔记 |
| `docs/releases/`、`docs/assets/` | 发行说明保持原位；`images/` 改名为 `assets/` |

嵌套的 AGENTS.md 先不建：Codex 不会自动读当前目录以下的文件，根文件里还是要写明先读哪份，模块的事实就会有两处要同步。拆完以后，某个子树有自己的工具链和规则、根文件放不下时，再单独为它建。

## 三、第一步：AGENTS.md 瘦身

### 根文件留下的

1. 0.x 约束、项目概述（CRM、Resource、Client）；
2. 架构地图：crate 表，每层一句话说明归属；
3. 构建、测试与运行命令；
4. 跨模块的不变量，每条一句：Rust core 拥有通用运行时机制，SDK 只是外观；直接 IPC 不依赖 relay；解析与调用按路由加契约身份；Core 拥有截止时间、就绪与生命周期；Python 不保留 Rust 权威的第二份状态；请求快照与传输租约的顺序，以及不多拷贝；`c2-core` 与载荷属主无关；同进程调用直接传 Python 对象；
5. 已退役接口：合成一张表，旧名字对应现在的做法；
6. 文档规则与路由表（措辞见下）；
7. Agent Working Rules，原样保留。

### 各节的去处

一事一处：先合并进已有的文档，没有才新建。下表是暂定的，实施方核对已有文档的覆盖面后可以调整，调整写进报告。

| 现在的小节 | 去处 |
| --- | --- |
| Local Release-Candidate Evidence | 已有的验收记录；根文件只留一句链接 |
| CRM Layer、契约与路由相关的 Transport 段落（契约身份、relay 解析、relay 发现的 IPC 身份校验、直接 IPC） | 新建 `reference/contracts-and-routing.md` |
| Config Layer、Environment Variables | `reference/configuration.md`；核对两份表一致后，删掉 AGENTS.md 里的副本 |
| Transport Layer 的会话、注册、启动、关闭、退役与连接截止时间 | 新建 `reference/runtime-lifecycle.md` |
| Rust Native Layer 的内存子系统 | 合并进 `reference/memory-policy.md` |
| Rust Native Layer 的本地端点、凭据与所有者控制 | 合并进 `reference/local-endpoint-lifecycle.md` |
| c2-mem-ffi ABI | 新建 `reference/c-abi.md` |
| CLI 选项表 | `cli/README.md`（已有 relay 选项） |
| FastDB CRM Pattern、Hold Mode、FastDB View Lifetime Boundary、Performance-Sensitive Code 的分派路径 | 新建 `reference/payload-lifetime.md` |
| Error Handling | 新建 `reference/errors.md` |
| SOTA API Pattern、Import Style、Naming | 用法进 `reference/python-usage.md`；根文件只留导入写法与命名各一句 |
| Python Version | 根文件一句 |

### 新规则的措辞（待用户确认）

> **Instruction size.** Keep this file within 32 KiB: Codex reads at most `project_doc_max_bytes` (32 KiB by default) of the AGENTS.md files from the project root to its working directory and drops the rest. `tests/repo/test_docs_hygiene.py` fails above the limit. Mechanism detail belongs in `docs/reference/`, routed from the table below.

> **Read before changing.** Before changing files under a path in this table, read the documents it names; they own that mechanism and its invariants. Update the owning document in the same change as the behavior.

> **Documentation layout.** `docs/README.md` routes each subject to the document that owns it. `docs/reference/` states current behavior; `docs/decisions/` holds one ADR per decision, amended with dated notes rather than rewritten; `docs/design/` holds the backlog, the roadmap and plans in progress; `docs/acceptance/` holds verification records and evidence; `docs/research/` holds notes made before a decision. Before deleting a superseded document, move what still holds into its owning page. Name locations with `~` or a placeholder, never a machine's home directory.

### 检查

- `tests/repo/test_docs_hygiene.py` 新增两项：根 AGENTS.md 不超过 32,768 字节；AGENTS.md、README 与 `docs/reference/` 中的相对链接都能解析。
- 另请一名审查者独立核对：旧 AGENTS.md 的每一条规则都在新的根文件或某份参考文档里有着落，没有丢失，也没有前后矛盾。
- `uv run pytest tests/repo -q`。

## 四、第二步：docs 重组

### 盘点

只读，分批进行，每批产出一张表：每份文档归入现行参考、仍有效的决定理由、未完成的工作、验收记录、已过时五类之一；对前三类，写明仍然成立的内容和去处。按风险分档，不逐篇细读：

| 档 | 范围 | 做法 |
| --- | --- | --- |
| 细读 | `superpowers/specs`（39 个，661 KiB）、`vision/`、`issues/`、`designs/`、顶层的 `architecture-review-v030.md` 与 `memory-pressure-backpressure.md` | 逐篇读，找还没写进现行文档的设计理由与未完成的事项 |
| 读状态 | `plans/`（73 个；2026-10 之前 34 个）、`superpowers/plans` 与 `superpowers/reports` | 读标题、状态与结尾的遗留项；遗留项进待办 |
| 按类处理 | `logs/`、`spikes/` | 抽查是否有被引用或未记录的结论，没有就整批删除 |
| 整批迁移 | `reports/`（含 `evidence/`）、`reviews/` | 挪进 `acceptance/`，不删；只剔除明显的临时文件 |

### 处置规则

- 仍然成立的内容，先挪进它的去处（参考文档、ADR 或待办），再删原文件；git 历史保留原文。
- 未完成的事项一律进 `docs/design/backlog.md`；`issues/` 里仍然有效的说明作为待办条目的细节文档，挪进 `design/`。
- 有链接指向的文档，删除或移动时一并改链接：README、CHANGELOG、各 SDK 的 README、测试与代码。
- 各目录自己的 README（`plans/`、`spikes/`、`reviews/`、`vision/`、`logs/`）由 `docs/README.md` 取代。
- `error_normalization.rs` 里扫描文档查旧 crate 名的测试，只钉一个已删除的名字，随重组删除。

### 实施

1. 按盘点结果移动、合并与删除，改写所有链接；
2. 写 `docs/README.md` 索引；
3. 按第五节的决定统一双语命名；
4. 起草 `CONTEXT.md` 术语表，用户确认后提交，再统一代码、文档与界面里的用词；
5. 文档卫生检查扩展到全部受版本管理的 Markdown：相对链接能解析；任何受版本管理的文件都不出现运行检查的那台机器的主目录路径。

### 检查

- `uv run pytest tests/repo -q`；`cargo test --manifest-path core/Cargo.toml -p c2-core --test error_normalization`。
- 链接检查覆盖全部 Markdown，零失败。
- 抽查五份被删文档，确认其中仍然成立的内容都已有去处。

## 五、待用户决定

1. **参考文档的语言。** Host 建议：面向使用者的指南（配置、Python 用法、Windows 用法）保留中英双语，统一用 README 的命名（`X.md` 英文、`X.zh-CN.md` 中文）；机制参考（IPC 握手、本地端点生命周期、内存策略、运行时生命周期、契约与路由、错误、载荷生命周期、C ABI）只留英文一份；计划、验收与待办这类工作记录用中文。按这个做法，`ipc-handshake.md` 要译成英文，`memory-policy.md` 与 `local-endpoint-lifecycle.md` 的中文版在核对英文版完整之后删除。理由是两份同样的机制文档要一直同步，现在中英两版的篇幅已经对不上。
2. **清退清单。** 方向已同意：`spikes/`、`logs/`、`superpowers/`、已完成的计划、`designs/` 与 v0.3.0 的架构评审。逐份的清单在盘点之后给出，由用户过目。
3. **第三节的三段新规则措辞。**
4. **术语表的词条**，在第二步起草后确认。

## 六、分工与顺序

- 第一步：实施方按第三节移动与合并，并写新的根文件；Host 读完差异，核对新规则措辞与用户确认的一致，并请第二名审查者独立核对"规则没有丢失"。
- 第二步：盘点按第四节分三到四批只读进行；Host 据此定处置表，交用户过目清退清单；然后由实施方执行移动、删除与链接改写；`docs/README.md` 与 `CONTEXT.md` 由 Host 写。
- 分支：两步在 `socu/docs-restructure` 上做，它从 `socu/over-defense-batches` 拉出；向上游的 PR 在代码批次的 PR 之后单独开。
- 顺带项：两步各自顺带做掉同一块的待办。

## 七、退出条件

- 根 AGENTS.md 不超过 32 KiB，测试守住；审查者确认旧 AGENTS.md 的每一条规则都有着落。
- `docs/` 只剩第二节的目录，`docs/README.md` 列出每份现行文档；全部相对链接能解析；没有本机路径。
- 待办里没有遗漏盘点中发现的未完成事项。
- `CONTEXT.md` 经用户确认。
