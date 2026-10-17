// eavs-desktopctl provides the same bounded read-only adapter as the native view.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"

	"github.com/byteowlz/eavs/desktop/control"
)

func run(args []string, out, stderr io.Writer) int {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Fprintln(out, "Usage: eavs-desktopctl status [--eavs-binary PATH] [--json]\nRead-only shared eavs device status; no inference or configuration writes.")
		return 0
	}
	flags := flag.NewFlagSet("status", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	binary := flags.String("eavs-binary", "eavs", "explicit eavs executable")
	machine := flags.Bool("json", false, "stable JSON output")
	if args[0] != "status" || flags.Parse(args[1:]) != nil || flags.NArg() != 0 {
		fmt.Fprintln(stderr, "Invalid command; use eavs-desktopctl --help")
		return 2
	}
	result, err := (control.ExecRunner{Binary: *binary}).Status(context.Background())
	if err != nil {
		if *machine {
			_ = json.NewEncoder(out).Encode(map[string]any{"schema_version": 1, "error": map[string]string{"code": "device_status_unavailable", "message": "Device status unavailable; check eavs configuration and binary compatibility"}})
		} else {
			fmt.Fprintln(stderr, "Device status unavailable; check eavs configuration and binary compatibility")
		}
		return 1
	}
	if *machine {
		if json.NewEncoder(out).Encode(result) != nil {
			return 1
		}
	} else {
		if len(result.Connections) == 0 {
			fmt.Fprintln(out, "No enrolled connections")
		}
		for _, connection := range result.Connections {
			fmt.Fprintf(out, "%s: %s\n", connection.Name, control.StateLabel(connection.State))
		}
	}
	return 0
}
func main() { os.Exit(run(os.Args[1:], os.Stdout, os.Stderr)) }
