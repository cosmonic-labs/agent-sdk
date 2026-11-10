# units-tool

A small typed tool: a WebAssembly component that converts temperatures between Celsius, Fahrenheit and kelvin. It exports an ordinary WIT interface, `example:units/temperature`, with real types and doc comments, and nothing from `cosmonic:agent`. A host turns that interface into a tool definition a model can call, and turns the model's JSON back into typed calls.

It imports no network and no files, so it can reach nothing. [`wit/units.wit`](wit/units.wit) is the whole interface.

## Build it

```shell
cargo build --target wasm32-wasip2 --release
wasm-tools component wit wit --wasm -o units-wit.wasm
```

The first command builds the component. The second builds the WIT package that carries the doc comments, which a host reads for the tool's descriptions.

[Write a typed tool](https://cosmonic.com/docs/agent-sdk/write-a-typed-tool/) walks through this tool line by line and shows how to offer it in Cosmonic Desktop and Cosmonic Control.
