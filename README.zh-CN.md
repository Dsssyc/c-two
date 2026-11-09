<p align="center">
  <img src="docs/assets/logo.png" width="150">
</p>

<h1 align="center">C-Two</h1>

<p align="center">
  面向有状态资源的 RPC 运行时。
</p>

<p align="center">
  <a href="https://pypi.org/project/c-two/"><img src="https://img.shields.io/pypi/v/c-two" alt="PyPI" /></a> <a href="https://pypi.org/project/c-two/"><img src="https://img.shields.io/badge/Python-3.10%2B-blue" alt="Python 3.10+" /></a> <img src="https://img.shields.io/badge/free--threading-3.14t-blue" alt="Free-threading" /> <a href="https://github.com/world-in-progress/c-two/actions/workflows/ci.yml"><img src="https://github.com/world-in-progress/c-two/actions/workflows/ci.yml/badge.svg" alt="CI" /></a> <a href="LICENSE"><img src="https://img.shields.io/github/license/world-in-progress/c-two" alt="License" /></a>
</p>

<p align="center">
  <a href="README.md">English</a>
</p>

---

## C-Two 是什么？

C-Two 让其他进程和机器像调用本地对象一样，调用一个有状态对象的方法。它为分布式科学计算而设计：这样的对象往往持有模型、网格或数 GB 的数据，这些数据应当留在原处，由许多客户端来调用。

C-Two 的名字取自 *CRM to Call*（Core Resource Model to Call），意思是：一个有状态的资源，通过它的 CRM 被其他进程和机器调用。

**CRM 契约**是一个用 `@cc.crm(namespace=..., version=...)` 装饰的类，列出可以远程调用的方法，方法体写 `...`。**资源**是实现这些方法、持有状态的普通对象，被调用的就是它。资源以一个**路由名**注册在某个进程里，这个进程称为它的 **Host**。**客户端**调用 `cc.connect(...)`，按路由名或 Host 的地址找到资源，得到一个方法与契约一致的代理；调用代理的方法，就是调用资源。一个进程可以同时承载多个资源，也可以作为客户端调用别的资源。

C-Two 的位置介于 ZeroMQ 这类消息库与 gRPC 这类服务框架之间。消息库只负责把字节送到对方，怎样组织一次调用、结果怎样返回，都由应用自己处理；服务框架则假定任何一个实例都能处理任何请求。C-Two 把每次调用送到持有状态的那个资源对象，并处理由此带来的问题：调用到达哪个对象，并发的调用怎样共用它，以及调用失败时调用方能知道什么。

这份 README 描述的是 main 分支。最新发布的版本是 C-Two 0.10.1 与 c3 0.6.0；服务端流、资源重新注册后客户端按路由名自动跟过去，以及 `cc.endpoint` 与 `cc.observe` 两个模块都在 main 上，会随下一个版本发布。[未发布的更新说明](docs/releases/unreleased.md)列出了 0.10.1 之后的全部改动，[0.10.0 更新说明](docs/releases/0.10.0.md)描述已发布的 API。

## 有状态 RPC 有什么不同

多数 RPC 框架假定服务是无状态的：服务实例本身不持有状态，状态放在外部的共享存储（例如数据库）里，所以任何一个实例都能处理任何请求。C-Two 面对的是另一种情形：状态由资源对象自己维护。状态可以在这个对象的内存里，也可以在它背后的文件或数据库里，怎样维护由资源决定，调用方不必知道；但读写这份状态的调用，都必须送到这个资源对象。下表对比两者的差别。

| | 无状态的 RPC 服务 | C-Two 的资源 |
| --- | --- | --- |
| 调用到达哪里 | 服务的任意一个实例 | 这个路由名下注册的那一个资源对象。资源重新注册后（例如重启之后，或换了一台机器），客户端按路由名跟过去；C-Two 不搬运状态，新注册的资源要接上原来的状态，由服务这个名字的 Host 负责 |
| 并发 | 请求彼此独立，可以同时处理 | 同一个资源对象上，方法默认按写处理，一次只执行一个；标为 `@cc.read` 的方法可以并行 |
| 出错之后 | 调用常常被自动重试或对冲 | 重放一次调用可能再改一次状态，所以可能已经执行过的调用不会被自动重试或重放；错误会说明调用是否可能已经执行，由调用方决定怎么办 |
| 关闭时 | 通常有一段宽限期，期满后取消在途请求 | 不再接收新调用；已经发出的调用照常执行完并交付结果，调用方因此能拿到真实的结果；开着的流在两个条目之间结束 |
| 大数据 | 每条消息整体在内存中编解码，数据较大时通常由应用自行切分 | 单次调用可以传输数 GB 的数组。同一台机器上经共享内存传输，共享内存不够时改走其他路径，内存用量不超过可以设置的上限 |

## 特点

### 本地与远程调用

同一进程内，调用直接到达资源对象，不经过编码。同一台机器上的不同进程之间，调用经 IPC 传输，使用 Unix domain socket 或 Windows Named Pipe。不同机器之间，调用经 relay 以 HTTP 传输；relay 是一个单独运行的进程（`c3 relay`），多个 relay 可以连成 relay mesh。客户端可以直接连接一个 Host 的地址，这不需要 relay；也可以只给路由名，由 relay 找到注册它的 Host。在一个 relay mesh 里，一个路由名同时最多只有一处在线注册，所以按名字找到的总是同一个资源。

### 声明并发

契约里的方法可以声明访问方式。只读的方法标为 `@cc.read`，可以并行执行；其余方法默认按写处理（也可以显式标为 `@cc.write`），在同一个资源对象上一次只执行一个。不论调用来自本进程还是远程，C-Two 都按同样的规则执行，并限制同时执行和排队等待的调用数量。

### 大数据

调用的输入与输出（载荷）可以很大。有类型的科学数据用 FastDB 编码：`@cc.transfer(...)` 指定方法输入和输出的 FastDB 规格，这样的载荷可以在不同语言之间传递。只在 Python 之间调用时，也可以直接传普通 Python 值，C-Two 会用 pickle 编码。同一台机器上，大载荷经共享内存传输，并受可以设置的内存预算约束。`cc.hold()` 把结果保留在内存里，直到你释放它，读取大结果时就不必再复制一次。详见[内存策略](docs/reference/memory-policy.md)与 [Python 载荷用法](docs/reference/python-usage.zh-CN.md)。

### 服务端流

标了 `@cc.stream` 的方法对一次调用返回多个条目。资源一个一个地产出条目，客户端在每个条目到达时读取它，也可以提前停止。流适合逐步推进的模拟、进度报告，以及大到不宜一次返回的结果。流可以在同一进程内使用，可以经 IPC 使用，Python 与 Rust 客户端还可以经 relay 使用；生成的 TypeScript 客户端目前只能经 IPC 读取。下面的[流](#流)一节给出了示例。

### 跨语言契约

CRM 契约可以导出为与语言无关的描述符，c3 据此生成 Python、Rust 与 TypeScript 的绑定。客户端连接和调用时，C-Two 都会核对路由名和契约身份（契约的 namespace、名称、版本，以及 ABI 与签名的哈希）是否与资源一致，契约对不上的客户端调用不到资源。Python 与 Rust SDK 共用同一个 Rust core，路由、传输、内存、错误和生命周期的行为因此一致。

### 工具与生命周期

c3 是 C-Two 的命令行工具，支持 Linux、macOS、Windows x64 与 ARM64。它运行 relay 和 relay mesh，生成绑定，并清理已退出的进程留下的端点。应用可以通过 `cc.observe.connection_events()` 观察自己的 IPC 连接何时建立、何时关闭。详见[生命周期指南](docs/reference/local-endpoint-lifecycle.md)。

## 安装

`pip` 安装的是最新发布的版本 0.10.1：

```bash
uv pip install c-two
# FastDB 载荷（fastdb4py 0.2.4）：
uv pip install 'c-two[fastdb]'
```

Windows PowerShell 使用相同命令。不用 FastDB 载荷时，c-two 不依赖其他 Python 包。

relay 与契约工具在 c3 里，c3 是一个单独的命令行程序。Linux、macOS 的安装命令：

```bash
curl -fsSL https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-installer.sh | sh -s -- --version 0.6.0
```

Windows x64 或 ARM64 使用 [x64 可执行文件](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-x86_64-pc-windows-msvc.exe)、[ARM64 可执行文件](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-aarch64-pc-windows-msvc.exe)或 [PowerShell 安装器](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-installer.ps1)；安装与校验步骤见 [Windows 指南](docs/reference/windows-native-usage.zh-CN.md#安装-python-包和-c3)。

同一台机器上互相调用的程序使用同一套 IPC 协议，所以这台机器上的 Python 与 Rust SDK、c3 和 Node 客户端版本要配套：C-Two 0.10.x 配合 c3 0.6.0。升级时先停止正在运行的服务，再一起升级。细节见 [0.10.0 迁移说明](docs/releases/0.10.0.md)、[握手规范](docs/reference/ipc-handshake.md)和[端点生命周期指南](docs/reference/local-endpoint-lifecycle.md)。

源码构建需要 Rust 1.95 或更新版本；构建与测试见[开发指南](docs/reference/development.md)。

## 快速开始

保存为 `counter.py`：

```python
import c_two as cc

@cc.crm(namespace='demo.counter', version='0.1.0')
class Counter:
    def increment(self, amount: int) -> int: ...

class CounterResource:
    def __init__(self):
        self.value = 0

    def increment(self, amount: int) -> int:
        self.value += amount
        return self.value

cc.register(Counter, CounterResource(), name='counter')
try:
    with cc.connect(Counter, name='counter') as counter:
        print(counter.increment(3))  # 3
finally:
    cc.shutdown()
```

运行 `python counter.py`，会打印 `3`。`@cc.crm` 声明契约 `Counter`；`CounterResource` 实现它，并在多次调用之间保存 `value`。`cc.register` 把一个 `CounterResource` 对象以路由名 `counter` 注册在当前进程里，`cc.connect` 按这个名字返回代理。这里资源和客户端在同一个进程里，所以调用直接到达对象。最后 `cc.shutdown()` 等已经发出的调用执行完，再停止服务。

C-Two 从环境变量 `C2_RELAY_ANCHOR_ADDRESS` 读取 relay 地址；运行这个例子时不要设置它，这样注册和连接都留在本地。

### 从另一个进程调用

要从同一台机器上的另一个进程调用资源，资源进程要用 `cc.serve()` 保持运行，并把 `cc.server_address()` 返回的地址交给客户端：`cc.connect(Counter, name='counter', address=...)`。[IPC 资源端](examples/python/ipc_resource.py)与 [IPC 客户端](examples/python/ipc_client.py)示例演示了两端的写法。

### 流

流方法在资源里写成生成器，每次 `yield` 发出一个条目：

```python
from collections.abc import Iterator

import c_two as cc

@cc.crm(namespace='demo.heat', version='0.1.0')
class HeatModel:
    @cc.stream
    def run(self, steps: int) -> Iterator[float]: ...

class Rod:
    def __init__(self):
        self.temperature = 20.0

    def run(self, steps: int) -> Iterator[float]:
        for _ in range(steps):
            self.temperature += 0.5
            yield self.temperature

cc.register(HeatModel, Rod(), name='rod')
try:
    with cc.connect(HeatModel, name='rod') as model:
        stream = model.run(3)
        for value in stream:
            print(value)  # 依次是 20.5、21.0、21.5
        print(stream.close())  # {'completed': True, 'complete_items': 3, 'error': None}
finally:
    cc.shutdown()
```

`close()` 报告流怎样结束。如果提前离开循环再调用 `close()`，还没有产出的条目就不再产出；[stream.py](examples/python/stream.py) 演示了这种情况。条目的编码、hold 与截止时间见 [Python 指南](docs/reference/python-usage.zh-CN.md#服务端流)。

### 跨机器调用

先用 `c3 relay --bind 0.0.0.0:8300` 启动一个 relay，再在资源进程和客户端进程里都调用 `cc.set_relay_anchor('http://relay-host:8300')`，之后客户端只按名字就能连接。relay 是一个独立运行的进程；客户端如果直接给了 IPC 地址，就不经过 relay，直接连接。[relay 资源端](examples/python/relay_resource.py)与 [relay 客户端](examples/python/relay_client.py)示例演示了两端的写法。

### 一个进程里的多个 Runtime

Runtime 是 C-Two 在进程里的管理对象，负责这个进程的注册、连接、配置和关闭。`cc.register`、`cc.connect` 这些模块函数使用进程的默认 Runtime；`cc.Runtime()` 再创建一个独立的 Runtime，它有自己的配置、注册和连接，由应用自己启动和关闭。见[独立 Runtime 示例](examples/python/independent_runtime.py)及 [Python 指南](docs/reference/python-usage.zh-CN.md#独立的-python-runtime)。

## 示例与文档

| 内容 | 入口 |
| --- | --- |
| 同进程调用 | [local.py](examples/python/local.py) |
| 服务端流 | [stream.py](examples/python/stream.py) |
| 直连 IPC | [资源端](examples/python/ipc_resource.py)、[客户端](examples/python/ipc_client.py) |
| 独立 Python Runtime | [应用自有端点预检](examples/python/independent_runtime.py) |
| Relay | [资源端](examples/python/relay_resource.py)、[客户端](examples/python/relay_client.py)、[mesh](examples/python/relay_mesh/README.md) |
| 配置：代码、环境变量与 `.env` | [配置指南](docs/reference/configuration.zh-CN.md) |
| Python 契约、跨语言载荷与 API 索引 | [Python SDK 指南](docs/reference/python-usage.zh-CN.md) |
| Rust SDK 与 API 迁移 | [Rust SDK 指南](sdk/rust/README.md) |
| Relay 与契约工具 | [c3 CLI 指南](cli/README.md) |
| 连接与端点 API | [生命周期指南](docs/reference/local-endpoint-lifecycle.md) |
| IPC 握手协议 | [握手规范](docs/reference/ipc-handshake.md) |
| Windows | [构建与使用](docs/reference/windows-native-usage.zh-CN.md) |
| 环境变量 | [.env.example](.env.example) |
| 开发 | [构建与测试](docs/reference/development.md)、[CI 与发行](docs/reference/ci-cd.md)、[贡献指南](CONTRIBUTING.md) |

## 许可证

[MIT](LICENSE)。
