# CDK Go Mnemonic Seed Example

Small Go application that generates a Cashu wallet mnemonic seed using
`github.com/cashubtc/cdk-go`.

## Run

```bash
go get github.com/cashubtc/cdk-go@v0.17.0-rc.3
go mod tidy
go run .
```

The app prints a freshly generated 12-word mnemonic. Keep real wallet mnemonics
private and backed up securely.

The dependency is pinned to `v0.17.0-rc.3` because `go get
github.com/cashubtc/cdk-go` currently resolves to `v0.16.0`, which failed Go
checksum verification while this example was created.
