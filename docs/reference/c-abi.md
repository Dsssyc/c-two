# C/FFI ABI

This page defines the current `c2-mem-ffi` ABI boundary for native endpoint and memory consumers.

## ABI 6 and consumers

The current `c2-mem-ffi` ABI is 6 and requires the complete endpoint-context symbol group, including verified Windows connection transfer, and allocation-bearing block descriptors. Official Windows Node connections must use that native entry; raw socket, path, or symbol injection is rejected. Transfer an unregistered pipe into Node's own CRT/libuv, retain the process pin through socket closure, and close late results through their existing owner. Do not create a second receive loop or compatibility loader. Node and TypeScript carry allocation IDs as lossless `bigint` values. Endpoint C functions accept the logical address without a protocol selector; consumers validate the ABI before exposing or invoking functions. Historical manifests and receipts retain their actual source ABI.

The [IPC handshake specification](ipc-handshake.md) describes protocol version 14; its change leaves C/FFI ABI 6 unchanged. See the [memory policy](memory-policy.md) for allocation IDs and block release rules, and the [Windows native implementation](windows-native-implementation.md) for the verified connection path.
