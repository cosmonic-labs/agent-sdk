# Cosmonic Agent SDK

The Cosmonic Agent SDK is a set of open interfaces for building agents whose every part runs in its own sandbox. The agent harness (the loop that decides what to do next) and each tool are WebAssembly components that start with no files, no network and no credentials, and get only what they are granted. The host makes the model calls, keeps the conversation and holds the keys.

This repository is the canonical source for:

| Path | What it is | Published as |
| --- | --- | --- |
| [`wit/cosmonic-agent`](wit/cosmonic-agent) | `cosmonic:agent`, the WIT package agents, tools and hosts are built against | `ghcr.io/cosmonic-labs/cosmonic/agent` |
| [`components/minimal-agent`](components/minimal-agent) | A small reference agent harness in Rust | `ghcr.io/cosmonic-labs/components/minimal-agent` and `minimal-agent-small` |
| [`components/units-tool`](components/units-tool) | A small typed tool in Rust, the one [Write a typed tool](https://cosmonic.com/docs/agent-sdk/write-a-typed-tool/) builds | Source only |

Everything here is licensed Apache-2.0.

## Get started

- **Build your first agent:** the [Quickstart](https://cosmonic.com/docs/agent-sdk/build-your-first-agent/) builds `minimal-agent`, runs it in Cosmonic Desktop, changes it and gives it a tool.
- **Write a tool:** [Write a typed tool](https://cosmonic.com/docs/agent-sdk/write-a-typed-tool/) builds a tool as an ordinary WIT interface that the host offers to a model.
- **Learn the design:** the [Agent SDK docs](https://cosmonic.com/docs/agent-sdk/) explain how an agent runs, sessions, capabilities and tools.

## Depend on `cosmonic:agent`

Releases are published as a WIT package. Point `wkg` (or `wash wit fetch`) at [`wit/wkg-registries.toml`](wit/wkg-registries.toml) and import the package in your world:

```shell
export WKG_CONFIG_FILE=path/to/wit/wkg-registries.toml
wash wit fetch
```

[`wit/cosmonic-agent/README.md`](wit/cosmonic-agent/README.md) is the design note for the package, and [Interfaces](https://cosmonic.com/docs/agent-sdk/interfaces/) is the reference.

## Where agents run

Agents built on these interfaces run on any host that serves them. [Cosmonic Desktop](https://cosmonic.com/desktop/) runs them on your machine and [Cosmonic Control](https://cosmonic.com/control/) runs them on Kubernetes. Both are built on [wasmCloud](https://wasmcloud.com/), the open source CNCF project.

## Releases and provenance

- `cosmonic:agent` publishes when a new version lands on `main`. A version is never republished with different contents.
- `minimal-agent` and `minimal-agent-small` publish on tags named `minimal-agent-v<version>` and `minimal-agent-small-v<version>`.

Every published artifact carries a GitHub build provenance attestation. Releases up to and including `minimal-agent-v0.3.1` and `cosmonic:agent` 0.4.2 were first built in [cosmonic-labs/awesome-cosmonic](https://github.com/cosmonic-labs/awesome-cosmonic), and this repository carries their history and tags. To verify an artifact:

```shell
gh attestation verify oci://ghcr.io/cosmonic-labs/components/minimal-agent:<version> --owner cosmonic-labs
```
