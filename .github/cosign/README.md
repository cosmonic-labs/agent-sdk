# Signing keys

`cosign-tools.pub` is the public half of the key that signs the agent harness images (`ghcr.io/cosmonic/agents/*`). Verify an image with:

```shell
cosign verify --insecure-ignore-tlog=true --key .github/cosign/cosign-tools.pub ghcr.io/cosmonic/agents/codex@<digest>
```

Signatures are not uploaded to a transparency log, so `--insecure-ignore-tlog=true` is expected. The build provenance attestation is checked separately with `gh attestation verify`.
