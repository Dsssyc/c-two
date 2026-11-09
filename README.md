<p align="center">
  <img src="docs/assets/logo.png" width="150">
</p>

<h1 align="center">C-Two</h1>

<p align="center">
  RPC for stateful resources.
</p>

<p align="center">
  <a href="https://pypi.org/project/c-two/"><img src="https://img.shields.io/pypi/v/c-two" alt="PyPI" /></a> <a href="https://pypi.org/project/c-two/"><img src="https://img.shields.io/badge/Python-3.10%2B-blue" alt="Python 3.10+" /></a> <img src="https://img.shields.io/badge/free--threading-3.14t-blue" alt="Free-threading" /> <a href="https://github.com/world-in-progress/c-two/actions/workflows/ci.yml"><img src="https://github.com/world-in-progress/c-two/actions/workflows/ci.yml/badge.svg" alt="CI" /></a> <a href="LICENSE"><img src="https://img.shields.io/github/license/world-in-progress/c-two" alt="License" /></a>
</p>

<p align="center">
  <a href="README.zh-CN.md">中文版</a>
</p>

---

## What is C-Two?

C-Two lets other processes and machines call the methods of a stateful object as if it were local. It is built for distributed scientific computation, where such an object often holds a model, a grid or gigabytes of data that should stay where it is while many clients call it.

The name C-Two comes from *CRM to Call* (Core Resource Model to Call): a stateful resource is called from other processes and machines through its CRM.

A **CRM contract** is a class decorated with `@cc.crm(namespace=..., version=...)` that lists the methods that can be called remotely; their bodies are `...`. A **resource** is an ordinary object that implements those methods and holds the state; it is the object that calls reach. A resource is registered under a **route name** in a process, which is called its **Host**. A **client** calls `cc.connect(...)`, finds the resource by its route name or by its Host's address, and gets a proxy whose methods match the contract; calling a proxy method calls the resource. One process can host several resources and be a client of others at the same time.

C-Two sits between message libraries such as ZeroMQ and service frameworks such as gRPC. A message library only delivers bytes, and how a call is put together and its result returned is left to the application; a service framework assumes that any instance can handle any request. C-Two carries each call to the one resource object that holds the state, and takes care of what follows from that: which object the call reaches, how concurrent calls share it, and what the caller learns when a call fails.

This README describes the main branch. The latest release is C-Two 0.10.1 with c3 0.6.0; server streams, clients that follow a route name to a resource registered again, and the `cc.endpoint` and `cc.observe` modules are on main and come with the next release. The [unreleased notes](docs/releases/unreleased.md) list every change since 0.10.1, and the [0.10.0 notes](docs/releases/0.10.0.md) describe the released API.

## Why stateful RPC is different

Most RPC frameworks assume a stateless service: a service instance holds no state of its own, the state lives in a shared external store such as a database, and so any instance can handle any request. C-Two addresses the other case: the resource object maintains the state itself. The state may live in the object's memory or in files or databases behind it, which is the resource's choice and not the caller's concern, but every call that reads or changes that state must reach this object. The table compares the two.

| | Stateless RPC services | C-Two resources |
| --- | --- | --- |
| Where a call goes | Any instance of the service | The one resource object registered under the route name. If the resource is registered again, for example after a restart or on another machine, clients follow the route name there; C-Two does not move the state, so the Host that serves the name makes the new registration continue from the old state |
| Concurrency | Requests are independent and can run at the same time | On one resource object, methods are writes by default and run one at a time; methods marked `@cc.read` can run in parallel |
| After a failure | Calls are often retried or hedged automatically | Replaying a call can change the state a second time, so a call that may already have run is never retried or replayed automatically; the error says whether it may have run, and the caller decides what to do |
| Shutdown | Usually a grace period, after which in-flight requests are cancelled | New calls are refused; calls already sent finish and deliver their results, so callers get their real outcome; open streams end between two items |
| Large data | Each message is encoded whole in memory; large data is usually split by the application | One call can carry arrays of several gigabytes. On one machine they travel through shared memory, other paths take over when shared memory runs short, and memory use stays within limits you can set |

## Features

### Local and remote calls

Within one process, a call goes straight to the resource object without encoding. Between processes on one machine, calls use IPC over Unix domain sockets or Windows named pipes. Between machines, calls go through a relay over HTTP; a relay is a separate process, `c3 relay`, and several relays can form a relay mesh. A client can connect to a Host's address directly, which needs no relay, or give only a route name and let the relay find the Host that registered it. In a relay mesh, a route name has at most one live registration at a time, so looking up a name always finds the same resource.

### Declared concurrency

A contract's methods can declare how they access the state. Read-only methods are marked `@cc.read` and can run in parallel; the other methods are writes by default (or marked `@cc.write` explicitly) and run one at a time on one resource object. C-Two applies the same rules whether a call comes from the same process or from elsewhere, and limits how many calls may run or wait at once.

### Large data

A call's input and output, its payloads, can be large. Typed scientific data uses FastDB: `@cc.transfer(...)` names the FastDB specification of a method's input and output, and such payloads can pass between languages. For calls between Python processes only, ordinary Python values also work, encoded with pickle. On one machine, large payloads go through shared memory within memory budgets you can set. `cc.hold()` keeps a result in memory until you release it, so a large result can be read without copying it again. See the [memory policy](docs/reference/memory-policy.md) and [Python payloads](docs/reference/python-usage.md).

### Server streams

A method marked `@cc.stream` returns several items for one call. The resource produces them one at a time, and the client reads each item as it arrives and can stop early. Streams suit simulations that advance step by step, progress reports and results too large to return at once. They work within one process, over IPC and, for Python and Rust clients, through a relay; generated TypeScript clients read them over IPC only for now. The [Streams](#streams) section below shows an example.

### Contracts across languages

A CRM contract can be exported as a language-neutral descriptor, from which c3 generates Python, Rust and TypeScript bindings. When a client connects and calls, C-Two checks that the route name and the contract identity (the contract's namespace, name and version, with its ABI and signature hashes) match the resource's, so a client with a different contract cannot reach it. The Python and Rust SDKs share one Rust core, so routing, transport, memory, errors and lifecycle behave the same in both.

### Tools and lifecycle

c3 is C-Two's command-line tool for Linux, macOS and Windows x64 and ARM64. It runs relays and relay meshes, generates bindings and cleans up the endpoints that exited processes leave behind. An application can watch its IPC connections open and close through `cc.observe.connection_events()`. See the [lifecycle guide](docs/reference/local-endpoint-lifecycle.md).

## Install

`pip` installs the latest release, 0.10.1:

```bash
uv pip install c-two
# With FastDB payloads (fastdb4py 0.2.4):
uv pip install 'c-two[fastdb]'
```

The same commands work in Windows PowerShell. Without FastDB payloads, c-two depends on no other Python package.

The relay and the contract tools are part of c3, a separate command-line program. On Linux or macOS:

```bash
curl -fsSL https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-installer.sh | sh -s -- --version 0.6.0
```

On Windows x64 or ARM64, use the [x64 executable](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-x86_64-pc-windows-msvc.exe), the [ARM64 executable](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-aarch64-pc-windows-msvc.exe) or the [PowerShell installer](https://github.com/world-in-progress/c-two/releases/download/c3-v0.6.0/c3-installer.ps1); the [Windows guide](docs/reference/windows-native-usage.md#install-python-and-c3) explains installation and checksum verification.

Programs that call each other on one machine use the same IPC protocol, so the Python and Rust SDKs, c3 and Node clients on that machine must come from matching releases: C-Two 0.10.x goes with c3 0.6.0. To upgrade, stop the running services first, then upgrade them together. The [0.10.0 migration notes](docs/releases/0.10.0.md), the [handshake spec](docs/reference/ipc-handshake.md) and the [endpoint lifecycle guide](docs/reference/local-endpoint-lifecycle.md) have the details.

Building from source needs Rust 1.95 or newer; see the [development guide](docs/reference/development.md).

## Quickstart

Save as `counter.py`:

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

Run `python counter.py`; it prints `3`. `@cc.crm` declares the contract `Counter`, and `CounterResource` implements it and keeps `value` between calls. `cc.register` registers a `CounterResource` object under the route name `counter` in the current process, and `cc.connect` returns a proxy for that name. Here the resource and the client share one process, so the call goes straight to the object. Finally, `cc.shutdown()` waits for the calls already sent to finish and then stops serving.

C-Two reads a relay address from the environment variable `C2_RELAY_ANCHOR_ADDRESS`; leave it unset when you run this example, so that registration and connection stay local.

### From another process

To call a resource from another process on the same machine, the resource process keeps running with `cc.serve()` and gives the client the address that `cc.server_address()` returns: `cc.connect(Counter, name='counter', address=...)`. The [IPC resource](examples/python/ipc_resource.py) and [IPC client](examples/python/ipc_client.py) examples show both sides.

### Streams

A stream method is written as a generator in the resource, and each `yield` sends one item:

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
            print(value)  # 20.5, then 21.0, then 21.5
        print(stream.close())  # {'completed': True, 'complete_items': 3, 'error': None}
finally:
    cc.shutdown()
```

`close()` reports how the stream ended. If the client leaves the loop early and then calls `close()`, the items not yet produced are never produced; [stream.py](examples/python/stream.py) shows this case. The [Python guide](docs/reference/python-usage.md#server-streams) covers item encoding, holds and deadlines.

### Across machines

First start a relay with `c3 relay --bind 0.0.0.0:8300`, then call `cc.set_relay_anchor('http://relay-host:8300')` in both the resource process and the client process; after that, clients connect by name alone. The relay runs as its own process; a client that is given an IPC address connects directly without going through it. The [relay resource](examples/python/relay_resource.py) and [relay client](examples/python/relay_client.py) examples show both sides.

### Separate Runtimes in one process

A Runtime is the object through which C-Two manages a process's registrations, connections, configuration and shutdown. Module functions such as `cc.register` and `cc.connect` use the process's default Runtime; `cc.Runtime()` creates a separate Runtime with its own configuration, registrations and connections, which the application starts and shuts down itself. See the [independent Runtime example](examples/python/independent_runtime.py) and the [Python guide](docs/reference/python-usage.md#independent-python-runtime-instances).

## Examples and documentation

| Topic | Guide |
| --- | --- |
| Local calls | [local.py](examples/python/local.py) |
| Server streams | [stream.py](examples/python/stream.py) |
| Direct IPC | [resource](examples/python/ipc_resource.py), [client](examples/python/ipc_client.py) |
| Independent Python Runtime | [owned endpoint preflight](examples/python/independent_runtime.py) |
| Relay | [resource](examples/python/relay_resource.py), [client](examples/python/relay_client.py), [mesh](examples/python/relay_mesh/README.md) |
| Configuration: code, environment and `.env` | [Configuration guide](docs/reference/configuration.md) |
| Python contracts, portable payloads and API index | [Python SDK guide](docs/reference/python-usage.md) |
| Rust SDK and API migration | [Rust SDK guide](sdk/rust/README.md) |
| Relay and contract tools | [c3 CLI guide](cli/README.md) |
| Connection and endpoint APIs | [Lifecycle guide](docs/reference/local-endpoint-lifecycle.md) |
| IPC handshake protocol | [Handshake spec](docs/reference/ipc-handshake.md) |
| Windows | [Build and usage](docs/reference/windows-native-usage.md) |
| Environment variables | [.env.example](.env.example) |
| Development | [Build and tests](docs/reference/development.md), [CI and releases](docs/reference/ci-cd.md), [contributing](CONTRIBUTING.md) |

## License

[MIT](LICENSE).
