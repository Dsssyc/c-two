# 跨语言契约与 Codec 架构

> **Status:** Vision（当前方向）
> **Date:** 2026-05-17
> **Scope:** 定义 C-Two 在扩展 Rust、TypeScript、C++ 或其他 SDK 前，如何拆分语言中立 CRM 契约与高性能 payload codec。
> **Audience:** C-Two 核心开发者、未来 SDK 作者、fastdb 贡献者，以及接手 roadmap 工作的 agents。

## 决策摘要

C-Two 应先稳定语言中立的 CRM contract descriptor，再扩展更多语言 SDK；fastdb 应作为第一个高性能 payload codec 试点，而不是成为 CRM 契约系统本身。

长期分层应该是：CRM contract 描述资源的可调用行为，codec 描述 payload 的布局和运行时传输机制。CRM 层负责 method name、namespace、version、read/write access、参数和返回值位置、buffer 语义、route identity、compatibility matching 和 error surface。codec 层负责一个值如何在线上或共享内存中表达，包括 fastdb feature buffer、Arrow/GeoArrow array、WKB geometry bytes、Protobuf message，或显式 custom transfer hooks。

JSON 或另一种确定性的文本格式适合承载 contract descriptor 和 codec manifest，但不应成为高吞吐 GIS payload 的默认数据格式。大型科学计算和地理数据仍应优先使用二进制、列式、零拷贝友好的格式。

## 背景

当前 Python SDK 可以用 pickle 作为默认 transferable fallback。这对 Python-only 工作流很方便，但不能自然推广到 C++、Rust、TypeScript 或浏览器客户端。这些 SDK 不能依赖 Python 运行时 serializer；如果要求用户为每个复杂 CRM 类型手写 `serialize`、`deserialize` 和 `from_buffer`，跨语言 CRM 的作者成本和 review 成本都会迅速上升。

当前 `transferable` 概念也同时承担了太多职责：Python 类型标记、运行时 serializer hook、ABI identity 来源，有时还是零拷贝 buffer adapter。这很灵活，但也意味着公开 CRM contract 不是 Protobuf/gRPC 用户熟悉的那种 plain text、codegen-friendly artifact。

C-Two 应借鉴成熟 RPC 系统的 IDL/codegen 纪律，但不应整体接受它们的 payload 模型。Protobuf 和 gRPC 证明了显式 service contract 与 generated stubs 的价值；但 C-Two 还包含普通 service IDL 覆盖不到的运行时语义：resource identity、same-process direct call、direct IPC、relay routing、SHM lease、hold/view mode、任意 payload wire format，以及 Rust-owned route validation。

## 架构形态

第一类 artifact 应是确定性的 `c-two.contract.v1` descriptor。它可以从当前 SDK 定义导出，但 canonical hash、结构校验和跨语言语义应进入 Rust `c2-contract`，并作为 codegen 与 compatibility check 的权威输入。

示例形态：

```json
{
  "schema": "c-two.contract.v1",
  "crm": {"namespace": "cc.geo", "name": "VectorLayer", "version": "0.2.0"},
  "methods": [
    {
      "name": "query",
      "access": "read",
      "buffer": "hold",
      "params": [
        {"name": "bbox", "type": {"kind": "builtin", "name": "bbox2d_f64"}}
      ],
      "returns": {
        "kind": "codec",
        "codec": "fastdb.feature_set.v1",
        "schema_ref": "cc.geo.FeatureBatch"
      }
    }
  ]
}
```

第二类 artifact 应是 codec descriptor。对 fastdb 来说，可以先从 `fastdb.schema.v1` 开始，描述 feature name、field order、scalar/list/ref kinds、WKB geometry 等 semantic annotations，以及 C-Two 期望使用的 binary codec family。

示例形态：

```json
{
  "schema": "fastdb.feature.v1",
  "name": "cc.geo.FeatureBatch",
  "fields": [
    {"name": "id", "type": "u32"},
    {"name": "geometry", "type": "bytes", "semantic": "wkb"},
    {"name": "value", "type": "f64"}
  ]
}
```

SDK codegen 应消费 contract descriptor 和它引用的 codec descriptors。生成出来的 SDK 应知道 method envelope 和 eligible codec adapters。custom `transferable` hooks 仍然应该保留，但它们应成为显式 opaque-codec escape hatch，而不是 portable CRM contract 的默认作者路径。

## fastdb 的角色

fastdb 是正确的第一个试点，因为它已经具备 C-Two 需要的 serious codec family 雏形：C++ core、Python bindings、TypeScript/WASM bindings、typed feature declarations、codegen、Python-to-TypeScript serializer interop tests、compact binary transport、ref-graph support 和 columnar numeric storage。

fastdb 不应成为 CRM IDL。它描述 data layout 和 transfer behavior，不描述 method access、route identity、relay validation、call metadata、resource lifecycle 或 compatibility policy。把 fastdb 提升为 CRM contract 会让 C-Two 过度绑定到一种数据模型，也会让非 GIS 或非 feature payload 变成二等公民。

合理的第一个集成目标是：Python-hosted CRM 接收或返回 fastdb-described payload，非 Python client 使用 generated code 调用它，双方都通过 Rust-owned validation 拒绝 contract 或 codec mismatch。

## Codec 分层

Tier 0 是 built-in contract types：scalar values、simple lists、tuples、optional values、byte buffers，以及每个 SDK 都可以直接实现的小型 JSON-compatible structs。

Tier 1 是 fastdb-backed feature data：GIS feature batches、typed object graphs、numeric lists，以及 C-Two 希望高性能处理、但不希望每个 SDK 作者都手写 transfer hooks 的 buffer-heavy resource payloads。

Tier 2 是标准外部 schema 与格式：Arrow/GeoArrow 用于 columnar arrays 和 geospatial extension types，GeoParquet 用于持久化 interchange，WKB 用于 geometry bytes，Protobuf/Avro/FlatBuffers 用于下游生态已经明确要求这些格式的场景。

Tier 3 是 opaque custom transferables：显式 `serialize`、`deserialize` 和可选 `from_buffer` 实现，必须声明 ABI identity 和 SDK availability。它们保留 C-Two 的中立性和性能逃生口，但应是 portable contracts 的 opt-in 路径。

## 工业界参考

gRPC 和 Protobuf 是显式 service definition 与 generated client/server code 的参考对象，但它们的默认 message model 不应替代 C-Two 的 resource runtime 与 zero-copy payload path。官方参考：gRPC core concepts https://grpc.io/docs/what-is-grpc/core-concepts/ ，Proto3 guide https://protobuf.dev/programming-guides/proto3/ 。

Apache Arrow 和 GeoArrow 是 language-independent columnar memory 与 geospatial extension types 的参考对象，比 JSON payload 更接近 C-Two 的 GIS 数据面需求。官方参考：Apache Arrow Columnar Format https://arrow.apache.org/docs/format/Columnar.html ，GeoArrow extension types https://geoarrow.org/extension-types.html 。

GeoParquet 是 persisted geospatial table interchange 的参考对象，不一定是 hot-path RPC payload。官方参考：https://geoparquet.org/releases/v1.1.0/ 。

Avro 和 FlatBuffers 对 schema evolution rules 与 generated code boundaries 有参考价值。官方参考：Avro specification https://avro.apache.org/docs/%2B%2Bversion%2B%2B/specification/ ，FlatBuffers evolution https://flatbuffers.dev/evolution/ 。

JSON Schema 适合验证 descriptor documents 和低容量 JSON-compatible structs，但不应被当作高吞吐 GIS call 的默认 data-plane format。官方参考：https://json-schema.org/understanding-json-schema/basics 。

## 实现顺序

1. 从当前 Python descriptor model 出发定义 canonical `c-two.contract.v1`，但从 portable schema 中移除 Python-only naming。
2. 将 canonical descriptor hashing 和 validation authority 放入 Rust `c2-contract`，Python 只作为 exporter 和 facade。
3. 将 pickle 标记为 Python-only codec reference：允许 Python-local convenience，但不能作为 portable cross-language CRM contract。
4. 增加 codec registry 概念，用于识别 built-in、fastdb、standard external 和 opaque custom codec families。
5. 在 fastdb 中增加 `fastdb.schema.v1` export，再把它作为 C-Two 第一个 nontrivial codec descriptor。
6. 在创建大范围 SDK 脚手架前先做一个真实 cross-language slice，例如 Python resource 到 Rust 或 TypeScript client，并覆盖 fastdb payload validation 和 explicit mismatch failures。
7. 只有当 descriptor 与 codec identity 稳定后，C-Two 才应继续推进 semver/range compatibility、auth metadata、async APIs、streaming 和更大的 SDK surface。

## 非目标

不要把 JSON 做成大 payload 数据格式。

不要让 fastdb 成为所有 transferable 的强制选择。

不要把 Python pickle fallback 保留为 portable ABI。

不要在可运行 cross-language slice 之前创建 placeholder SDK directories。

不要把 contract authority 放进单一 SDK。语言中立的 validation、compatibility、relay matching 和 route identity 属于 Rust core 或 FFI。

