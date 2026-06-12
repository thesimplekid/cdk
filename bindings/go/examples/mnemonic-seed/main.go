package main

import (
	"fmt"
	"os"
	"strings"

	cdk "github.com/cashubtc/cdk-go/bindings/cdkffi"
)

func main() {
	mnemonic, err := cdk.GenerateMnemonic()
	if err != nil {
		fmt.Fprintf(os.Stderr, "generate mnemonic: %v\n", err)
		os.Exit(1)
	}

	fmt.Println("Mnemonic seed:")
	fmt.Println(mnemonic)
	fmt.Printf("\nWord count: %d\n", len(strings.Fields(mnemonic)))
}
